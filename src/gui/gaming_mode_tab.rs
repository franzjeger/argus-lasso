//! Gaming Mode tab: CPU topology display, parking, game launcher, profiles.

use egui::{Color32, RichText, Ui};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, GamingProfile};
use crate::cpu_park::{
    self, detect_topology, get_smt_siblings_of, is_helper_authorized, is_helper_current,
    is_helper_installed, park_cpus, unpark_all, CpuTopology,
};
use crate::utils::{get_offline_cpus, get_online_cpus};

// ── Events emitted from this tab ──────────────────────────────────────────────

pub enum GamingEvent {
    GamingModeChanged { active: bool, elevate_nice: bool },
    ResetAll,
    GameLaunched { pid: u32, profile: String },
    LogMessage(String),
    ConfigChanged(Box<Config>),
}

// ── Launcher watch phase ──────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub(crate) enum WatchPhase {
    Idle,
    Waiting,
    Running,
}

// ── GamingModeTab ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum GamingSection {
    #[default]
    Cpu,
    Launcher,
    Overlay,
    Recording,
    Sensors,
}

pub struct GamingModeTab {
    pub section: GamingSection,
    overlay_install_status: String,
    pub config: Config,
    pub topo: Option<CpuTopology>,
    pub topo_description: String,
    pub parked: bool,
    pub parking_in_progress: bool,

    // Preferred CCD checkbox grid: cpu_num → checked
    pub preferred_checks: HashMap<u32, bool>,
    pub smt_siblings: HashSet<u32>,

    // Helper status
    pub helper_status_text: String,
    pub helper_ok: bool,
    /// Helper installed with a working sudoers rule but content is outdated.
    pub helper_outdated: bool,

    // Nice elevation
    pub elevate_nice: bool,

    // CPU status line (None = use theme default text color)
    pub cpu_status_text: String,
    pub cpu_status_color: Option<Color32>,

    // Log (local to tab)
    pub log_lines: Vec<String>,

    // Game launcher
    pub game_name: String,
    pub command: String,
    pub auto_restore: bool,
    pub watch_phase: WatchPhase,
    pub launched_pid: Option<u32>,
    pub watch_status: String,
    pub last_poll: std::time::Instant,

    // Profiles
    pub selected_profile: String,

    // Dialogs
    pub show_install_dialog: bool,
    /// "current: <governor> / <epp>" display next to the power-profile buttons
    power_status_text: String,
    /// Current scaling governor, used to preselect the power-profile control.
    power_governor: String,
    /// Result channel for the background helper-install thread — the polkit
    /// auth dialog can stay open for minutes, so installing synchronously
    /// would freeze the whole UI.
    install_result_rx: Option<std::sync::mpsc::Receiver<String>>,
    steam_picker: Option<crate::gui::dialogs::SteamGamePickerDialog>,
    lutris_picker: Option<crate::gui::dialogs::LutrisGamePickerDialog>,

    // Pending re-enable after unpark (profile switch)
    pending_enable_after_unpark: bool,
    /// Expansion state of the two panels behind the footer buttons.
    overlay_settings_open: bool,
    sensor_access: crate::sensor_access::SensorAccess,
    benchmark: crate::game_benchmark::GameBenchmark,

    // Events to emit to app.rs
    pub events: Vec<GamingEvent>,
}

impl GamingModeTab {
    pub fn new(mut config: Config) -> Self {
        if let Ok(global) = crate::gui::overlay_install::is_global() {
            config.ui.global_overlay = global;
        }
        let topo = detect_topology();
        let topo_description = topo.description.clone();
        let offline = get_offline_cpus();

        let all_cpus: HashSet<u32> = topo.preferred.iter().copied().collect();
        let smt_siblings = get_smt_siblings_of(&all_cpus);

        let mut preferred_checks: HashMap<u32, bool> = HashMap::new();
        for &cpu in &topo.preferred {
            preferred_checks.insert(cpu, !offline.contains(&cpu));
        }

        let parked = !offline.is_empty();
        let helper_ok = is_helper_current() && is_helper_authorized();

        let mut tab = Self {
            config,
            topo: Some(topo),
            topo_description,
            parked,
            parking_in_progress: false,
            preferred_checks,
            smt_siblings,
            helper_status_text: String::new(),
            helper_ok,
            helper_outdated: false,
            elevate_nice: true,
            cpu_status_text: String::new(),
            cpu_status_color: None,
            log_lines: Vec::new(),
            game_name: String::new(),
            command: String::new(),
            auto_restore: true,
            watch_phase: WatchPhase::Idle,
            launched_pid: None,
            watch_status: String::new(),
            last_poll: std::time::Instant::now(),
            selected_profile: String::new(),
            show_install_dialog: false,
            power_status_text: String::new(),
            power_governor: String::new(),
            install_result_rx: None,
            steam_picker: None,
            lutris_picker: None,
            pending_enable_after_unpark: false,
            section: GamingSection::default(),
            overlay_install_status: String::new(),
            overlay_settings_open: false,
            sensor_access: Default::default(),
            benchmark: Default::default(),
            events: Vec::new(),
        };
        tab.refresh_helper_status();
        tab.refresh_cpu_status();
        tab.refresh_power_status();

        if parked {
            tab.events.push(GamingEvent::GamingModeChanged {
                active: true,
                elevate_nice: tab.elevate_nice,
            });
        }

        tab
    }

