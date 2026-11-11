use std::sync::{Arc, Mutex};

use nix::sys::signal::Signal;

use crate::gui::detail_window::DetailWindow;
use crate::gui::dialog_manager::{DialogManager, DialogTarget};
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::gui::process_tab::{ExportFormat, PendingKill, PendingStops, TableAction};
use crate::monitor::{AppState, ProcInfo};
use crate::process_control::ProcessHandle;

/// How long a kill waits, with the process suspended, to be undone.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a process tree gets to exit on SIGTERM before SIGKILL.
const TREE_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// What a process-table action works with: the snapshot it was chosen from
/// and the UI state it may change. Borrowed for the one call, so nothing is
/// owned twice.
pub struct ActionContext<'a, E: Fn(&str)> {
    pub snapshot: &'a [ProcInfo],
    pub state: &'a Arc<Mutex<AppState>>,
    pub pending_kill: &'a mut Option<PendingKill>,
    pub pending_stops: &'a mut PendingStops,
    pub dialog_manager: &'a mut DialogManager,
    pub detail_window: &'a mut DetailWindow,
    pub notify_error: &'a E,
}

impl<E: Fn(&str)> ActionContext<'_, E> {
    /// Carry out `action`. Returns the rule to open in the rule editor when
    /// the action was "add a rule for this process".
    pub fn handle(mut self, action: TableAction) -> Option<crate::rules::Rule> {
        match action {
            TableAction::Kill { pid, name, force } => self.schedule_kill(pid, name, force),
            TableAction::Suspend { pid, name } => self.set_stopped(pid, &name, true),
            TableAction::Resume { pid, name } => self.set_stopped(pid, &name, false),
            TableAction::SetAffinity { pid, name, current } => {
                if let Some(target) = self.dialog_target(pid, &name) {
                    self.dialog_manager.affinity_dialog =
                        Some((target, AffinityDialog::new(&current, &name, pid)));
                }
            }
            TableAction::SetNice { pid, name, current } => {
                if let Some(target) = self.dialog_target(pid, &name) {
                    self.dialog_manager.nice_dialog =
                        Some((target, NiceDialog::new(current, &name)));
                }
            }
            TableAction::SetIonice { pid, name } => {
                if let Some(target) = self.dialog_target(pid, &name) {
                    self.dialog_manager.ionice_dialog = Some((target, IoNiceDialog::new(&name)));
                }
            }
            TableAction::AddRule { name } => return Some(rule_for_process(name)),
            TableAction::ShowDetails { pid } => self.detail_window.set_pid(pid),
            TableAction::KillTree { pid, name } => self.kill_tree(pid, &name),
            TableAction::Export { format } => self.export(format),
            TableAction::None => {}
        }
        None
    }

    fn log(&self, line: String) {
        if let Ok(mut s) = self.state.lock() {
            s.append_log(line);
        }
    }

    /// A handle on the process the snapshot row described: not on whatever
    /// holds that PID by now.
    fn open(&self, pid: u32) -> Result<(ProcessHandle, u64), nix::Error> {
        let row = self
            .snapshot
            .iter()
            .find(|p| p.pid == pid)
            .ok_or(nix::Error::ESRCH)?;
        Ok((ProcessHandle::open(pid, row.start_ticks)?, row.start_ticks))
    }

    fn dialog_target(&self, pid: u32, name: &str) -> Option<DialogTarget> {
        DialogTarget::open(self.snapshot, pid)
            .inspect_err(|e| (self.notify_error)(&format!("Cannot identify {name} ({pid}): {e}")))
            .ok()
    }

    /// Suspend the process now and kill it after a grace period, so the kill
    /// can be undone with nothing lost.
    fn schedule_kill(&mut self, pid: u32, name: String, force: bool) {
        let target = match self.open(pid) {
            Ok((target, _)) => target,
            Err(e) => {
                (self.notify_error)(&format!("Cannot identify {name} ({pid}): {e}"));
                return;
            }
        };
        if let Err(e) = target.signal(Signal::SIGSTOP) {
            (self.notify_error)(&format!(
                "Could not suspend {name} ({pid}); no kill scheduled: {e}"
            ));
            return;
        }
        if let Some(mut old) = self.pending_kill.take() {
            let outcome = old.cancel();
            self.log(format!(
                "{} Previous kill cancelled for {} ({}): {}",
                crate::monitor::KILL_TAG,
                old.name,
                old.pid,
                outcome
                    .map(|_| "resumed".to_owned())
                    .unwrap_or_else(|e| e.to_string())
            ));
        }
        self.log(format!(
            "Suspended {name} ({pid}) — will {} in 5s",
            if force { "force kill" } else { "terminate" }
        ));
        *self.pending_kill = Some(PendingKill {
            pid,
            name,
            force,
            deadline: std::time::Instant::now() + KILL_GRACE,
            target: Some(target),
        });
    }

    fn set_stopped(&mut self, pid: u32, name: &str, stop: bool) {
        let result = self.open(pid).and_then(|(target, start_ticks)| {
            target.signal(if stop {
                Signal::SIGSTOP
            } else {
                Signal::SIGCONT
            })?;
            Ok(start_ticks)
        });
        match result {
            Ok(start_ticks) => {
                self.pending_stops.record(pid, start_ticks, stop);
                self.log(format!(
                    "{} {name} ({pid})",
                    if stop { "Suspended" } else { "Resumed" }
                ));
            }
            Err(e) => {
                (self.notify_error)(&format!("Process action failed for {name} ({pid}): {e}"))
            }
        }
    }

    /// SIGTERM to the process and everything below it, children first, and
    /// SIGKILL after a grace period to whatever is still there.
    fn kill_tree(&mut self, pid: u32, name: &str) {
        let edges: Vec<_> = self.snapshot.iter().map(|p| (p.pid, p.ppid)).collect();
        let tree = crate::utils::process_tree(pid, &edges);
        let mut targets = Vec::new();
        let mut failures = 0;
        for member in tree.iter().rev() {
            let signalled = self.open(*member).and_then(|(target, _)| {
                target.signal(Signal::SIGTERM)?;
                // A suspended process cannot act on SIGTERM.
                let _ = target.signal(Signal::SIGCONT);
                Ok(target)
            });
            match signalled {
                Ok(target) => targets.push(target),
                Err(_) => failures += 1,
            }
        }
        self.log(format!(
            "{} Termination requested for {} processes in tree of {name} ({pid}); {failures} failed",
            crate::monitor::KILL_TAG,
            targets.len()
        ));
        if failures > 0 {
            (self.notify_error)(&format!(
                "Could not terminate {failures} processes in tree of {name}"
            ));
        }
        let state = Arc::clone(self.state);
        std::thread::spawn(move || {
            // Give applications time to flush data before escalation.
            std::thread::sleep(TREE_GRACE);
            let mut forced = 0;
            let mut failed = 0;
            for target in targets {
                match target.signal(Signal::SIGKILL) {
                    Ok(()) => forced += 1,
                    Err(nix::Error::ESRCH) => {}
                    Err(_) => failed += 1,
                }
            }
            if let Ok(mut s) = state.lock() {
                s.append_log(format!(
                    "Process tree cleanup: {forced} force-kill signals sent; {failed} failed"
                ));
            }
        });
    }

    /// Ask where to save, then write the snapshot there, off the GUI thread.
    fn export(&mut self, format: ExportFormat) {
        let (default_name, filter) = export_file(format);
        let content = match format {
            ExportFormat::Csv => crate::utils::export_csv(self.snapshot),
            ExportFormat::Json => crate::utils::export_json(self.snapshot),
        };
        let count = self.snapshot.len();
        let state = Arc::clone(self.state);
        std::thread::spawn(move || {
            let result = (|| -> Result<Option<String>, String> {
                let Some(path) = crate::file_dialog::save(default_name, filter)? else {
                    return Ok(None);
                };
                std::fs::write(&path, content).map_err(|e| format!("Export failed: {e}"))?;
                Ok(Some(format!(
                    "Exported {count} processes to {}",
                    path.display()
                )))
            })();
            if let Ok(mut s) = state.lock() {
                match result {
                    Ok(Some(message)) => s.append_log(message),
                    Ok(None) => {}
                    Err(error) => {
                        s.append_log(error.clone());
                        s.operation_error = Some(error);
                    }
                }
            }
        });
    }
}

