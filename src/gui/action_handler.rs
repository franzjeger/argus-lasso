use crossbeam_channel::Sender;
use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::gui::detail_window::DetailWindow;
use crate::gui::dialog_manager::DialogManager;
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::gui::process_tab::PendingKill;
use crate::gui::process_tab::TableAction;
use crate::monitor::{AppState, DaemonCmd, ProcInfo};
use crate::rules::RuleEngine;

pub struct ActionHandler;

impl ActionHandler {
    pub fn deliver_kill(pid: u32, force: bool) -> Result<(), nix::Error> {
        use nix::sys::signal::{self, Signal};
        use nix::unistd::Pid;
        let sig = if force {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        let result = signal::kill(Pid::from_raw(pid as i32), sig);
        let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGCONT);
        result
    }

    pub fn handle(
        action: TableAction,
        snapshot: &[ProcInfo],
        state: &Arc<Mutex<AppState>>,
        pending_kill: &mut Option<PendingKill>,
        dialog_manager: &mut DialogManager,
        detail_window: &mut DetailWindow,
        notify_error: &impl Fn(&str),
        trigger_rules_tab: &mut Option<crate::rules::Rule>,
    ) {
        match action {
            TableAction::Kill { pid, name, force } => {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                if let Some(old) = pending_kill.take() {
                    let msg = match Self::deliver_kill(old.pid, old.force) {
                        Ok(_) => format!(
                            "{}illed {} ({}) — superseded by new kill",
                            if old.force { "Force k" } else { "K" },
                            old.name,
                            old.pid
                        ),
                        Err(e) => format!("Kill failed for {} ({}): {e}", old.name, old.pid),
                    };
                    if let Ok(mut s) = state.lock() {
                        s.append_log(msg);
                    }
                }
                match signal::kill(Pid::from_raw(pid as i32), Signal::SIGSTOP) {
                    Ok(()) => {
                        *pending_kill = Some(PendingKill {
                            pid,
                            name: name.clone(),
                            force,
                            deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
                        });
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!(
                                "Suspended {} ({}) — will {} in 5s",
                                name,
                                pid,
                                if force { "force kill" } else { "kill" }
                            ));
                        }
                    }
                    Err(e) => {
                        let outcome = match Self::deliver_kill(pid, force) {
                            Ok(()) => format!(
                                "{}illed it immediately.",
                                if force { "Force k" } else { "K" }
                            ),
                            Err(ke) => format!("kill also failed: {ke}"),
                        };
                        notify_error(&format!(
                            "Suspend failed for {} ({}): {e}. {outcome}",
                            name, pid
                        ));
                    }
                }
            }
            TableAction::Suspend { pid, name } => {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                match signal::kill(Pid::from_raw(pid as i32), Signal::SIGSTOP) {
                    Ok(()) => {
                        if let Ok(mut s) = state.lock() {
                            s.suspended_pids.insert(pid);
                            s.append_log(format!("Suspended {} ({})", name, pid));
                        }
                    }
                    Err(e) => notify_error(&format!("Suspend failed for {} ({}): {e}", name, pid)),
                }
            }
            TableAction::Resume { pid, name } => {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                match signal::kill(Pid::from_raw(pid as i32), Signal::SIGCONT) {
                    Ok(()) => {
                        if let Ok(mut s) = state.lock() {
                            s.suspended_pids.remove(&pid);
                            s.append_log(format!("Resumed {} ({})", name, pid));
                        }
                    }
                    Err(e) => notify_error(&format!("Resume failed for {} ({}): {e}", name, pid)),
                }
            }
            TableAction::SetAffinity { pid, name, current } => {
                dialog_manager.affinity_dialog = Some((pid, AffinityDialog::new(&current, &name)));
            }
            TableAction::SetNice { pid, name, current } => {
                dialog_manager.nice_dialog = Some((pid, NiceDialog::new(current, &name)));
            }
            TableAction::SetIonice { pid, name } => {
                dialog_manager.ionice_dialog = Some((pid, IoNiceDialog::new(&name)));
            }
            TableAction::AddRule { name } => {
                let mut rule = crate::rules::Rule::new_empty();
                rule.name = name.clone();
                rule.pattern = name;
                rule.match_type = "contains".into();
                *trigger_rules_tab = Some(rule);
            }
            TableAction::ShowDetails { pid } => {
                detail_window.set_pid(pid);
            }
            TableAction::KillTree { pid, name } => {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                let edges: Vec<(u32, u32)> = snapshot.iter().map(|p| (p.pid, p.ppid)).collect();
                let tree = crate::utils::process_tree(pid, &edges);
                if tree.is_empty() {
                    notify_error(&format!("No process tree found for {} ({})", name, pid));
                    return;
                }
                let count = tree.len();
                let mut killed = 0u32;
                let mut failed = 0u32;
                let mut survivors: Vec<u32> = Vec::new();
                for &t in tree.iter().rev() {
                    match signal::kill(Pid::from_raw(t as i32), Signal::SIGTERM) {
                        Ok(()) => {
                            killed += 1;
                            let _ = signal::kill(Pid::from_raw(t as i32), Signal::SIGCONT);
                        }
                        Err(_) => {
                            failed += 1;
                        }
                    }
                    survivors.push(t);
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
                for &t in &survivors {
                    if let Err(nix::Error::ESRCH) =
                        signal::kill(Pid::from_raw(t as i32), Signal::SIGKILL)
                    {
                        continue;
                    }
                    let _ = signal::kill(Pid::from_raw(t as i32), Signal::SIGCONT);
                }
                let msg = format!(
                    "Killed {} of {} processes in tree of {} ({})",
                    killed, count, name, pid
                );
                if let Ok(mut s) = state.lock() {
                    s.append_log(msg.clone());
                }
                if failed > 0 {
                    notify_error(&format!("{} — {} failed (permissions?)", msg, failed));
                }
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
                let Some(path) = crate::file_dialog::save(&default_name, filter) else {
                    return;
                };
                let content = match format {
                    crate::gui::process_tab::ExportFormat::Csv => {
                        crate::utils::export_csv(snapshot)
                    }
                    crate::gui::process_tab::ExportFormat::Json => {
                        crate::utils::export_json(snapshot)
                    }
                };
                match std::fs::write(&path, content) {
                    Ok(_) => {
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!(
                                "Exported {} processes to {}",
                                snapshot.len(),
                                path.display()
                            ));
                        }
                    }
                    Err(e) => notify_error(&format!("Export failed: {e}")),
                }
            }
            TableAction::None => {}
        }
    }
}
