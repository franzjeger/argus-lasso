use crossbeam_channel::Sender;
use eframe::egui;
use egui::Context;
use std::sync::{Arc, Mutex};

use crate::config;
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::monitor::{AppState, DaemonCmd};
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

#[derive(Default)]
pub struct DialogManager {
    pub affinity_dialog: Option<(u32, AffinityDialog)>,
    pub nice_dialog: Option<(u32, NiceDialog)>,
    pub ionice_dialog: Option<(u32, IoNiceDialog)>,
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
        // Affinity dialog
        if let Some((pid, ref mut dlg)) = self.affinity_dialog {
            let proc_name = dlg.title.clone();
            if let Some(result) = dlg.show(ctx, opacity) {
                let cpulist = result.as_str();
                if cpulist.is_empty() {
                    self.affinity_dialog = None;
                } else if utils::set_affinity(pid, cpulist) {
                    let _ = cmd_tx.send(DaemonCmd::SetManualOverride {
                        pid,
                        duration_secs: 30.0,
                    });
                    if let Ok(mut s) = state.lock() {
                        s.append_log(format!("[Manual] affinity={cpulist} → PID {pid}"));
                    }
                    self.offer_rule(proc_name, Some(result.clone()), None, None);
                } else {
                    notify_error(&format!(
                        "Failed to set affinity '{cpulist}' on {} (PID {pid}) — needs root?",
                        proc_name
                    ));
                }
                self.affinity_dialog = None;
            }
        }

        // Nice dialog
        if let Some((pid, ref mut dlg)) = self.nice_dialog {
            let proc_name = dlg.title.clone();
            if let Some(result) = dlg.show(ctx, opacity) {
                if let Some(nice) = result {
                    if utils::set_nice(pid, nice) {
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!("[Manual] nice={nice} → PID {pid}"));
                        }
                        self.offer_rule(proc_name, None, Some(nice), None);
                    } else {
                        notify_error(&format!(
                            "Failed to set nice {nice} on {} (PID {pid}) — needs root?",
                            proc_name
                        ));
                    }
                }
                self.nice_dialog = None;
            }
        }

        // IoNice dialog
        if let Some((pid, ref mut dlg)) = self.ionice_dialog {
            let proc_name = dlg.title.clone();
            if let Some(result) = dlg.show(ctx, opacity) {
                if let Some((class, level)) = result {
                    if utils::set_ionice(pid, class, Some(level)) {
                        if let Ok(mut s) = state.lock() {
                            s.append_log(format!(
                                "[Manual] ionice class={class} level={level} → PID {pid}"
                            ));
                        }
                        self.offer_rule(proc_name, None, None, Some((class, level)));
                    } else {
                        notify_error(&format!(
                            "Failed to set ionice {class}/{level} on {} (PID {pid}) — needs root?",
                            proc_name
                        ));
                    }
                }
                self.ionice_dialog = None;
            }
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
                    let _ = config::save(&cfg);
                }
                let _ = cmd_tx.send(DaemonCmd::ReapplyDefaults);
            } else if dismiss {
                self.rule_offer = None;
            }
        }
    }
}