/// The rule the editor opens with for "add a rule for this process".
fn rule_for_process(name: String) -> crate::rules::Rule {
    let mut rule = crate::rules::Rule::new_empty();
    rule.name = name.clone();
    rule.pattern = name;
    rule.match_type = crate::config::MatchType::Contains;
    rule
}

/// The suggested file name and the dialog's filter for an export.
fn export_file(format: ExportFormat) -> (&'static str, &'static str) {
    match format {
        ExportFormat::Csv => ("processes.csv", "*.csv"),
        ExportFormat::Json => ("processes.json", "*.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A child to act on, killed and reaped when the test ends.
    struct Child(std::process::Child);

    impl Child {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap(),
            )
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn row(&self) -> ProcInfo {
            ProcInfo {
                pid: self.pid(),
                start_ticks: stat(self.pid()).starttime,
                ..ProcInfo::default()
            }
        }

        /// Wait for the kernel to show the process stopped, or running.
        fn is_stopped(&self, wanted: bool) -> bool {
            (0..400).any(|_| {
                let stopped = stat(self.pid()).state == b'T';
                if stopped != wanted {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                stopped == wanted
            })
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn stat(pid: u32) -> crate::fast_proc::FastStat {
        crate::fast_proc::read_stat(pid, &mut [0; 1024]).unwrap()
    }

    /// The UI state an action can touch, and the errors it reported.
    #[derive(Default)]
    struct Ui {
        state: Arc<Mutex<AppState>>,
        pending_kill: Option<PendingKill>,
        pending_stops: PendingStops,
        dialog_manager: DialogManager,
        detail_window: DetailWindow,
        errors: RefCell<Vec<String>>,
    }

    impl Ui {
        fn handle(
            &mut self,
            snapshot: &[ProcInfo],
            action: TableAction,
        ) -> Option<crate::rules::Rule> {
            let errors = &self.errors;
            ActionContext {
                snapshot,
                state: &self.state,
                pending_kill: &mut self.pending_kill,
                pending_stops: &mut self.pending_stops,
                dialog_manager: &mut self.dialog_manager,
                detail_window: &mut self.detail_window,
                notify_error: &|message: &str| errors.borrow_mut().push(message.to_owned()),
            }
            .handle(action)
        }

        fn log(&self) -> Vec<String> {
            self.state
                .lock()
                .unwrap()
                .log_lines
                .iter()
                .cloned()
                .collect()
        }
    }

    #[test]
    fn suspend_and_resume_act_on_the_process_and_are_remembered() {
        let child = Child::spawn();
        let (pid, snapshot) = (child.pid(), [child.row()]);
        let mut ui = Ui::default();
        let name = || "sleep".to_string();

        ui.handle(&snapshot, TableAction::Suspend { pid, name: name() });
        assert!(child.is_stopped(true));
        // The snapshot still says running; the table shows it suspended.
        assert!(ui.pending_stops.stopped_pids(&snapshot).contains(&pid));

        ui.handle(&snapshot, TableAction::Resume { pid, name: name() });
        assert!(child.is_stopped(false));
        assert!(ui.errors.borrow().is_empty());
        let log = ui.log();
        assert!(log
            .iter()
            .any(|l| l.contains(&format!("Suspended sleep ({pid})"))));
        assert!(log
            .iter()
            .any(|l| l.contains(&format!("Resumed sleep ({pid})"))));
    }

    /// A row whose PID now belongs to another process, or to none, is
    /// refused: the signal would land on a process the user never chose.
    #[test]
    fn an_action_on_a_process_that_is_not_the_one_shown_is_refused() {
        let child = Child::spawn();
        let pid = child.pid();
        let mut ui = Ui::default();
        let stale = [ProcInfo {
            start_ticks: child.row().start_ticks + 1,
            ..child.row()
        }];
        let name = || "sleep".to_string();

        ui.handle(&stale, TableAction::Suspend { pid, name: name() });
        ui.handle(&[], TableAction::Suspend { pid, name: name() });
        ui.handle(
            &stale,
            TableAction::Kill {
                pid,
                name: name(),
                force: true,
            },
        );
        ui.handle(
            &stale,
            TableAction::SetNice {
                pid,
                name: name(),
                current: 0,
            },
        );

        assert_eq!(ui.errors.borrow().len(), 4, "{:?}", ui.errors);
        assert!(ui.pending_kill.is_none());
        assert!(ui.dialog_manager.nice_dialog.is_none());
        assert!(ui.pending_stops.stopped_pids(&stale).is_empty());
        assert_ne!(stat(pid).state, b'T', "the child was left alone");
    }

    /// A kill suspends at once and waits; cancelling it resumes the process
    /// with nothing lost, and scheduling another kill cancels the first.
    #[test]
    fn a_kill_is_scheduled_with_the_process_suspended_and_can_be_cancelled() {
        let (first, second) = (Child::spawn(), Child::spawn());
        let snapshot = [first.row(), second.row()];
        let mut ui = Ui::default();
        let kill = |pid| TableAction::Kill {
            pid,
            name: "sleep".into(),
            force: false,
        };

        ui.handle(&snapshot, kill(first.pid()));
        assert!(first.is_stopped(true));
        let pending = ui.pending_kill.as_ref().unwrap();
        assert_eq!((pending.pid, pending.force), (first.pid(), false));
        assert!(pending.deadline > std::time::Instant::now());

        ui.handle(&snapshot, kill(second.pid()));
        assert!(second.is_stopped(true));
        assert!(first.is_stopped(false), "the first kill was cancelled");
        assert_eq!(ui.pending_kill.as_ref().unwrap().pid, second.pid());
        assert!(ui
            .log()
            .iter()
            .any(|l| l.contains("Previous kill cancelled")));

        ui.pending_kill = None;
        assert!(second.is_stopped(false), "dropping a pending kill resumes");
        assert!(ui.errors.borrow().is_empty());
    }

    #[test]
    fn dialogs_open_for_the_process_and_details_follow_it() {
        let child = Child::spawn();
        let (pid, snapshot) = (child.pid(), [child.row()]);
        let mut ui = Ui::default();
        let name = || "sleep".to_string();

        ui.handle(
            &snapshot,
            TableAction::SetNice {
                pid,
                name: name(),
                current: 3,
            },
        );
        ui.handle(&snapshot, TableAction::SetIonice { pid, name: name() });
        ui.handle(
            &snapshot,
            TableAction::SetAffinity {
                pid,
                name: name(),
                current: "0".into(),
            },
        );
        assert!(ui.dialog_manager.nice_dialog.is_some());
        assert!(ui.dialog_manager.ionice_dialog.is_some());
        assert!(ui.dialog_manager.affinity_dialog.is_some());
        assert!(ui.errors.borrow().is_empty());
        assert!(ui.handle(&snapshot, TableAction::None).is_none());
    }

    #[test]
    fn add_rule_opens_the_editor_on_a_contains_rule_for_the_name() {
        let mut ui = Ui::default();
        let rule = ui
            .handle(
                &[],
                TableAction::AddRule {
                    name: "firefox".into(),
                },
            )
            .unwrap();
        assert_eq!(
            (rule.name.as_str(), rule.pattern.as_str()),
            ("firefox", "firefox")
        );
        assert_eq!(rule.match_type, crate::config::MatchType::Contains);
    }

    #[test]
    fn an_export_is_named_for_its_format() {
        assert_eq!(export_file(ExportFormat::Csv), ("processes.csv", "*.csv"));
        assert_eq!(
            export_file(ExportFormat::Json),
            ("processes.json", "*.json")
        );
    }
}
