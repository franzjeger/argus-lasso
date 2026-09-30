//! Settings tab: default affinity, monitor intervals, appearance, autostart.

use crate::config::Config;
use crate::cpu_park::{detect_topology, CpuTopology};
use crate::gui::dialogs::AffinityDialog;
use crate::gui::theme::tokens;
use crate::gui::theme::{self, AppTheme};
use crate::utils::cpuset_to_cpulist;
use egui::Ui;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsSection {
    #[default]
    Appearance,
    Processes,
    Power,
    Notifications,
    Startup,
}

pub struct SettingsTab {
    pub section: SettingsSection,
    /// The settings as in effect; every change is stored at once.
    pub config: Config,
    pub default_affinity_enabled: bool,
    pub default_affinity_text: String,
    /// Why the typed CPU list was not used.
    affinity_error: Option<String>,
    pub cpu_dialog: Option<AffinityDialog>,
    pub opacity: f32,
    pub autostart_enabled: bool,
    /// A check or change of autostart in progress: systemctl can be slow,
    /// so it runs off the UI thread. Yields whether autostart is enabled
    /// afterwards, and what to say about it.
    autostart_job: Option<std::sync::mpsc::Receiver<(bool, String)>>,
    pub status: String,
    /// Active theme — changes are applied immediately in show().
    pub theme: AppTheme,
    // CPU Power state
    pub cpu_governor: String,
    pub available_governors: Vec<String>,
    pub cpu_epp: String,
    pub available_epps: Vec<String>,
    pub power_status: String,
    /// A governor/EPP change in progress: pkexec can wait on an
    /// authentication dialog, so it runs off the UI thread.
    power_job: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    /// When sysfs was last read for changes made outside this tab.
    power_synced: Option<std::time::Instant>,
    /// Detected CPU topology — drives dynamic quick-buttons
    pub topo: CpuTopology,
}

impl SettingsTab {
    pub fn new(config: Config) -> Self {
        let current_affinity = config.cpu.default_affinity.clone().unwrap_or_default();
        let default_affinity_enabled = !current_affinity.is_empty();
        let autostart_enabled = xdg_entry().is_some_and(|entry| entry.exists());
        // Restore opacity and theme from persisted config.
        let opacity = config.ui.opacity.clamp(0.1, 1.0);
        let theme = AppTheme::from_str(&config.ui.theme);
        let governor = read_governor();
        let epp = read_epp();
        Self {
            section: SettingsSection::default(),
            default_affinity_text: current_affinity,
            default_affinity_enabled,
            affinity_error: None,
            cpu_dialog: None,
            config,
            opacity,
            autostart_enabled,
            autostart_job: Some(spawn_autostart(|| {
                (check_autostart_enabled(), String::new())
            })),
            status: String::new(),
            theme,
            cpu_governor: governor,
            available_governors: read_available_governors(),
            cpu_epp: epp,
            available_epps: read_available_epps(),
            power_status: String::new(),
            power_job: None,
            power_synced: None,
            topo: detect_topology(),
        }
    }

    /// The default affinity the controls describe, or why the typed CPU
    /// list cannot be used.
    fn edited_affinity(&self) -> Result<Option<String>, String> {
        let text = self.default_affinity_text.trim();
        if !self.default_affinity_enabled || text.is_empty() {
            return Ok(None);
        }
        let count = crate::utils::get_cpu_count();
        match crate::utils::cpulist_to_set(text) {
            Ok(cpus) if !cpus.is_empty() && cpus.iter().all(|&cpu| cpu < count) => {
                Ok(Some(text.to_string()))
            }
            _ => Err(format!(
                "\"{text}\" is not a CPU list for this machine (CPUs 0–{})",
                count.saturating_sub(1)
            )),
        }
    }

    /// Store the default affinity the controls describe, if it is usable.
    /// Returns whether the configuration changed.
    fn commit_affinity(&mut self) -> bool {
        let affinity = match self.edited_affinity() {
            Ok(affinity) => affinity,
            Err(e) => {
                self.affinity_error = Some(e);
                return false;
            }
        };
        self.affinity_error = None;
        if affinity == self.config.cpu.default_affinity {
            return false;
        }
        self.status = match &affinity {
            Some(list) => format!("Default affinity → {list}"),
            None => "Default affinity off".into(),
        };
        self.config.cpu.default_affinity = affinity;
        true
    }

