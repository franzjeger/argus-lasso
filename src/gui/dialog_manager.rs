use crossbeam_channel::Sender;
use eframe::egui;
use egui::Context;
use std::sync::{Arc, Mutex};

use crate::config;
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::monitor::{AppState, DaemonCmd, ProcInfo};
use crate::process_control::ProcessHandle;
use crate::rules::RuleEngine;
use crate::utils;

pub struct RuleOffer {
    pub proc_name: String,
    pub affinity: Option<String>,
    pub nice: Option<i32>,
    pub ionice: Option<(i32, i32)>,
}

impl RuleOffer {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(a) = &self.affinity {
            parts.push(format!("affinity {a}"));
        }
        if let Some(n) = self.nice {
            parts.push(format!("nice {n}"));
        }
        if let Some((c, l)) = self.ionice {
            parts.push(format!("ionice {c}/{l}"));
        }
        parts.join(", ")
    }
}

/// How long rule enforcement and ProBalance leave a process alone after a
/// manual change, so the change is not undone on their next pass.
const MANUAL_OVERRIDE_SECS: f64 = 30.0;

/// The process a dialog was opened for, held through a pidfd: the change is
/// never applied to a process that reused its PID while the dialog was open.
pub struct DialogTarget {
    pid: u32,
    handle: ProcessHandle,
}

impl DialogTarget {
    /// Identify `pid` as it appears in the snapshot the user acted on.
    pub fn open(snapshot: &[ProcInfo], pid: u32) -> Result<Self, nix::Error> {
        let start_ticks = snapshot
            .iter()
            .find(|p| p.pid == pid)
            .ok_or(nix::Error::ESRCH)?
            .start_ticks;
        Ok(Self {
            pid,
            handle: ProcessHandle::track(pid, start_ticks)?,
        })
    }
}

/// Apply a change the user made in a dialog. There are no pidfd forms of
/// setpriority, ioprio_set or sched_setaffinity, so the process is confirmed
/// alive immediately before — a live process's PID cannot have been reused.
/// Returns whether the change was applied.
fn apply_manual_change(
    target: &DialogTarget,
    name: &str,
    change: &str,
    apply: impl FnOnce(u32) -> bool,
    state: &Arc<Mutex<AppState>>,
    cmd_tx: &Sender<DaemonCmd>,
    notify_error: &impl Fn(&str),
) -> bool {
    let pid = target.pid;
    if target.handle.has_exited() {
        notify_error(&format!(
            "{name} (PID {pid}) exited before {change} was applied"
        ));
        return false;
    }
    if !apply(pid) {
        notify_error(&format!(
            "Failed to set {change} on {name} (PID {pid}) — needs root?"
        ));
        return false;
    }
    let _ = cmd_tx.send(DaemonCmd::SetManualOverride {
        pid,
        duration_secs: MANUAL_OVERRIDE_SECS,
    });
    if let Ok(mut s) = state.lock() {
        s.append_log(format!("[Manual] {change} → PID {pid}"));
    }
    true
}

#[derive(Default)]
pub struct DialogManager {
    pub affinity_dialog: Option<(DialogTarget, AffinityDialog)>,
    pub nice_dialog: Option<(DialogTarget, NiceDialog)>,
    pub ionice_dialog: Option<(DialogTarget, IoNiceDialog)>,
    pub rule_offer: Option<RuleOffer>,
}

impl DialogManager {
    pub fn offer_rule(
        &mut self,
        proc_name: String,
        affinity: Option<String>,
        nice: Option<i32>,
        ionice: Option<(i32, i32)>,
    ) {
        match &mut self.rule_offer {
            Some(offer) if offer.proc_name == proc_name => {
                if affinity.is_some() {
                    offer.affinity = affinity;
                }
                if nice.is_some() {
                    offer.nice = nice;
                }
                if ionice.is_some() {
                    offer.ionice = ionice;
                }
            }
            _ => {
                self.rule_offer = Some(RuleOffer {
                    proc_name,
                    affinity,
                    nice,
                    ionice,
                });
            }
        }
    }