    fn refresh_helper_status(&mut self) {
        let sudoers_ok = is_helper_authorized();
        self.helper_ok = is_helper_current() && sudoers_ok;
        self.helper_outdated = !self.helper_ok && is_helper_installed() && sudoers_ok;
        self.helper_status_text = if self.helper_ok {
            "Helper installed — parking + nice -1 available".into()
        } else if self.helper_outdated {
            "Helper needs update — click 'Install / Update Helper'".into()
        } else {
            "Helper not installed — click 'Install / Update Helper' to enable parking".into()
        };
    }

    fn refresh_power_status(&mut self) {
        let gov = cpu_park::current_governor().unwrap_or_else(|| "?".into());
        self.power_status_text = match cpu_park::current_epp() {
            Some(epp) => format!("current: {gov} / {epp}"),
            None => format!("current: {gov}"),
        };
        self.power_governor = gov;
    }

    fn refresh_cpu_status(&mut self) {
        let online = get_online_cpus()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let offline = get_offline_cpus()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let total = online.len() + offline.len();
        if offline.is_empty() {
            self.cpu_status_text = format!("All {total} CPUs online");
            self.cpu_status_color = None; // theme default text color
        } else {
            self.cpu_status_text = format!(
                "{} of {total} CPUs online · parked: {}",
                online.len(),
                offline
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            self.cpu_status_color = Some(crate::gui::theme::Breeze::WARNING);
        }
    }

    fn enable_gaming_mode(&mut self) {
        if let Some(ref topo) = self.topo.clone() {
            if !topo.has_asymmetry() {
                return;
            }
            if !is_helper_installed() {
                self.append_log("[Gaming Mode] Helper missing — install first.".into());
                return;
            }
            let unchecked: HashSet<u32> = self
                .preferred_checks
                .iter()
                .filter(|(_, &checked)| !checked)
                .map(|(&cpu, _)| cpu)
                .collect();
            let to_park: HashSet<u32> = topo
                .non_preferred
                .iter()
                .copied()
                .chain(unchecked)
                .collect();
            self.append_log(format!("[Gaming Mode] Parking CPUs {:?}…", {
                let mut v: Vec<_> = to_park.iter().copied().collect();
                v.sort_unstable();
                v
            }));
            self.parking_in_progress = true;

            // Park synchronously (blocking — parking is fast, sub-second)
            let log_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let ll = log_lines.clone();
            let ok = park_cpus(&to_park, move |msg| {
                ll.lock().unwrap().push(msg);
            });
            for msg in log_lines.lock().unwrap().drain(..) {
                self.append_log(msg);
            }
            self.parking_in_progress = false;
            self.parked = ok;
            self.refresh_cpu_status();
            if ok {
                self.append_log("[Gaming Mode] ACTIVE — non-preferred CPUs offline.".into());
                self.events.push(GamingEvent::GamingModeChanged {
                    active: true,
                    elevate_nice: self.elevate_nice,
                });
                self.events
                    .push(GamingEvent::LogMessage("[Gaming Mode] enabled".into()));
            } else {
                self.append_log("[Gaming Mode] Parking failed — check log.".into());
            }
        }
    }

    fn disable_gaming_mode(&mut self) {
        self.append_log("[Gaming Mode] Unparking all CPUs…".into());
        let log_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let ll = log_lines.clone();
        let _ok = unpark_all(move |msg| {
            ll.lock().unwrap().push(msg);
        });
        for msg in log_lines.lock().unwrap().drain(..) {
            self.append_log(msg);
        }
        self.parked = false;
        self.refresh_cpu_status();
        // Re-detect topology now all CPUs are back online
        let topo = detect_topology();
        self.topo_description = topo.description.clone();
        self.rebuild_preferred_checks(&topo);
        self.topo = Some(topo);
        self.append_log("[Gaming Mode] Disabled — all CPUs online.".into());
        self.events.push(GamingEvent::GamingModeChanged {
            active: false,
            elevate_nice: false,
        });
        self.events
            .push(GamingEvent::LogMessage("[Gaming Mode] disabled".into()));

        if self.pending_enable_after_unpark {
            self.pending_enable_after_unpark = false;
            self.enable_gaming_mode();
        }
    }

    fn rebuild_preferred_checks(&mut self, topo: &CpuTopology) {
        let offline = get_offline_cpus();
        self.smt_siblings = get_smt_siblings_of(&topo.preferred);
        self.preferred_checks.clear();
        for &cpu in &topo.preferred {
            self.preferred_checks.insert(cpu, !offline.contains(&cpu));
        }
    }

    fn append_log(&mut self, msg: String) {
        self.log_lines.push(msg.clone());
        if self.log_lines.len() > 200 {
            self.log_lines.drain(0..self.log_lines.len() - 200);
        }
        self.events.push(GamingEvent::LogMessage(msg));
    }

    pub fn poll_game_process(&mut self) {
        if self.watch_phase == WatchPhase::Idle {
            return;
        }
        if self.last_poll.elapsed().as_secs_f32()
            < if self.watch_phase == WatchPhase::Running {
                5.0
            } else {
                2.0
            }
        {
            return;
        }
        self.last_poll = std::time::Instant::now();

        let pids: Vec<u32> = std::fs::read_dir("/proc")
            .ok()
            .map(|d| {
                d.filter_map(|e| {
                    e.ok()
                        .and_then(|e| e.file_name().to_str().and_then(|s| s.parse().ok()))
                })
                .collect()
            })
            .unwrap_or_default();

        let name = self.game_name.clone();

        if self.watch_phase == WatchPhase::Waiting {
            for &pid in &pids {
                if proc_name_matches(&name, pid) {
                    self.launched_pid = Some(pid);
                    self.events.push(GamingEvent::GameLaunched {
                        pid,
                        profile: self.selected_profile.clone(),
                    });
                    self.watch_phase = WatchPhase::Running;
                    self.watch_status = format!("Game running (PID {pid})");
                    self.append_log(format!("[Launcher] Game process found: PID {pid}"));
                    return;
                }
            }
        } else if self.watch_phase == WatchPhase::Running {
            if let Some(pid) = self.launched_pid {
                if !pids.contains(&pid) {
                    // Check for replacement
                    if let Some(new_pid) =
                        pids.iter().find(|&&p| proc_name_matches(&name, p)).copied()
                    {
                        self.launched_pid = Some(new_pid);
                        self.events.push(GamingEvent::GameLaunched {
                            pid: new_pid,
                            profile: self.selected_profile.clone(),
                        });
                        self.append_log(format!("[Launcher] Game PID changed → {new_pid}"));
                    } else {
                        self.append_log(format!("[Launcher] Game (PID {pid}) exited."));
                        if self.auto_restore && self.parked {
                            self.disable_gaming_mode();
                        }
                        self.watch_phase = WatchPhase::Idle;
                        self.launched_pid = None;
                        self.watch_status = String::new();
                    }
                }
            }
        }
    }

    pub fn overlay_window(&mut self, ctx: &egui::Context, opacity: f32) -> bool {
        crate::gui::overlay_settings::window(
            ctx,
            &mut self.config.gaming_mode.overlay,
            &mut self.overlay_settings_open,
            crate::utils::get_cpu_count(),
            opacity,
        )
    }
    pub fn show(&mut self, ui: &mut Ui, ctx: &egui::Context, opacity: f32) {
        // Do NOT clear events here: app.rs drains them with mem::take AFTER
        // show(), and the constructor may queue a startup GamingModeChanged
        // event (CPUs already parked at launch) that a clear would discard.
        self.poll_game_process();

        // Collect the result of a background helper install, if one finished.
        if let Some(rx) = &self.install_result_rx {
            match rx.try_recv() {
                Ok(msg) => {
                    self.append_log(msg);
                    self.refresh_helper_status();
                    self.install_result_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.install_result_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // keep repainting while we wait for the auth dialog
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            }
        }

        crate::gui::theme::section_nav(
            ui,
            &mut self.section,
            &[
                (GamingSection::Cpu, "CPU & performance"),
                (GamingSection::Launcher, "Launcher & profiles"),
                (GamingSection::Overlay, "Overlay"),
                (GamingSection::Recording, "Recording"),
                (GamingSection::Sensors, "Sensors"),
            ],
        );
        egui::ScrollArea::vertical().id_salt(("gaming_body", self.section as u8)).show(ui, |ui| {
            use crate::gui::theme::{self as th, tokens};
            let s = th::sem(ui);
            let mut reset_clicked = false;

            let has_asym = self
                .topo
                .as_ref()
                .map(|t| t.has_asymmetry())
                .unwrap_or(false);

            if self.section == GamingSection::Cpu {
            // ── Helper banner: the blocking prerequisite gets one clear action
            if !self.helper_ok {
                let color = if self.helper_outdated {
                    s.warning
                } else {
                    s.negative
                };
                if th::banner(
                    ui,
                    color,
                    &self.helper_status_text,
                    Some("Set up CPU control…"),
                ) {
                    self.show_install_dialog = true;
                }
                ui.add_space(tokens::SPACE_S);
            }

            // ── Status hero: state, topology summary, one primary action ──
            crate::gui::theme::card_untitled(ui, |ui| {
                ui.horizontal(|ui| {
                    status_dot(
                        ui,
                        if self.parked {
                            s.ok
                        } else {
                            ui.visuals().weak_text_color()
                        },
                    );
                    ui.add_space(tokens::SPACE_XS);
                    ui.vertical(|ui| {
                        ui.set_max_width((ui.available_width() - 200.0).max(200.0));
                        ui.label(
                            RichText::new(if self.parked {
                                "Gaming Mode is on"
                            } else {
                                "Gaming Mode is off"
                            })
                            .font(th::bold_font(tokens::FONT_HERO))
                            .color(crate::gui::theme::strong_color(ui)),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                self.topo_description, self.cpu_status_text
                            ))
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let enabled = has_asym && self.helper_ok && !self.parking_in_progress;
                        let label = if self.parked {
                            "Disable Gaming Mode"
                        } else {
                            "Enable Gaming Mode"
                        };
                        let btn =
                            egui::Button::new(RichText::new(label).strong().color(s.on_accent))
                                .fill(if self.parked { s.negative } else { s.accent })
                                .min_size(egui::vec2(110.0, 30.0));
                        if ui.add_enabled(enabled, btn).clicked() {
                            if self.parked {
                                self.disable_gaming_mode();
                            } else {
                                self.enable_gaming_mode();
                            }
                        }
                    });
                });
            });
            ui.add_space(tokens::SPACE_S);

            // ── Core map: which cores stay online in Gaming Mode ──────────
            th::card_hinted(
                ui,
                "CPU threads used by Gaming Mode",
                "Select which preferred CPU threads stay online",
                |ui| {
                    let (pref, nonpref, pref_label, nonpref_label) = match &self.topo {
                        Some(t) => (
                            t.preferred.iter().copied().collect::<Vec<u32>>(),
                            t.non_preferred.iter().copied().collect::<Vec<u32>>(),
                            t.preferred_label.clone(),
                            t.non_preferred_label.clone(),
                        ),
                        None => (Vec::new(), Vec::new(), String::new(), String::new()),
                    };

                    if !has_asym {
                        ui.label(
                        RichText::new(
                            "No CPU asymmetry detected — parking is unavailable on this machine.",
                        )
                        .size(tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                    );
                        ui.add_space(tokens::SPACE_S);
                    }

                    core_map(
                        ui,
                        &pref,
                        &nonpref,
                        &self.smt_siblings,
                        &mut self.preferred_checks,
                        has_asym,
                    );

                    ui.add_space(tokens::SPACE_S);
                    ui.horizontal(|ui| {
                        if th::chip(ui, "All threads", false) {
                            for v in self.preferred_checks.values_mut() {
                                *v = true;
                            }
                        }
                        let has_smt = !self.smt_siblings.is_empty();
                        ui.add_enabled_ui(has_smt, |ui| {
                            if th::chip(ui, "One thread per core", false) {
                                for (&cpu, v) in &mut self.preferred_checks {
                                    *v = !self.smt_siblings.contains(&cpu);
                                }
                            }
                        });
                        if th::chip(ui, "Clear selection", false) {
                            for v in self.preferred_checks.values_mut() {
                                *v = false;
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Uniform-topology machines have no non-preferred group,
                            // so that swatch would render with an empty caption.
                            if !nonpref_label.is_empty() {
                                legend_swatch(ui, s.manual, &format!("{nonpref_label} parked"));
                            }
                            if !pref_label.is_empty() {
                                legend_swatch(ui, s.accent, &format!("{pref_label} active"));
                            }
                        });
                    });
                },
            );
            ui.add_space(tokens::SPACE_S);

            // ── Behaviour: what happens when Gaming Mode is on ────────────
            th::card(ui, "Automatic game detection & priority", |ui| {
                ui.checkbox(&mut self.elevate_nice, "Elevate game priority (nice -1)");
                let mut auto_changed = ui
                    .checkbox(
                        &mut self.config.gaming_mode.auto_detect,
                        "Auto-enable when a game is detected (Steam/Proton)",
                    )
                    .changed();
                if self.config.gaming_mode.auto_detect {
                    ui.indent("gm_auto_park", |ui| {
                        auto_changed |= ui
                            .checkbox(
                                &mut self.config.gaming_mode.auto_park,
                                "Also park non-preferred CPUs when auto-enabling",
                            )
                            .changed();
                    });
                }
                if auto_changed {
                    self.events
                        .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
                }

                ui.add_space(tokens::SPACE_S);
                ui.horizontal(|ui| {
                    ui.label("Power profile");
                    ui.add_space(tokens::SPACE_S);
                    use cpu_park::PowerProfile;
                    let profiles = [
                        PowerProfile::Performance,
                        PowerProfile::Balanced,
                        PowerProfile::PowerSave,
                    ];
                    let sel = match self.power_governor.as_str() {
                        "performance" => 0,
                        "powersave" => 2,
                        _ => 1,
                    };
                    ui.add_enabled_ui(self.helper_ok, |ui| {
                        if let Some(i) =
                            th::segmented(ui, &["Performance", "Balanced", "Power save"], sel)
                        {
                            let (_ok, msg) = cpu_park::apply_power_profile(profiles[i]);
                            self.append_log(msg);
                            self.refresh_power_status();
                        }
                    });
                    ui.add_space(tokens::SPACE_S);
                    ui.label(
                        RichText::new(if self.helper_ok {
                            self.power_status_text.clone()
                        } else {
                            "requires the privileged helper".to_string()
                        })
                        .size(tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                    );
                });
            });
            ui.add_space(tokens::SPACE_S);

            ui.add_space(tokens::SPACE_S);
            if ui.button("Restore all CPU assignments").on_hover_text("Restores every process CPU affinity and brings all CPUs online.").clicked() {
                reset_clicked = true;
            }
            egui::CollapsingHeader::new("Gaming activity").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &self.log_lines { ui.monospace(line); }
                });
            });
            }
            if self.section == GamingSection::Launcher {
                th::card(ui, "Game launcher and profiles", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Profile");
                        let profiles = self
                            .config
                            .gaming_mode
                            .profiles
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>();
                        egui::ComboBox::from_id_salt("profile_combo")
                            .selected_text(if self.selected_profile.is_empty() {
                                "—"
                            } else {
                                &self.selected_profile
                            })
                            .show_ui(ui, |ui| {
                                for name in &profiles {
                                    if ui
                                        .selectable_label(*name == self.selected_profile, name)
                                        .clicked()
                                    {
                                        self.selected_profile = name.clone();
                                        self.load_profile(name);
                                    }
                                }
                            });
                        if ui.button("Save profile").clicked() {
                            self.save_profile();
                        }
                        if ui
                            .add_enabled(
                                !self.selected_profile.is_empty(),
                                egui::Button::new("Delete profile"),
                            )
                            .clicked()
                        {
                            let name = self.selected_profile.clone();
                            self.config.gaming_mode.profiles.remove(&name);
                            self.selected_profile.clear();
                            self.events
                                .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
                        }
                    });

                    // The two launcher fields sat on bare horizontal rows, so
                    // their inputs started at different x — the same label
                    // grid the rest of the app uses lines them up.
                    let lw = tokens::FORM_LABEL_W;
                    th::form_row_w(ui, lw, "Game", |ui| {
                        ui.text_edit_singleline(&mut self.game_name);
                        if ui.button("Steam…").clicked() {
                            self.steam_picker =
                                Some(crate::gui::dialogs::SteamGamePickerDialog::new());
                        }
                        if ui.button("Lutris…").clicked() {
                            self.lutris_picker =
                                Some(crate::gui::dialogs::LutrisGamePickerDialog::new());
                        }
                    });

                    th::form_row_w(ui, lw, "Command", |ui| {
                        ui.text_edit_singleline(&mut self.command);
                    });

                    ui.horizontal(|ui| {
                        let can_launch = !self.game_name.is_empty() && !self.command.is_empty();
                        let launch =
                            egui::Button::new(RichText::new("Launch").strong().color(s.on_accent))
                                .fill(s.accent);
                        if ui.add_enabled(can_launch, launch).clicked() {
                            self.launch_game();
                        }
                        let can_kill = self.watch_phase != WatchPhase::Idle;
                        if ui
                            .add_enabled(can_kill, egui::Button::new("Force quit game"))
                            .clicked()
                        {
                            if let Some(pid) = self.launched_pid {
                                match nix::sys::signal::kill(
                                    nix::unistd::Pid::from_raw(pid as i32),
                                    nix::sys::signal::Signal::SIGTERM,
                                ) {
                                    Ok(()) => self.append_log(format!(
                                        "[Launcher] Sent SIGTERM to PID {pid}"
                                    )),
                                    Err(e) => self.append_log(format!(
                                        "[Launcher] SIGTERM to PID {pid} failed: {e}"
                                    )),
                                }
                            }
                            if self.auto_restore && self.parked {
                                self.disable_gaming_mode();
                            }
                            self.watch_phase = WatchPhase::Idle;
                            self.launched_pid = None;
                            self.watch_status = String::new();
                        }
                        ui.checkbox(&mut self.auto_restore, "Disable Gaming Mode when the game exits");
                        if !self.watch_status.is_empty() {
                            ui.colored_label(s.ok, &self.watch_status);
                        }
                    });
                });
            }

            // ── Overlay switch and Gaming customization submenu ───────────
            if self.section == GamingSection::Recording {
                th::card_untitled(ui, |ui| self.benchmark.show(ui));
            }
            if self.section == GamingSection::Sensors {
                th::card_untitled(ui, |ui| self.sensor_access.show(ui));
            }
            if self.section == GamingSection::Overlay {
                let mut overlay_changed = false;
                th::card(ui, "In-game overlay", |ui| {
                    overlay_changed = crate::gui::overlay_settings::show(ui, &mut self.config.gaming_mode.overlay, &mut self.overlay_settings_open, crate::utils::get_cpu_count());
                    ui.separator();
                    let mut global = self.config.ui.global_overlay;
                    if ui.checkbox(&mut global, "Load in all Vulkan games").on_hover_text("Applies to games started after this change. Individual games can use ARGUS_LASSO_HUD_DISABLE=1.").changed() {
                        match crate::gui::overlay_install::set_global(global) {
                            Ok(()) => { self.config.ui.global_overlay = global; overlay_changed = true; self.overlay_install_status = "Saved. Restart running games to change layer loading.".into(); }
                            Err(e) => self.overlay_install_status = format!("Could not change layer loading: {e}"),
                        }
                    }
                    if !self.overlay_install_status.is_empty() { ui.label(&self.overlay_install_status); }
                });
                if overlay_changed { self.events.push(GamingEvent::ConfigChanged(Box::new(self.config.clone()))); }
            }

            // ── Footer: helper status ─────────────────────────────────────
            if self.section == GamingSection::Cpu && self.helper_ok {
                ui.add_space(tokens::SPACE_S);
                ui.horizontal(|ui| {
                    // A whole line in accent-adjacent green reads as a link.
                    // The dot carries the "good" state; the text stays normal
                    // body colour so nothing here looks clickable but the
                    // button.
                    ui.colored_label(s.ok, "●");
                    ui.label(&self.helper_status_text);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("Reinstall helper…")
                            .on_hover_text("Reinstall or update the privileged sysfs helper")
                            .clicked()
                        {
                            self.show_install_dialog = true;
                        }
                    });
                });
            }

            if reset_clicked {
                if self.parked {
                    self.events.push(GamingEvent::GamingModeChanged {
                        active: false,
                        elevate_nice: false,
                    });
                    self.parked = false;
                }
                if !get_offline_cpus().is_empty() {
                    self.append_log("[Reset] Unparking CPUs…".into());
                    let ll = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
                    let l2 = ll.clone();
                    unpark_all(move |m| l2.lock().unwrap().push(m));
                    for m in ll.lock().unwrap().drain(..) {
                        self.append_log(m);
                    }
                    self.refresh_cpu_status();
                    let topo = detect_topology();
                    self.rebuild_preferred_checks(&topo);
                    self.topo_description = topo.description.clone();
                    self.topo = Some(topo);
                }
                self.events.push(GamingEvent::ResetAll);
            }
        });

        // ── Install helper dialog ─────────────────────────────────────────
        if self.show_install_dialog {
            egui::Window::new("Set up CPU control")
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    let pkexec = cpu_park::is_pkexec_available();
                    ui.label(
                        "Argus-Lasso installs three small root-owned helpers — CPU parking, \
                         power profile, and process priority — each authorised separately \
                         through the system's polkit policy.",
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Priority changes are restricted to your own processes.",
                        )
                        .weak(),
                    );
                    if cpu_park::legacy_install_present() {
                        ui.add_space(6.0);
                        ui.colored_label(
                            crate::gui::theme::Breeze::WARNING,
                            "This replaces the older single helper and its passwordless \
                             sudoers rule, which granted root for every operation. Both \
                             are removed during install.",
                        );
                    }
                    if !pkexec {
                        ui.add_space(6.0);
                        ui.colored_label(
                            crate::gui::theme::Breeze::WARNING,
                            "pkexec was not found. Install polkit first — without it the \
                             helpers cannot be authorised.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let installing = self.install_result_rx.is_some();
                        if ui
                            .add_enabled(
                                pkexec && !installing,
                                egui::Button::new("Install (system authentication)"),
                            )
                            .clicked()
                        {
                            self.show_install_dialog = false;
                            self.append_log("Installing privileged helpers via pkexec…".into());
                            let (tx, rx) = std::sync::mpsc::channel();
                            self.install_result_rx = Some(rx);
                            std::thread::spawn(move || {
                                let (_ok, msg) = cpu_park::install_helper_via_pkexec();
                                let _ = tx.send(msg);
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_install_dialog = false;
                        }
                    });
                });
        }

        // ── Steam picker ──────────────────────────────────────────────────
        if let Some(ref mut picker) = self.steam_picker {
            if let Some(result) = picker.show(ctx, opacity) {
                if let Some((appid, name)) = result {
                    self.game_name = name;
                    self.command = format!("steam -applaunch {appid}");
                }
                self.steam_picker = None;
            }
        }

        // ── Lutris picker ─────────────────────────────────────────────────
        if let Some(ref mut picker) = self.lutris_picker {
            if let Some(result) = picker.show(ctx, opacity) {
                if let Some((slug, name)) = result {
                    self.game_name = name;
                    self.command = format!("lutris lutris:rungame/{slug}");
                }
                self.lutris_picker = None;
            }
        }
    }

    fn load_profile(&mut self, name: &str) {
        if let Some(profile) = self.config.gaming_mode.profiles.get(name).cloned() {
            self.game_name = profile.game_name.clone();
            self.command = profile.command.clone();
            self.elevate_nice = profile.elevate_nice;
            for (&cpu, checked) in &mut self.preferred_checks {
                if let Some(&v) = profile.cpu_states.get(&cpu.to_string()) {
                    *checked = v;
                }
            }
            self.append_log(format!("[Profile] Loaded '{name}' — {}", profile.command));
            if self.parked {
                self.append_log(format!("[Profile] Re-applying CPU parking for '{name}'…"));
                self.disable_gaming_mode();
                self.pending_enable_after_unpark = true;
            }
        }
    }

    fn save_profile(&mut self) {
        let name = if self.selected_profile.is_empty() {
            self.game_name.clone()
        } else {
            self.selected_profile.clone()
        };
        if name.is_empty() {
            return;
        }

        let cpu_states: HashMap<String, bool> = self
            .preferred_checks
            .iter()
            .map(|(&k, &v)| (k.to_string(), v))
            .collect();

        self.config.gaming_mode.profiles.insert(
            name.clone(),
            GamingProfile {
                game_name: self.game_name.clone(),
                command: self.command.clone(),
                cpu_states,
                elevate_nice: self.elevate_nice,
            },
        );
        self.selected_profile = name.clone();
        self.events
            .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
        self.append_log(format!("[Profile] Saved '{name}'"));
    }

    fn launch_game(&mut self) {
        if !self.parked {
            self.enable_gaming_mode();
        }

        let cmd = self.command.clone();
        self.append_log(format!("[Launcher] Launching '{}': {cmd}", self.game_name));
        self.watch_phase = WatchPhase::Waiting;
        self.watch_status = "Waiting for game process…".into();
        self.last_poll = std::time::Instant::now();

        // Spawn detached
        let parts: Vec<_> = cmd.split_whitespace().collect();
        if let Some((prog, args)) = parts.split_first() {
            let _ = std::process::Command::new(prog)
                .args(args)
                .env("ARGUS_LASSO_HUD", "1")
                .spawn();
        }
    }
}