    /// Finish a CPU list typed but not entered, as leaving its field does:
    /// use it if it is valid, otherwise put back the one in effect and say
    /// so. For when the Processes section is no longer shown. Returns the
    /// settings if they changed.
    pub fn finish_editing(&mut self) -> Option<Config> {
        let pending = match self.edited_affinity() {
            Ok(affinity) => affinity != self.config.cpu.default_affinity,
            Err(_) => true,
        };
        if !pending {
            return None;
        }
        if self.commit_affinity() {
            return Some(self.config.clone());
        }
        let kept = self.config.cpu.default_affinity.clone();
        self.status = format!(
            "{} — kept {}",
            self.affinity_error.take().unwrap_or_default(),
            kept.as_deref().unwrap_or("all CPUs")
        );
        self.default_affinity_enabled = kept.is_some();
        self.default_affinity_text = kept.unwrap_or_default();
        None
    }

    /// Register or remove autostart as the checkbox now says, then show
    /// what is actually in place.
    fn commit_autostart(&mut self) {
        let enable = self.autostart_enabled;
        self.autostart_job = Some(spawn_autostart(move || {
            let result = if enable {
                write_autostart()
            } else {
                disable_autostart()
            };
            let note = match result {
                Ok(note) => note,
                Err(e) => format!("Autostart failed: {e}"),
            };
            (check_autostart_enabled(), note)
        }));
    }