    pub fn poll_dialogs(
        &mut self,
        ctx: &Context,
        opacity: f32,
        state: &Arc<Mutex<AppState>>,
        cmd_tx: &Sender<DaemonCmd>,
        rule_engine: &Arc<Mutex<RuleEngine>>,
        notify_error: &impl Fn(&str),
    ) {
        if let Some(cpulist) = self
            .affinity_dialog
            .as_mut()
            .and_then(|(_, dlg)| dlg.show(ctx, opacity))
        {
            if let Some((target, dlg)) = self.affinity_dialog.take() {
                // An empty selection is the dialog's cancel.
                if !cpulist.is_empty()
                    && apply_manual_change(
                        &target,
                        &dlg.title,
                        &format!("affinity={cpulist}"),
                        |pid| utils::set_affinity(pid, &cpulist),
                        state,
                        cmd_tx,
                        notify_error,
                    )
                {
                    self.offer_rule(dlg.title, Some(cpulist), None, None);
                }
            }
        }

        if let Some(accepted) = self
            .nice_dialog
            .as_mut()
            .and_then(|(_, dlg)| dlg.show(ctx, opacity))
        {
            if let (Some(nice), Some((target, dlg))) = (accepted, self.nice_dialog.take()) {
                if apply_manual_change(
                    &target,
                    &dlg.title,
                    &format!("nice={nice}"),
                    |pid| utils::set_nice(pid, nice),
                    state,
                    cmd_tx,
                    notify_error,
                ) {
                    self.offer_rule(dlg.title, None, Some(nice), None);
                }
            }
            self.nice_dialog = None;
        }

        if let Some(accepted) = self
            .ionice_dialog
            .as_mut()
            .and_then(|(_, dlg)| dlg.show(ctx, opacity))
        {
            if let (Some((class, level)), Some((target, dlg))) =
                (accepted, self.ionice_dialog.take())
            {
                if apply_manual_change(
                    &target,
                    &dlg.title,
                    &format!("ionice class={class} level={level}"),
                    |pid| utils::set_ionice(pid, class, Some(level)),
                    state,
                    cmd_tx,
                    notify_error,
                ) {
                    self.offer_rule(dlg.title, None, None, Some((class, level)));
                }
            }
            self.ionice_dialog = None;
        }

        // Rule offer
        if let Some(offer) = &self.rule_offer {
            let mut create = false;
            let mut dismiss = false;
            egui::Window::new("Remember settings?")
                .id(egui::Id::new("rule_offer_window"))
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -40.0))
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Keep {} for '{}' with a rule?\nThe setting will be re-applied every time the process starts.",
                        offer.summary(), offer.proc_name
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Create rule").clicked() { create = true; }
                        if ui.button("No thanks").clicked() { dismiss = true; }
                    });
                });

            if create {
                let offer = self.rule_offer.take().unwrap();
                let mut rule = crate::rules::Rule::new_empty();
                rule.name = offer.proc_name.clone();
                rule.pattern = offer.proc_name.clone();
                rule.match_type = "exact".into();
                rule.affinity = offer.affinity;
                rule.nice = offer.nice;
                rule.ionice_class = offer.ionice.map(|(c, _)| c);
                rule.ionice_level = offer.ionice.map(|(_, l)| l);

                let rules_cfg = if let Ok(mut re) = rule_engine.lock() {
                    re.add_rule(rule);
                    re.to_config_list()
                } else {
                    Vec::new()
                };
                let cfg = if let Ok(mut s) = state.lock() {
                    s.config.rules = rules_cfg;
                    s.append_log(format!(
                        "[Rule] Created rule for '{}' from manual change",
                        offer.proc_name
                    ));
                    Some(s.config.clone())
                } else {
                    None
                };

                if let Some(cfg) = cfg {
                    if let Err(e) = config::save(&cfg) {
                        log::warn!("Failed to save rule to disk: {e}");
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!(
                                "[Rule] Failed to save '{}' to disk: {e}",
                                offer.proc_name
                            ));
                        }
                    }
                }
                let _ = cmd_tx.send(DaemonCmd::ReapplyDefaults);
            } else if dismiss {
                self.rule_offer = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_for(child: &std::process::Child) -> DialogTarget {
        let pid = child.id();
        let start_ticks = crate::fast_proc::read_stat(pid, &mut [0; 1024])
            .unwrap()
            .starttime;
        DialogTarget {
            pid,
            handle: ProcessHandle::track(pid, start_ticks).unwrap(),
        }
    }

    /// Nice and ionice changes used to skip the override, so the next rule
    /// pass reverted them. Every applied change must send one.
    #[test]
    fn an_applied_change_is_protected_from_enforcement() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let target = target_for(&child);
        let state = Arc::new(Mutex::new(AppState::default()));
        let (tx, rx) = crossbeam_channel::unbounded();

        let applied =
            apply_manual_change(&target, "sleep", "nice=5", |_| true, &state, &tx, &|_| {});

        assert!(applied);
        assert!(matches!(
            rx.try_recv(),
            Ok(DaemonCmd::SetManualOverride { pid, .. }) if pid == child.id()
        ));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_change_is_never_applied_after_the_process_exits() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let target = target_for(&child);
        child.kill().unwrap();
        child.wait().unwrap();
        let state = Arc::new(Mutex::new(AppState::default()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let called = std::cell::Cell::new(false);

        let applied = apply_manual_change(
            &target,
            "sleep",
            "nice=5",
            |_| {
                called.set(true);
                true
            },
            &state,
            &tx,
            &|_| {},
        );

        assert!(!applied);
        assert!(
            !called.get(),
            "the PID may already belong to another process"
        );
        assert!(rx.try_recv().is_err());
    }
}
