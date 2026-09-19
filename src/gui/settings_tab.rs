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
    pub config: Config,
    /// Snapshot of the last-saved config — drives the dirty indicator and Discard.
    pub saved: Config,
    pub default_affinity_enabled: bool,
    pub default_affinity_text: String,
    pub cpu_dialog: Option<AffinityDialog>,
    pub opacity: f32,
    pub native_ppp: f32,
    pub autostart_enabled: bool,
    /// Autostart state as last written to disk.
    saved_autostart: bool,
    pub status: String,
    /// Active theme — changes are applied immediately in show().
    pub theme: AppTheme,
    // CPU Power state
    pub cpu_governor: String,
    pub available_governors: Vec<String>,
    pub cpu_epp: String,
    pub available_epps: Vec<String>,
    /// Governor/EPP as last applied to sysfs.
    saved_governor: String,
    saved_epp: String,
    pub power_status: String,
    /// Detected CPU topology — drives dynamic quick-buttons
    pub topo: CpuTopology,
}

impl SettingsTab {
    pub fn new(config: Config) -> Self {
        let current_affinity = config.cpu.default_affinity.clone().unwrap_or_default();
        let default_affinity_enabled = !current_affinity.is_empty();
        let autostart_enabled = check_autostart_enabled();
        // Restore opacity and theme from persisted config.
        let opacity = config.ui.opacity.clamp(0.1, 1.0);
        let theme = AppTheme::from_str(&config.ui.theme);
        let governor = read_governor();
        let epp = read_epp();
        Self {
            section: SettingsSection::default(),
            default_affinity_text: current_affinity,
            default_affinity_enabled,
            cpu_dialog: None,
            saved: config.clone(),
            config,
            opacity,
            native_ppp: 1.0,
            autostart_enabled,
            saved_autostart: autostart_enabled,
            status: String::new(),
            theme,
            saved_governor: governor.clone(),
            cpu_governor: governor,
            available_governors: read_available_governors(),
            saved_epp: epp.clone(),
            cpu_epp: epp,
            available_epps: read_available_epps(),
            power_status: String::new(),
            topo: detect_topology(),
        }
    }

    /// Default affinity as it would be stored in the config.
    fn edited_affinity(&self) -> Option<String> {
        if self.default_affinity_enabled {
            let t = self.default_affinity_text.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        } else {
            None
        }
    }

    /// True when the edited state differs from the last-saved state.
    /// Opacity/theme are deliberately excluded — they are live-preview fields
    /// that app.rs applies and persists on every frame.
    fn is_dirty(&self) -> bool {
        let a = &self.config;
        let b = &self.saved;
        self.edited_affinity() != b.cpu.default_affinity
            || a.monitor.display_refresh_interval_ms != b.monitor.display_refresh_interval_ms
            || a.monitor.rule_enforce_interval_ms != b.monitor.rule_enforce_interval_ms
            || a.ui.notifications_enabled != b.ui.notifications_enabled
            || a.ui.check_updates_on_start != b.ui.check_updates_on_start
            || a.hw_alerts.enabled != b.hw_alerts.enabled
            || (a.hw_alerts.temp_threshold_celsius - b.hw_alerts.temp_threshold_celsius).abs()
                > 0.01
            || a.hw_alerts.cooldown_secs != b.hw_alerts.cooldown_secs
            || self.autostart_enabled != self.saved_autostart
            || self.cpu_governor != self.saved_governor
            || self.cpu_epp != self.saved_epp
    }

    /// Restore every edited field from the saved snapshot.
    fn discard(&mut self) {
        self.config = self.saved.clone();
        let aff = self.saved.cpu.default_affinity.clone().unwrap_or_default();
        self.default_affinity_enabled = !aff.is_empty();
        self.default_affinity_text = aff;
        self.autostart_enabled = self.saved_autostart;
        self.cpu_governor = self.saved_governor.clone();
        self.cpu_epp = self.saved_epp.clone();
        self.status = "Changes discarded.".into();
        self.power_status.clear();
    }