    fn poll_autostart(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        let Some(job) = &self.autostart_job else {
            return;
        };
        match job.try_recv() {
            Ok((enabled, note)) => {
                self.autostart_enabled = enabled;
                if !note.is_empty() {
                    self.status = note;
                }
                self.autostart_job = None;
            }
            Err(TryRecvError::Disconnected) => self.autostart_job = None,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
    }

    /// Set a governor or EPP just picked. pkexec can wait on an
    /// authentication dialog, so it runs off the UI thread; see poll_power.
    fn commit_power(&mut self, governor: Option<String>, epp: Option<String>) {
        if self.power_job.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.power_job = Some(rx);
        self.power_status = "Applying CPU power settings…".into();
        std::thread::spawn(move || {
            let _ = tx.send(apply_power(governor, epp));
        });
    }

    /// Collect a finished governor/EPP change. The pickers then show what
    /// the kernel has, so a refused choice goes back to the value in effect.
    fn poll_power(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        let Some(job) = &self.power_job else {
            return;
        };
        let msgs = match job.try_recv() {
            Ok(msgs) => msgs,
            Err(TryRecvError::Disconnected) => vec!["CPU power change failed".into()],
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
        };
        self.power_job = None;
        self.sync_power();
        self.power_status = msgs.join("  ·  ");
        self.status = self.power_status.clone();
    }

    /// Follow governor and EPP as the kernel reports them: Gaming → Power
    /// profile changes both, and a governor change can change EPP.
    fn sync_power(&mut self) {
        self.cpu_governor = read_governor();
        self.cpu_epp = read_epp();
        self.power_synced = Some(std::time::Instant::now());
    }

    /// Returns the settings when one changed this frame; each change takes
    /// effect and is saved at once. Theme and opacity are read from the tab
    /// by the caller.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        opacity: f32,
        updates: &mut crate::updater::UpdateState,
    ) -> Option<Config> {
        let mut changed = false;
        if self.section != SettingsSection::Processes {
            changed |= self.finish_editing().is_some();
        }
        self.poll_power(ctx);
        self.poll_autostart(ctx);
        let stale = self
            .power_synced
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
        if self.section == SettingsSection::Power && self.power_job.is_none() && stale {
            self.sync_power();
        }

        crate::gui::theme::section_nav(
            ui,
            &mut self.section,
            &[
                (SettingsSection::Appearance, "Appearance"),
                (SettingsSection::Processes, "Processes"),
                (SettingsSection::Power, "CPU power"),
                (SettingsSection::Notifications, "Notifications"),
                (SettingsSection::Startup, "Startup & updates"),
            ],
        );
        egui::ScrollArea::vertical()
            .id_salt(("settings_body", self.section as u8))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.section == SettingsSection::Processes {
                    // ── Default CPU affinity ──────────────────────────────────────────
                    theme::card(ui, "Default CPU affinity", |ui| {
                        let help = if self.topo.has_asymmetry() {
                            format!(
                                "Applied to every process that doesn't match a specific rule. \
                         Detected: {}. Typical: Default → {}, Game rule → {}.",
                                self.topo.kind_label(),
                                self.topo.non_preferred_label,
                                self.topo.preferred_label,
                            )
                        } else {
                            "Applied to every process that doesn't match a specific rule.".to_string()
                        };
                        help_text(ui, &help);
                        ui.add_space(tokens::SPACE_S);

                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut self.default_affinity_enabled, "Enabled").changed() {
                                changed |= self.commit_affinity();
                            }
                            // Typed lists take effect on Enter, leaving the
                            // field or leaving the page (finish_editing), not
                            // with every keystroke.
                            let typed = ui.add(
                                egui::TextEdit::singleline(&mut self.default_affinity_text)
                                    .hint_text("e.g. 8-15,24-31")
                                    .desired_width(130.0)
                                    .interactive(self.default_affinity_enabled),
                            );
                            if typed.changed() {
                                self.affinity_error = None;
                            }
                            if typed.lost_focus() {
                                changed |= self.commit_affinity();
                            }
                            if ui
                                .add_enabled(
                                    self.default_affinity_enabled,
                                    egui::Button::new("Pick CPUs…"),
                                )
                                .clicked()
                            {
                                self.cpu_dialog =
                                    Some(AffinityDialog::new(&self.default_affinity_text, "Default", "default"));
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Quick presets:")
                                    .color(ui.visuals().weak_text_color()),
                            );
                            let current = self.default_affinity_text.trim().to_string();
                            let on = self.default_affinity_enabled;
                            if self.topo.has_asymmetry() {
                                let pref = cpuset_to_cpulist(&self.topo.preferred);
                                let npref = cpuset_to_cpulist(&self.topo.non_preferred);
                                if theme::chip(
                                    ui,
                                    &self.topo.preferred_button_label(),
                                    on && current == pref,
                                ) {
                                    self.default_affinity_text = pref;
                                    self.default_affinity_enabled = true;
                                    changed |= self.commit_affinity();
                                }
                                if theme::chip(
                                    ui,
                                    &self.topo.non_preferred_button_label(),
                                    on && current == npref,
                                ) {
                                    self.default_affinity_text = npref;
                                    self.default_affinity_enabled = true;
                                    changed |= self.commit_affinity();
                                }
                            }
                            if theme::chip(ui, "All CPU threads", on && current.is_empty()) {
                                self.default_affinity_text = String::new();
                                self.default_affinity_enabled = true;
                                changed |= self.commit_affinity();
                            }
                        });
                        if let Some(e) = &self.affinity_error {
                            ui.colored_label(theme::sem(ui).negative, e);
                        }
                    });

                    ui.add_space(tokens::SPACE_M);

                    // ── Monitoring ────────────────────────────────────────────────────
                    theme::card(ui, "Monitoring", |ui| {
                        help_text(
                            ui,
                            "How often rules are enforced on running processes, and how often \
                     the process table refreshes on screen.",
                        );
                        ui.add_space(tokens::SPACE_S);

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Apply rules every", |ui| {
                            changed |= committed(&ui.add(
                                theme::number(&mut self.config.monitor.rule_enforce_interval_ms)
                                    .range(100..=10000)
                                    .suffix(" ms"),
                            ));
                        });

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Refresh process list", |ui| {
                            const PICKS: [u64; 4] = [500, 1000, 2000, 5000];
                            let sel = PICKS
                                .iter()
                                .position(|ms| *ms == self.config.monitor.display_refresh_interval_ms)
                                .unwrap_or(usize::MAX);
                            if let Some(i) = theme::segmented(ui, &["0.5 s", "1 s", "2 s", "5 s"], sel) {
                                self.config.monitor.display_refresh_interval_ms = PICKS[i];
                                changed = true;
                            }
                        });
                    });

                    ui.add_space(tokens::SPACE_M);

                }
                if self.section == SettingsSection::Appearance {
                    // ── Appearance and power ──────────────────────────────────────────
                    theme::card(ui, "Appearance", |ui| {
                        help_text(
                            ui,
                            "Game overlay appearance is in Gaming → Overlay.",
                        );
                        ui.add_space(tokens::SPACE_S);

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Theme", |ui| {
                            let prev_theme = self.theme.clone();
                            egui::ComboBox::from_id_salt("theme_picker")
                                .selected_text(self.theme.label())
                                .show_ui(ui, |ui| {
                                    for t in [
                                        AppTheme::BreezeDark,
                                        AppTheme::BreezeLight,
                                        AppTheme::AdwaitaDark,
                                        AppTheme::AdwaitaLight,
                                    ] {
                                        ui.selectable_value(&mut self.theme, t.clone(), t.label());
                                    }
                                });
                            if self.theme != prev_theme {
                                theme::apply_theme(ctx, &self.theme);
                            }
                        });

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Window opacity", |ui| {
                            // The track is painted with `inactive.bg_fill`, which
                            // equals the window background — invisible without a
                            // value fill. Enable the trailing fill (accent colour)
                            // so the slider reads as a slider, not a floating box.
                            ui.spacing_mut().slider_width = 200.0;
                            ui.add(
                                egui::Slider::new(&mut self.opacity, 0.1f32..=1.0)
                                    .trailing_fill(true)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                    .custom_parser(|s| {
                                        s.trim_end_matches('%')
                                            .trim()
                                            .parse::<f64>()
                                            .ok()
                                            .map(|p| p / 100.0)
                                    })
                                    .show_value(true),
                            );
                        });

                    });
                }
                if self.section == SettingsSection::Power {
                    theme::card(ui, "CPU power management", |ui| {
                        help_text(ui, "Controls CPU frequency policy and the balance between performance and energy use.");
                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Frequency policy (governor)", |ui| {
                            if self.available_governors.is_empty() {
                                ui.label(
                                    egui::RichText::new("Unavailable on this system")
                                        .italics()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            } else {
                                let before = self.cpu_governor.clone();
                                ui.add_enabled_ui(self.power_job.is_none(), |ui| {
                                egui::ComboBox::from_id_salt("gov_picker")
                                    .selected_text(&self.cpu_governor)
                                    .show_ui(ui, |ui| {
                                        for g in &self.available_governors.clone() {
                                            ui.selectable_value(
                                                &mut self.cpu_governor,
                                                g.clone(),
                                                g.as_str(),
                                            );
                                        }
                                    });
                                });
                                if self.cpu_governor != before {
                                    self.commit_power(Some(self.cpu_governor.clone()), None);
                                }
                            }
                        });

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Energy preference (EPP)", |ui| {
                            if self.available_epps.is_empty() {
                                ui.label(
                                    egui::RichText::new("Unavailable on this system")
                                        .italics()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            } else {
                                let before = self.cpu_epp.clone();
                                ui.add_enabled_ui(self.power_job.is_none(), |ui| {
                                egui::ComboBox::from_id_salt("epp_picker")
                                    .selected_text(&self.cpu_epp)
                                    .show_ui(ui, |ui| {
                                        for e in &self.available_epps.clone() {
                                            ui.selectable_value(
                                                &mut self.cpu_epp,
                                                e.clone(),
                                                e.as_str(),
                                            );
                                        }
                                    });
                                });
                                if self.cpu_epp != before {
                                    self.commit_power(None, Some(self.cpu_epp.clone()));
                                }
                            }
                        });

                        if !self.power_status.is_empty() {
                            help_text(ui, &self.power_status.clone());
                        }
                    });

                    ui.add_space(tokens::SPACE_M);

                }
                if self.section == SettingsSection::Notifications {
                    // ── Notifications and startup ─────────────────────────────────────
                    theme::card(ui, "Desktop notifications", |ui| {
                        help_text(
                            ui,
                            "Desktop notifications cover ProBalance throttling, hardware alerts \
                     and process termination.",
                        );
                        ui.add_space(tokens::SPACE_S);

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Desktop notifications", |ui| {
                            changed |= ui
                                .checkbox(&mut self.config.ui.notifications_enabled, "Enabled")
                                .changed();
                        });

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Temperature alerts", |ui| {
                            changed |= ui.checkbox(&mut self.config.hw_alerts.enabled, "Enabled").changed();
                            let on = self.config.hw_alerts.enabled;
                            let weak = ui.visuals().weak_text_color();
                            ui.add_enabled_ui(on, |ui| {
                                ui.label("at");
                                changed |= committed(&ui.add(
                                    theme::number(
                                        &mut self.config.hw_alerts.temp_threshold_celsius,
                                    )
                                    .range(50.0..=110.0)
                                    .speed(1.0)
                                    .fixed_decimals(0)
                                    .suffix(" °C"),
                                ));
                                ui.colored_label(weak, "·  at least");
                                changed |= committed(&ui.add(
                                    theme::number(&mut self.config.hw_alerts.cooldown_secs)
                                        .range(10..=300)
                                        .speed(5.0)
                                        .suffix(" s"),
                                ));
                                ui.colored_label(weak, "between alerts");
                            });
                        });

                    });
                }
                if self.section == SettingsSection::Startup {
                    theme::card(ui, "Startup", |ui| {
                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Start with session", |ui| {
                            let idle = self.autostart_job.is_none();
                            if ui
                                .add_enabled(
                                    idle,
                                    egui::Checkbox::new(
                                        &mut self.autostart_enabled,
                                        "Launch Argus-Lasso automatically with your desktop session",
                                    ),
                                )
                                .changed()
                            {
                                self.commit_autostart();
                            }
                        });
                    });

                    ui.add_space(tokens::SPACE_M);

                    // ── Updates ───────────────────────────────────────────────────────
                    theme::card(ui, "Updates", |ui| {
                        help_text(
                            ui,
                            "Argus-Lasso updates the app and Vulkan overlay together from GitHub \
                     releases. A system-wide install is left to your package manager.",
                        );
                        ui.add_space(tokens::SPACE_S);

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Installed version", |ui| {
                            ui.label(
                                egui::RichText::new(format!("v{}", crate::updater::current_version()))
                                    .font(theme::num_font(tokens::FONT_BODY)),
                            );
                            // The outcome qualifies the version, so it sits beside
                            // it as weak subtext. As a loose line under the whole
                            // group it read as an unrelated status message.
                            if !updates.message.is_empty() {
                                ui.add_space(tokens::SPACE_XS);
                                ui.label(
                                    egui::RichText::new(&updates.message)
                                        .size(tokens::FONT_HELP)
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                            ui.add_space(tokens::SPACE_S);
                            let label = if updates.busy {
                                "Working…"
                            } else {
                                "Check now"
                            };
                            if ui
                                .add_enabled(
                                    !updates.busy && !updates.installed,
                                    egui::Button::new(label),
                                )
                                .on_disabled_hover_text(
                                    "Restart Argus first: it is still running the version \
                                     it replaced.",
                                )
                                .clicked()
                            {
                                updates.start_check();
                            }
                            let pending = updates
                                .available
                                .as_ref()
                                .map(|u| (u.tag.clone(), u.page_url.clone()));
                            if let Some((tag, page_url)) = pending {
                                let s = theme::sem(ui);
                                if updates.installed {
                                    let btn = egui::Button::new(
                                        egui::RichText::new("Restart now").color(s.on_accent),
                                    )
                                    .fill(s.accent);
                                    if ui.add(btn).clicked() {
                                        updates.restart_requested = true;
                                    }
                                } else {
                                    let btn = egui::Button::new(
                                        egui::RichText::new(format!("Update to {tag}"))
                                            .color(s.on_accent),
                                    )
                                    .fill(s.accent);
                                    if ui.add_enabled(!updates.busy, btn).clicked() {
                                        updates.start_install();
                                    }
                                }
                                if !page_url.is_empty() {
                                    ui.hyperlink_to("Release notes", &page_url);
                                }
                            }
                        });

                        ui.horizontal_wrapped(|ui| {
                            if updates.installed && updates.available.is_none() && ui.button("Restart now").clicked() { updates.restart_requested = true; }
                            if ui.add_enabled(!updates.busy && crate::updater::rollback_available(), egui::Button::new("Restore previous app and overlay")).clicked() {
                                updates.start_rollback();
                            }
                        });
                        help_text(ui, "A previous installation is retained after an update. Restart games to load the matching overlay. Local service customizations are preserved.");

                        crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Check on startup", |ui| {
                            changed |= ui
                                .checkbox(&mut self.config.ui.check_updates_on_start, "Enabled")
                                .changed();
                        });
                    });

                }
                if !self.status.is_empty() {
                    ui.add_space(tokens::SPACE_S);
                    ui.colored_label(ui.visuals().weak_text_color(), &self.status);
                }
            });

        // Drawn whichever section is open: it is a window of its own, and
        // tied to the Processes section it vanished and came back with it.
        if let Some(ref mut dlg) = self.cpu_dialog {
            if let Some(result) = dlg.show(ctx, opacity) {
                if !result.is_empty() {
                    self.default_affinity_text = result;
                    changed |= self.commit_affinity();
                }
                self.cpu_dialog = None;
            }
        }

        changed.then(|| self.config.clone())
    }
}