/// Small filled circle used as an on/off state indicator.
fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// Colour swatch + caption, used under the core map.
fn legend_swatch(ui: &mut Ui, color: Color32, label: &str) {
    use crate::gui::theme::tokens;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(2), color);
    ui.label(
        RichText::new(label)
            .size(tokens::FONT_SMALL)
            .color(ui.visuals().weak_text_color()),
    );
}

/// Grid of clickable core cells. Preferred cores toggle between "kept online"
/// and "parked by you"; non-preferred cores are always parked in Gaming Mode
/// and are shown for context only.
fn core_map(
    ui: &mut Ui,
    preferred: &[u32],
    non_preferred: &[u32],
    smt: &HashSet<u32>,
    checks: &mut HashMap<u32, bool>,
    interactive: bool,
) {
    use crate::gui::theme::{self as th, tokens};
    const CELL: f32 = 34.0;
    const GAP: f32 = 4.0;
    const COLS: usize = 16;

    let s = th::sem(ui);
    let mut cells: Vec<(u32, bool)> = preferred
        .iter()
        .map(|&c| (c, true))
        .chain(non_preferred.iter().map(|&c| (c, false)))
        .collect();
    cells.sort_unstable();
    if cells.is_empty() {
        return;
    }

    let rows = cells.len().div_ceil(COLS);
    let cols = cells.len().min(COLS);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            cols as f32 * (CELL + GAP) - GAP,
            rows as f32 * (CELL + GAP) - GAP,
        ),
        egui::Sense::hover(),
    );

    for (i, &(cpu, is_pref)) in cells.iter().enumerate() {
        let (r, c) = (i / COLS, i % COLS);
        let cell = egui::Rect::from_min_size(
            rect.min + egui::vec2(c as f32 * (CELL + GAP), r as f32 * (CELL + GAP)),
            egui::vec2(CELL, CELL),
        );
        let kept = is_pref && checks.get(&cpu).copied().unwrap_or(true);
        let clickable = interactive && is_pref;
        let resp = ui.interact(
            cell,
            ui.id().with(("core", cpu)),
            if clickable {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        if resp.clicked() {
            let v = checks.entry(cpu).or_insert(true);
            *v = !*v;
        }

        let (fill, stroke) = if !is_pref {
            (th::tint(s.manual, 26), egui::Stroke::NONE)
        } else if kept {
            (th::tint(s.accent, 38), egui::Stroke::new(1.0_f32, s.accent))
        } else {
            (
                Color32::TRANSPARENT,
                egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
            )
        };
        let radius = egui::CornerRadius::same(4);
        ui.painter().rect_filled(cell, radius, fill);
        if stroke != egui::Stroke::NONE {
            ui.painter()
                .rect_stroke(cell, radius, stroke, egui::StrokeKind::Inside);
        }
        if resp.hovered() && clickable {
            ui.painter()
                .rect_filled(cell, radius, th::tint(ui.visuals().text_color(), 20));
        }

        let text_col = if kept || !is_pref {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        ui.painter().text(
            cell.center() - egui::vec2(0.0, 5.0),
            egui::Align2::CENTER_CENTER,
            cpu.to_string(),
            th::num_font(tokens::FONT_LABEL),
            text_col,
        );
        let tag = if smt.contains(&cpu) { "SMT" } else { "CPU" };
        ui.painter().text(
            cell.center() + egui::vec2(0.0, 7.0),
            egui::Align2::CENTER_CENTER,
            tag,
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );

        let state = if !is_pref {
            "parked in Gaming Mode"
        } else if kept {
            "kept online"
        } else {
            "parked by you"
        };
        resp.on_hover_text(format!("CPU {cpu} — {state}"));
    }
}

fn proc_name_matches(game_name: &str, pid: u32) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let name_n = norm(game_name);
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let comm_n = norm(comm.trim());
    if name_n.contains(&comm_n) || comm_n.contains(&name_n) {
        return true;
    }
    // Fallback: cmdline
    if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
        if norm(&cmdline).contains(&name_n) {
            return true;
        }
    }
    false
}