    /// Commit the edited state: write affinity into the config, push
    /// governor/EPP to sysfs and (de)register autostart. Returns the config
    /// that the caller should persist.
    fn apply(&mut self, ctx: &egui::Context) -> Config {
        let mut msgs: Vec<String> = Vec::new();

        // Default affinity
        self.config.cpu.default_affinity = self.edited_affinity();
        match &self.config.cpu.default_affinity {
            Some(list) => msgs.push(format!("Default affinity → {list}")),
            None if self.default_affinity_enabled => {
                msgs.push("Default affinity → all CPUs".into())
            }
            None => msgs.push("Default affinity disabled".into()),
        }

        // Live-preview fields are persisted alongside everything else.
        self.config.ui.opacity = self.opacity;
        self.config.ui.theme = self.theme.to_str().into();
        theme::apply_theme(ctx, self.native_ppp, &self.theme);

        // CPU power
        if !self.available_governors.is_empty() && self.cpu_governor != self.saved_governor {
            match set_governor(&self.cpu_governor) {
                Ok(_) => {
                    msgs.push(format!("Governor → {}", self.cpu_governor));
                    self.saved_governor = self.cpu_governor.clone();
                }
                Err(e) => msgs.push(format!("Governor failed: {e}")),
            }
        }
        if !self.available_epps.is_empty() && self.cpu_epp != self.saved_epp {
            match set_epp(&self.cpu_epp) {
                Ok(_) => {
                    msgs.push(format!("EPP → {}", self.cpu_epp));
                    self.saved_epp = self.cpu_epp.clone();
                }
                Err(e) => msgs.push(format!("EPP failed: {e}")),
            }
        }

        // Autostart
        if self.autostart_enabled != self.saved_autostart {
            if self.autostart_enabled {
                match write_autostart() {
                    Ok(note) => {
                        msgs.push(note);
                        self.saved_autostart = true;
                    }
                    Err(e) => msgs.push(format!("Autostart failed: {e}")),
                }
            } else {
                match disable_autostart() {
                    Ok(note) => {
                        msgs.push(note);
                        self.saved_autostart = false;
                    }
                    Err(e) => msgs.push(format!("Disable failed: {e}")),
                }
            }
        }

        self.status = msgs.join("  ·  ");
        self.saved = self.config.clone();
        self.config.clone()
    }

    /// Returns Some(updated_config) when "Apply changes" is clicked.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        opacity: f32,
        updates: &mut crate::updater::UpdateState,
    ) -> Option<Config> {
        let mut applied: Option<Config> = None;

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
        // Reserve room for the apply bar, then scroll everything above it.
        let bar_h = 44.0;
        let body_h = (ui.available_height() - bar_h).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt(("settings_body", self.section as u8))
            .max_height(body_h)
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
                        ui.checkbox(&mut self.default_affinity_enabled, "Enabled");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.default_affinity_text)
                                .hint_text("e.g. 8-15,24-31")
                                .desired_width(130.0)
                                .interactive(self.default_affinity_enabled),
                        );
                        if ui
                            .add_enabled(
                                self.default_affinity_enabled,
                                egui::Button::new("Pick CPUs…"),
                            )
                            .clicked()
                        {
                            self.cpu_dialog =
                                Some(AffinityDialog::new(&self.default_affinity_text, "Default"));
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
                            }
                            if theme::chip(
                                ui,
                                &self.topo.non_preferred_button_label(),
                                on && current == npref,
                            ) {
                                self.default_affinity_text = npref;
                                self.default_affinity_enabled = true;
                            }
                        }
                        if theme::chip(ui, "All CPU threads", on && current.is_empty()) {
                            self.default_affinity_text = String::new();
                            self.default_affinity_enabled = true;
                        }
                    });
                });

                // Handle Pick CPUs dialog
                if let Some(ref mut dlg) = self.cpu_dialog {
                    if let Some(result) = dlg.show(ctx, opacity) {
                        if !result.is_empty() {
                            self.default_affinity_text = result;
                        }
                        self.cpu_dialog = None;
                    }
                }

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
                        ui.add(
                            egui::DragValue::new(&mut self.config.monitor.rule_enforce_interval_ms)
                                .range(100..=10000)
                                .suffix(" ms"),
                        );
                    });

                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Refresh process list", |ui| {
                        const PICKS: [u64; 4] = [500, 1000, 2000, 5000];
                        let sel = PICKS
                            .iter()
                            .position(|ms| *ms == self.config.monitor.display_refresh_interval_ms)
                            .unwrap_or(usize::MAX);
                        if let Some(i) = theme::segmented(ui, &["0.5 s", "1 s", "2 s", "5 s"], sel) {
                            self.config.monitor.display_refresh_interval_ms = PICKS[i];
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
                        "Theme and window opacity are saved immediately. Game overlay appearance is in Gaming → Overlay.",
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
                            theme::apply_theme(ctx, self.native_ppp, &self.theme);
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
                    help_text(ui, "Controls CPU frequency policy and the balance between performance and energy use. Changes take effect with Apply changes.");
                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Frequency policy (governor)", |ui| {
                        if self.available_governors.is_empty() {
                            ui.label(
                                egui::RichText::new("Unavailable on this system")
                                    .italics()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        } else {
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
                        ui.checkbox(&mut self.config.ui.notifications_enabled, "Enabled");
                    });

                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Temperature alerts", |ui| {
                        ui.checkbox(&mut self.config.hw_alerts.enabled, "Enabled");
                        let on = self.config.hw_alerts.enabled;
                        let weak = ui.visuals().weak_text_color();
                        ui.add_enabled_ui(on, |ui| {
                            ui.label("at");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.config.hw_alerts.temp_threshold_celsius,
                                )
                                .range(50.0..=110.0)
                                .speed(1.0)
                                .fixed_decimals(0)
                                .suffix(" °C"),
                            );
                            ui.colored_label(weak, "·  at least");
                            ui.add(
                                egui::DragValue::new(&mut self.config.hw_alerts.cooldown_secs)
                                    .range(10..=300)
                                    .speed(5.0)
                                    .suffix(" s"),
                            );
                            ui.colored_label(weak, "between alerts");
                        });
                    });

                });
                }
                if self.section == SettingsSection::Startup {
                theme::card(ui, "Startup", |ui| {
                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Start with session", |ui| {
                        ui.checkbox(
                            &mut self.autostart_enabled,
                            "Launch Argus-Lasso automatically with your desktop session",
                        );
                    });
                });

                ui.add_space(tokens::SPACE_M);

                // ── Updates ───────────────────────────────────────────────────────
                theme::card(ui, "Updates", |ui| {
                    help_text(
                        ui,
                        "Argus-Lasso can replace its own binary from the project's GitHub \
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
                            .add_enabled(!updates.busy, egui::Button::new(label))
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

                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Check on startup", |ui| {
                        ui.checkbox(&mut self.config.ui.check_updates_on_start, "Enabled");
                    });
                });

                }
                if !self.status.is_empty() {
                    ui.add_space(tokens::SPACE_S);
                    ui.colored_label(ui.visuals().weak_text_color(), &self.status);
                }
            });

        // ── Single bottom apply bar (§5) ──────────────────────────────────
        let dirty = self.is_dirty();
        let (discard, apply) = theme::apply_bar(ui, dirty);
        if discard {
            self.discard();
        }
        if apply {
            applied = Some(self.apply(ctx));
        }

        applied
    }
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

