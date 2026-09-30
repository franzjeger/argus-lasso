use std::sync::{Arc, Mutex};

use crate::gui::detail_window::DetailWindow;
use crate::gui::dialog_manager::{DialogManager, DialogTarget};
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::gui::process_tab::TableAction;
use crate::gui::process_tab::{PendingKill, PendingStops};
use crate::monitor::{AppState, ProcInfo};

pub struct ActionHandler;

impl ActionHandler {
    // One dispatch point borrows the existing UI state without duplicating ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn handle(
        action: TableAction,
        snapshot: &[ProcInfo],
        state: &Arc<Mutex<AppState>>,
        pending_kill: &mut Option<PendingKill>,
        pending_stops: &mut PendingStops,
        dialog_manager: &mut DialogManager,
        detail_window: &mut DetailWindow,
        notify_error: &impl Fn(&str),
        trigger_rules_tab: &mut Option<crate::rules::Rule>,
    ) {
        let suspend = matches!(action, TableAction::Suspend { .. });
        match action {
            TableAction::Kill { pid, name, force } => {
                use nix::sys::signal::Signal;
                let target = snapshot
                    .iter()
                    .find(|p| p.pid == pid)
                    .ok_or(nix::Error::ESRCH)
                    .and_then(|p| crate::process_control::ProcessHandle::open(pid, p.start_ticks));
                let target = match target {
                    Ok(target) => target,
                    Err(e) => {
                        notify_error(&format!("Cannot identify {name} ({pid}): {e}"));
                        return;
                    }
                };
                if let Err(e) = target.signal(Signal::SIGSTOP) {
                    notify_error(&format!(
                        "Could not suspend {name} ({pid}); no kill scheduled: {e}"
                    ));
                    return;
                }
                if let Some(mut old) = pending_kill.take() {
                    let outcome = old.cancel();
                    if let Ok(mut s) = state.lock() {
                        s.append_log(format!(
                            "{} Previous kill cancelled for {} ({}): {}",
                            crate::monitor::KILL_TAG,
                            old.name,
                            old.pid,
                            outcome
                                .map(|_| "resumed".to_owned())
                                .unwrap_or_else(|e| e.to_string())
                        ));
                    }
                }
                *pending_kill = Some(PendingKill {
                    pid,
                    name: name.clone(),
                    force,
                    deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
                    target: Some(target),
                });
                if let Ok(mut s) = state.lock() {
                    s.append_log(format!(
                        "Suspended {name} ({pid}) — will {} in 5s",
                        if force { "force kill" } else { "terminate" }
                    ));
                }
            }
            TableAction::Suspend { pid, name } | TableAction::Resume { pid, name } => {
                let result = snapshot
                    .iter()
                    .find(|p| p.pid == pid)
                    .ok_or(nix::Error::ESRCH)
                    .and_then(|p| {
                        let target =
                            crate::process_control::ProcessHandle::open(pid, p.start_ticks)?;
                        target.signal(if suspend {
                            nix::sys::signal::Signal::SIGSTOP
                        } else {
                            nix::sys::signal::Signal::SIGCONT
                        })?;
                        Ok(p.start_ticks)
                    });
                match result {
                    Ok(start_ticks) => {
                        pending_stops.record(pid, start_ticks, suspend);
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!(
                                "{} {name} ({pid})",
                                if suspend { "Suspended" } else { "Resumed" }
                            ));
                        }
                    }
                    Err(e) => {
                        notify_error(&format!("Process action failed for {name} ({pid}): {e}"))
                    }
                }
            }
            TableAction::SetAffinity { pid, name, current } => {
                match DialogTarget::open(snapshot, pid) {
                    Ok(target) => {
                        dialog_manager.affinity_dialog =
                            Some((target, AffinityDialog::new(&current, &name)))
                    }
                    Err(e) => notify_error(&format!("Cannot identify {name} ({pid}): {e}")),
                }
            }
            TableAction::SetNice { pid, name, current } => {
                match DialogTarget::open(snapshot, pid) {
                    Ok(target) => {
                        dialog_manager.nice_dialog = Some((target, NiceDialog::new(current, &name)))
                    }
                    Err(e) => notify_error(&format!("Cannot identify {name} ({pid}): {e}")),
                }
            }
            TableAction::SetIonice { pid, name } => match DialogTarget::open(snapshot, pid) {
                Ok(target) => {
                    dialog_manager.ionice_dialog = Some((target, IoNiceDialog::new(&name)))
                }
                Err(e) => notify_error(&format!("Cannot identify {name} ({pid}): {e}")),
            },
            TableAction::AddRule { name } => {
                let mut rule = crate::rules::Rule::new_empty();
                rule.name = name.clone();
                rule.pattern = name;
                rule.match_type = crate::config::MatchType::Contains;
                *trigger_rules_tab = Some(rule);
            }
            TableAction::ShowDetails { pid } => {
                detail_window.set_pid(pid);
            }
            TableAction::KillTree { pid, name } => {
                use nix::sys::signal::Signal;
                let edges: Vec<_> = snapshot.iter().map(|p| (p.pid, p.ppid)).collect();
                let tree = crate::utils::process_tree(pid, &edges);
                let mut targets = Vec::new();
                let mut failures = 0;
                for pid in tree.iter().rev() {
                    let handle = snapshot
                        .iter()
                        .find(|p| p.pid == *pid)
                        .ok_or(nix::Error::ESRCH)
                        .and_then(|p| {
                            crate::process_control::ProcessHandle::open(*pid, p.start_ticks)
                        });
                    match handle {
                        Ok(target) => match target.signal(Signal::SIGTERM) {
                            Ok(()) => {
                                let _ = target.signal(Signal::SIGCONT);
                                targets.push(target);
                            }
                            Err(_) => failures += 1,
                        },
                        Err(_) => failures += 1,
                    }
                }
                if let Ok(mut s) = state.lock() {
                    s.append_log(format!(
                        "{} Termination requested for {} processes in tree of {name} ({pid}); {failures} failed",
                        crate::monitor::KILL_TAG,
                        targets.len()
                    ));
                }
                if failures > 0 {
                    notify_error(&format!(
                        "Could not terminate {failures} processes in tree of {name}"
                    ));
                }
                let state = Arc::clone(state);
                std::thread::spawn(move || {
                    // Give applications time to flush data before escalation.
                    std::thread::sleep(std::time::Duration::from_secs(3));
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
                        s.append_log(format!("Process tree cleanup: {forced} force-kill signals sent; {failed} failed"));
                    }
                });
            }
            TableAction::Export { format } => {
                let ext = match format {
                    crate::gui::process_tab::ExportFormat::Csv => "csv",
                    crate::gui::process_tab::ExportFormat::Json => "json",
                };
                let default_name = format!("processes.{}", ext);
                let filter = match format {
                    crate::gui::process_tab::ExportFormat::Csv => "*.csv",
                    crate::gui::process_tab::ExportFormat::Json => "*.json",
                };
                let content = match format {
                    crate::gui::process_tab::ExportFormat::Csv => {
                        crate::utils::export_csv(snapshot)
                    }
                    crate::gui::process_tab::ExportFormat::Json => {
                        crate::utils::export_json(snapshot)
                    }
                };
                let count = snapshot.len();
                let state = Arc::clone(state);
                std::thread::spawn(move || {
                    let result = (|| -> Result<Option<String>, String> {
                        let Some(path) = crate::file_dialog::save(&default_name, filter)? else {
                            return Ok(None);
                        };
                        std::fs::write(&path, content)
                            .map_err(|e| format!("Export failed: {e}"))?;
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

            TableAction::None => {}
        }
    }
}