/// A number field's edit is finished: the drag was released, or a typed or
/// stepped value was entered. Storing on every frame of a drag would save
/// the configuration dozens of times a second.
fn committed(response: &egui::Response) -> bool {
    response.drag_stopped() || (response.changed() && !response.dragged())
}

/// Weak, small help line under a group title (§7).
fn help_text(ui: &mut Ui, text: &str) {
    theme::help_text(ui, text);
}

// ── CPU governor / EPP sysfs helpers ─────────────────────────────────────────

fn read_governor() -> String {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_available_governors() -> Vec<String> {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors")
        .map(|s| s.split_whitespace().map(|t| t.to_string()).collect())
        .unwrap_or_default()
}

/// Write `value` to `path_suffix` under every CPU's cpufreq directory (e.g.
/// "cpufreq/scaling_governor"), falling back to the privileged polkit helper
/// — which owns the privileged path and covers every core itself — if ANY
/// core's direct write failed, not only if every one of them did. A
/// non-uniform sysfs permission setup (or one core in an unexpected state)
/// previously reported success while silently leaving some cores on their
/// old value, because the fallback only fired when EVERY core failed.
fn write_sysfs_all_cpus(
    path_suffix: &str,
    value: &str,
    fallback: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    write_sysfs_all_cpus_at(
        std::path::Path::new("/sys/devices/system/cpu"),
        crate::utils::get_cpu_count(),
        path_suffix,
        value,
        fallback,
    )
}

fn write_sysfs_all_cpus_at(
    base: &std::path::Path,
    cpu_count: u32,
    path_suffix: &str,
    value: &str,
    fallback: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let mut errors = 0usize;
    for i in 0..cpu_count {
        let path = base.join(format!("cpu{i}")).join(path_suffix);
        if std::fs::write(&path, value).is_err() {
            errors += 1;
        }
    }
    if errors > 0 {
        fallback(value)
    } else {
        Ok(())
    }
}

/// Set the governor, then EPP (a governor change can reset EPP), and say
/// what the kernel ended up with. Runs off the UI thread.
fn apply_power(governor: Option<String>, epp: Option<String>) -> Vec<String> {
    let mut msgs = Vec::new();
    let mut report = |what: &str, wanted: &str, set: Result<(), String>, now: String| {
        msgs.push(match set {
            Err(e) => format!("{what} {wanted} failed: {e}"),
            Ok(()) if now != wanted => format!("{what} {wanted} did not take effect (still {now})"),
            Ok(()) => format!("{what} → {wanted}"),
        });
    };
    if let Some(governor) = governor {
        report(
            "Governor",
            &governor,
            set_governor(&governor),
            read_governor(),
        );
    }
    if let Some(epp) = epp {
        report("EPP", &epp, set_epp(&epp), read_epp());
    }
    msgs
}

fn set_governor(governor: &str) -> Result<(), String> {
    write_sysfs_all_cpus(
        "cpufreq/scaling_governor",
        governor,
        crate::cpu_park::set_governor_via_helper,
    )
}

fn read_epp() -> String {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_available_epps() -> Vec<String> {
    std::fs::read_to_string(
        "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_available_preferences",
    )
    .map(|s| s.split_whitespace().map(|t| t.to_string()).collect())
    .unwrap_or_default()
}

fn set_epp(epp: &str) -> Result<(), String> {
    write_sysfs_all_cpus(
        "cpufreq/energy_performance_preference",
        epp,
        crate::cpu_park::set_epp_via_helper,
    )
}

const SERVICE_UNIT: &str = "argus-lasso.service";

fn spawn_autostart(
    work: impl FnOnce() -> (bool, String) + Send + 'static,
) -> std::sync::mpsc::Receiver<(bool, String)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx
}

/// `systemctl --user <args>`'s output, if it ran and succeeded.
fn systemctl_user(args: &[&str]) -> Option<String> {
    std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Whether the service unit the installer or a package set up is known to
/// the user's service manager, from the user's or the system's unit
/// directories. Starting with the session then means enabling it.
fn service_unit_installed() -> bool {
    systemctl_user(&["show", "-p", "LoadState", "--value", SERVICE_UNIT])
        .is_some_and(|state| state == "loaded")
}

fn xdg_entry() -> Option<std::path::PathBuf> {
    crate::config::home_dir().map(|home| home.join(".config/autostart/argus-lasso.desktop"))
}

fn check_autostart_enabled() -> bool {
    xdg_entry().is_some_and(|entry| entry.exists())
        || systemctl_user(&["is-enabled", SERVICE_UNIT]).is_some_and(|state| state == "enabled")
}

/// `path` as one argument of a desktop entry's Exec key. The spec applies
/// two escapings in order: quoting (reserved characters inside the quotes
/// get a backslash) and then the general string escaping of every value
/// (each backslash doubled), which readers undo first. `%` starts a field
/// code and is doubled.
fn desktop_exec_arg(path: &std::path::Path) -> String {
    let mut out = String::from("\"");
    for c in path.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Start with the session through one mechanism only: the installed service
/// if there is one, otherwise an XDG autostart entry. Both used to be set up,
/// so two instances started at login, and the one that lost the instance
/// lock asked the other to show its window.
fn write_autostart() -> std::io::Result<String> {
    let entry = xdg_entry().ok_or_else(|| std::io::Error::other("home directory unknown"))?;
    if service_unit_installed() {
        let _ = std::fs::remove_file(&entry);
        return match systemctl_user(&["enable", SERVICE_UNIT]) {
            Some(_) => Ok("Autostart enabled (systemd service)".into()),
            None => Err(std::io::Error::other(format!(
                "systemctl --user enable {SERVICE_UNIT} failed"
            ))),
        };
    }
    // The path of the binary on disk: after an update, before a restart,
    // the running image's path ends in " (deleted)".
    let exe = crate::updater::install_target()
        .unwrap_or_else(|_| std::path::PathBuf::from("argus-lasso"));
    if let Some(dir) = entry.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nType=Application\nName=Argus-Lasso\n\
             Exec={} --minimized\nIcon=argus-lasso\nHidden=false\n\
             X-GNOME-Autostart-enabled=true\n",
            desktop_exec_arg(&exe)
        ),
    )?;
    Ok("Autostart enabled".into())
}

fn disable_autostart() -> std::io::Result<String> {
    if let Some(entry) = xdg_entry() {
        match std::fs::remove_file(&entry) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    let enabled =
        || systemctl_user(&["is-enabled", SERVICE_UNIT]).is_some_and(|state| state == "enabled");
    if enabled() && systemctl_user(&["disable", SERVICE_UNIT]).is_none() {
        return Err(std::io::Error::other(format!(
            "systemctl --user disable {SERVICE_UNIT} failed"
        )));
    }
    Ok("Autostart disabled".into())
}

#[cfg(test)]
mod tests {

    #[test]
    fn exec_paths_are_quoted_for_a_desktop_entry() {
        let path = std::path::Path::new("/home/a b/100%/\"x\"/$bin");
        assert_eq!(
            desktop_exec_arg(path),
            r#""/home/a b/100%%/\\"x\\"/\\$bin""#
        );
        assert_eq!(
            desktop_exec_arg(std::path::Path::new(r"/a\b")),
            r#""/a\\\\b""#
        );
    }

    use super::{desktop_exec_arg, write_sysfs_all_cpus_at};
    use std::cell::Cell;

    fn fake_cpu_dir(root: &std::path::Path, writable_cpus: &[u32], cpu_count: u32) {
        std::fs::create_dir_all(root).unwrap();
        for i in 0..cpu_count {
            if writable_cpus.contains(&i) {
                // A regular directory: the write below will succeed.
                std::fs::create_dir_all(root.join(format!("cpu{i}"))).unwrap();
            }
            // Cores not in `writable_cpus` get no directory at all, so
            // writing "cpu{i}/governor" fails with NotFound — standing in
            // for a real permission failure without needing root to set up
            // an actually-unwritable file.
        }
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "argus-settings-tab-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn no_fallback_when_every_core_writes_successfully() {
        let root = temp_root("all-ok");
        fake_cpu_dir(&root, &[0, 1, 2], 3);
        let fallback_called = Cell::new(false);
        let result = write_sysfs_all_cpus_at(&root, 3, "governor", "performance", |_| {
            fallback_called.set(true);
            Ok(())
        });
        assert!(result.is_ok());
        assert!(
            !fallback_called.get(),
            "fallback must not run when nothing failed"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The bug this exists for: previously the fallback only ran when EVERY
    /// core failed. One failure out of several must still trigger it.
    #[test]
    fn fallback_runs_when_only_one_of_several_cores_fails() {
        let root = temp_root("one-fails");
        fake_cpu_dir(&root, &[0, 2], 3); // cpu1 has no directory -> its write fails
        let fallback_called = Cell::new(false);
        let result = write_sysfs_all_cpus_at(&root, 3, "governor", "performance", |_| {
            fallback_called.set(true);
            Ok(())
        });
        assert!(result.is_ok());
        assert!(
            fallback_called.get(),
            "a single core's failed write must still trigger the fallback"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fallback_runs_when_every_core_fails() {
        let root = temp_root("all-fail");
        fake_cpu_dir(&root, &[], 3); // no cpu directories at all
        let fallback_called = Cell::new(false);
        let result = write_sysfs_all_cpus_at(&root, 3, "governor", "performance", |_| {
            fallback_called.set(true);
            Ok(())
        });
        assert!(result.is_ok());
        assert!(fallback_called.get());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fallback_error_propagates() {
        let root = temp_root("fallback-fails");
        fake_cpu_dir(&root, &[], 1);
        let result = write_sysfs_all_cpus_at(&root, 1, "governor", "performance", |_| {
            Err("helper unavailable".to_string())
        });
        assert_eq!(result, Err("helper unavailable".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Clicks in the tab, rendered headless; returns what `show` reported.
    fn click(tab: &mut super::SettingsTab, label: &str) -> Option<crate::config::Config> {
        let ctx = egui::Context::default();
        crate::gui::theme::apply_theme(&ctx, &crate::gui::theme::AppTheme::BreezeDark);
        ctx.enable_accesskit();
        let mut updates = crate::updater::UpdateState::default();
        let mut reported = None;
        let mut frame = |events: Vec<egui::Event>, tab: &mut super::SettingsTab| {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 700.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |root| {
                egui::CentralPanel::default().show_inside(root, |ui| {
                    let ctx = ui.ctx().clone();
                    if let Some(config) = tab.show(ui, &ctx, 1.0, &mut updates) {
                        reported = Some(config);
                    }
                });
            })
        };
        frame(vec![], tab);
        let output = frame(vec![], tab);
        let update = output
            .platform_output
            .accesskit_update
            .expect("accesskit update");
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Apply changes")),
            "settings have no apply step"
        );
        let bounds = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("nothing labelled {label}"));
        let pos = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![egui::Event::PointerMoved(pos), button(true)], tab);
        frame(vec![button(false)], tab);
        frame(vec![], tab);
        reported
    }

    /// Settings used to wait for "Apply changes" while theme and opacity
    /// on the same page took effect at once.
    #[test]
    fn a_setting_takes_effect_when_it_is_changed() {
        let mut tab = super::SettingsTab::new(crate::config::Config::default());
        tab.section = super::SettingsSection::Processes;
        let reported = click(&mut tab, "5 s").expect("the change is reported at once");
        assert_eq!(reported.monitor.display_refresh_interval_ms, 5000);
    }

    #[test]
    fn a_typed_cpu_list_is_checked_before_it_is_used() {
        let mut tab = super::SettingsTab::new(crate::config::Config::default());
        tab.default_affinity_enabled = true;
        tab.default_affinity_text = "abc".into();
        assert!(!tab.commit_affinity());
        assert!(tab.affinity_error.is_some());
        assert_eq!(tab.config.cpu.default_affinity, None);

        tab.default_affinity_text = "0".into();
        assert!(tab.commit_affinity());
        assert_eq!(tab.config.cpu.default_affinity.as_deref(), Some("0"));
        assert!(tab.affinity_error.is_none());
        assert!(!tab.commit_affinity(), "nothing new to store");

        tab.default_affinity_enabled = false;
        assert!(tab.commit_affinity());
        assert_eq!(tab.config.cpu.default_affinity, None);
    }

    /// A CPU list typed but not entered used to be lost, still showing, when
    /// the section or tab changed: egui drops the focus of a field it no
    /// longer draws without reporting it.
    #[test]
    fn a_typed_cpu_list_is_finished_when_the_page_is_left() {
        let mut tab = super::SettingsTab::new(crate::config::Config::default());
        assert!(tab.finish_editing().is_none(), "nothing typed");

        tab.default_affinity_enabled = true;
        tab.default_affinity_text = "0".into();
        let stored = tab.finish_editing().expect("a valid list is used");
        assert_eq!(stored.cpu.default_affinity.as_deref(), Some("0"));

        tab.default_affinity_text = "abc".into();
        assert!(tab.finish_editing().is_none());
        assert_eq!(
            tab.default_affinity_text, "0",
            "the list in effect is shown again"
        );
        assert!(tab.status.contains("kept 0"), "{}", tab.status);
    }
}