fn set_governor(governor: &str) -> Result<(), String> {
    // Try direct sysfs write first, fall back to privileged helper.
    let cpu_count = crate::utils::get_cpu_count();
    let mut errors = 0usize;
    for i in 0..cpu_count {
        let path = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_governor");
        if std::fs::write(&path, governor).is_err() {
            errors += 1;
        }
    }
    if errors == cpu_count as usize {
        // Every direct sysfs write was refused — fall back to the polkit
        // helper, which owns the privileged path now.
        crate::cpu_park::set_governor_via_helper(governor)
    } else {
        Ok(())
    }
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
    // Try direct sysfs write first, fall back to privileged helper.
    let cpu_count = crate::utils::get_cpu_count();
    let mut errors = 0usize;
    for i in 0..cpu_count {
        let path = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/energy_performance_preference");
        if std::fs::write(&path, epp).is_err() {
            errors += 1;
        }
    }
    if errors == cpu_count as usize {
        crate::cpu_park::set_epp_via_helper(epp)
    } else {
        Ok(())
    }
}

fn check_autostart_enabled() -> bool {
    // Check XDG autostart first (works on GNOME and KDE).
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg = format!("{home}/.config/autostart/argus-lasso.desktop");
    if std::path::Path::new(&xdg).exists() {
        return true;
    }
    // Fall back to systemd user service check.
    std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", "argus-lasso.service"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
        .unwrap_or(false)
}

fn write_autostart() -> std::io::Result<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("argus-lasso"));

    // ── XDG autostart (works on GNOME, KDE, XFCE, and most other DEs) ────────
    let xdg_dir = format!("{home}/.config/autostart");
    std::fs::create_dir_all(&xdg_dir)?;
    let xdg_entry = format!(
        "[Desktop Entry]\nType=Application\nName=Argus-Lasso\n\
         Exec={} --minimized\nIcon=argus-lasso\nHidden=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(format!("{xdg_dir}/argus-lasso.desktop"), xdg_entry)?;

    // ── systemd user service (KDE / systemd-based desktops) ──────────────────
    let systemd_dir = format!("{home}/.config/systemd/user");
    let mut systemd_note = String::new();
    if std::fs::create_dir_all(&systemd_dir).is_ok() {
        let unit = format!(
            "[Unit]\nDescription=Argus-Lasso Linux\nAfter=graphical-session.target\n\n\
             [Service]\nExecStart={} --minimized\nRestart=on-failure\n\n\
             [Install]\nWantedBy=graphical-session.target\n",
            exe.display()
        );
        match std::fs::write(format!("{systemd_dir}/argus-lasso.service"), unit) {
            Ok(()) => {
                if std::process::Command::new("systemctl")
                    .args(["--user", "enable", "argus-lasso.service"])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    systemd_note = " + systemd".into();
                } else {
                    systemd_note = " (systemd unit written but not enabled)".into();
                }
            }
            Err(e) => systemd_note = format!(" (systemd unit not written: {e})"),
        }
    }

    Ok(format!("Autostart enabled (XDG{systemd_note})"))
}

fn disable_autostart() -> std::io::Result<String> {
    let home = std::env::var("HOME").unwrap_or_default();

    // Remove XDG autostart entry (best-effort: it may not exist).
    let xdg = format!("{home}/.config/autostart/argus-lasso.desktop");
    let _ = std::fs::remove_file(&xdg);

    // Disable systemd unit if present.
    let systemd_ok = std::process::Command::new("systemctl")
        .args(["--user", "disable", "argus-lasso.service"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let systemd_note = if systemd_ok {
        " + systemd"
    } else {
        " (systemd unit still enabled)"
    };

    Ok(format!("Autostart disabled (XDG{systemd_note})"))
}
