//! Gaming Mode tab: CPU topology display, parking, game launcher, profiles.

use egui::{Color32, RichText, Ui};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, GamingProfile};
use crate::cpu_park::{
    self, detect_topology, get_smt_siblings_of, is_helper_authorized, is_helper_current,
    is_helper_installed, CpuTopology,
};
use crate::monitor::Parking;
use crate::utils::{get_offline_cpus, get_online_cpus};

// ── Events emitted from this tab ──────────────────────────────────────────────

pub enum GamingEvent {
    GamingModeChanged {
        active: bool,
        elevate_nice: bool,
        parking: Parking,
    },
    ResetAll,
    GameLaunched {
        pid: u32,
        profile: String,
    },
    LogMessage(String),
    ConfigChanged(Box<Config>),
}

// ── Launcher watch phase ──────────────────────────────────────────────────────

/// The game process the launcher is watching, held through a pidfd: a PID
/// recycled after the game exits can never be mistaken for it, or be sent
/// its "Force quit".
pub(crate) struct LaunchedGame {
    pub pid: u32,
    /// Start time in clock ticks, which orders candidates (see `find`).
    start: u64,
    handle: crate::process_control::ProcessHandle,
}

impl LaunchedGame {
    /// None if the process is already gone — including a zombie, which has
    /// exited but not been reaped, and whose /proc entry would otherwise be
    /// taken for a running game — or started before `not_before` (process
    /// start ticks): a game is never older than its own launch.
    fn open(pid: u32, not_before: u64) -> Option<Self> {
        let stat = crate::fast_proc::read_stat(pid, &mut [0; 1024])?;
        if stat.state == b'Z' || stat.starttime < not_before {
            return None;
        }
        let handle = crate::process_control::ProcessHandle::open(pid, stat.starttime).ok()?;
        Some(Self {
            pid,
            start: stat.starttime,
            handle,
        })
    }

    /// The game called `name`, started since `not_before`. Several processes
    /// can carry the name (a stub and the game, a crash handler); the one
    /// started first is taken, ties by PID, so which one is watched does not
    /// depend on the unspecified order of /proc.
    fn find(name: &str, not_before: u64) -> Option<Self> {
        std::fs::read_dir("/proc")
            .ok()?
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| proc_name_matches(name, pid))
            .filter_map(|pid| Self::open(pid, not_before))
            .min_by_key(|game| (game.start, game.pid))
    }
}

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
    /// The shared overlay visibility as this tab last took it (see
    /// `follow_overlay_shown`).
    overlay_shown_seen: bool,
    pub topo: Option<CpuTopology>,
    pub topo_description: String,
    /// Mirrors the daemon, which owns Gaming Mode (see `sync_gaming_state`).
    pub parked: bool,
    /// The state asked of the daemon, until it reports the request handled.
    gaming_request: Option<bool>,
    /// The daemon's change counter as last seen.
    seen_gaming_changes: u64,

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
    pub(crate) launched: Option<LaunchedGame>,
    /// Start ticks of the process the launcher spawned. The game is that
    /// process or one started after it.
    launch_start_ticks: u64,
    pub watch_status: String,
    launch_error: bool,
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
    /// A power profile change in progress; pkexec runs off the UI thread.
    power_profile_rx: Option<std::sync::mpsc::Receiver<String>>,
    /// When the governor/EPP shown was read; Settings can change them too.
    power_read_at: std::time::Instant,
    steam_picker: Option<crate::gui::dialogs::SteamGamePickerDialog>,
    lutris_picker: Option<crate::gui::dialogs::LutrisGamePickerDialog>,

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
            overlay_shown_seen: config.gaming_mode.overlay.show_overlay,
            config,
            topo: Some(topo),
            topo_description,
            parked,
            gaming_request: None,
            seen_gaming_changes: 0,
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
            launched: None,
            launch_start_ticks: 0,
            watch_status: String::new(),
            launch_error: false,
            last_poll: std::time::Instant::now(),
            selected_profile: String::new(),
            show_install_dialog: false,
            power_status_text: String::new(),
            power_governor: String::new(),
            install_result_rx: None,
            power_profile_rx: None,
            power_read_at: std::time::Instant::now(),
            steam_picker: None,
            lutris_picker: None,
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

        // CPUs already parked at start (e.g. after a crash) are adopted as
        // they are.
        if parked {
            tab.request_gaming(true, Parking::Keep);
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
            "CPU control needs an update. Use Set up CPU control to continue.".into()
        } else {
            "Set up CPU control to enable core parking and priority changes.".into()
        };
    }

    fn refresh_power_status(&mut self) {
        self.power_read_at = std::time::Instant::now();
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

    /// Ask the daemon, which owns Gaming Mode, for a change. It parks on its
    /// own thread; `sync_gaming_state` picks up the outcome.
    fn request_gaming(&mut self, active: bool, parking: Parking) {
        self.gaming_request = Some(active);
        self.events.push(GamingEvent::GamingModeChanged {
            active,
            elevate_nice: active && self.elevate_nice,
            parking,
        });
    }

    /// Mirror the daemon's Gaming Mode state, called every frame.
    pub fn sync_gaming_state(&mut self, active: bool, changes: u64) {
        if changes == self.seen_gaming_changes {
            return;
        }
        self.seen_gaming_changes = changes;
        let requested = self.gaming_request.take();
        let was_active = std::mem::replace(&mut self.parked, active);
        if requested == Some(true) && !active {
            self.append_log("[Gaming Mode] Could not be enabled — see the log.".into());
        } else if was_active != active {
            self.append_log(format!(
                "[Gaming Mode] {}",
                if active { "Enabled" } else { "Disabled" }
            ));
        }
        self.refresh_cpu_status();
        if !active {
            // Re-detect topology now all CPUs are back online
            let topo = detect_topology();
            self.topo_description = topo.description.clone();
            self.rebuild_preferred_checks(&topo);
            self.topo = Some(topo);
        }
    }

    /// The CPUs Gaming Mode parks: the non-preferred ones plus any preferred
    /// ones the user unchecked.
    fn cpus_to_park(&self) -> Option<HashSet<u32>> {
        let topo = self.topo.as_ref().filter(|t| t.has_asymmetry())?;
        let unchecked = self
            .preferred_checks
            .iter()
            .filter(|(_, &checked)| !checked)
            .map(|(&cpu, _)| cpu);
        Some(
            topo.non_preferred
                .iter()
                .copied()
                .chain(unchecked)
                .collect(),
        )
    }

    /// Returns whether a request was sent.
    fn enable_gaming_mode(&mut self) -> bool {
        let Some(to_park) = self.cpus_to_park() else {
            return false;
        };
        if !is_helper_installed() {
            self.append_log("[Gaming Mode] Helper missing — install first.".into());
            return false;
        }
        let mut sorted: Vec<_> = to_park.iter().copied().collect();
        sorted.sort_unstable();
        self.append_log(format!("[Gaming Mode] Parking CPUs {sorted:?}…"));
        self.request_gaming(true, Parking::Exactly(to_park));
        true
    }

    fn disable_gaming_mode(&mut self) {
        self.append_log("[Gaming Mode] Unparking all CPUs…".into());
        self.request_gaming(false, Parking::Keep);
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

        let name = self.game_name.clone();
        let not_before = self.launch_start_ticks;
        let find_game = || LaunchedGame::find(&name, not_before);

        match self.watch_phase {
            WatchPhase::Idle => {}
            WatchPhase::Waiting => {
                if let Some(game) = find_game() {
                    let pid = game.pid;
                    self.launched = Some(game);
                    self.events.push(GamingEvent::GameLaunched {
                        pid,
                        profile: self.selected_profile.clone(),
                    });
                    self.watch_phase = WatchPhase::Running;
                    self.watch_status = format!("Game running (PID {pid})");
                    self.append_log(format!("[Launcher] Game process found: PID {pid}"));
                }
            }
            WatchPhase::Running => {
                let Some(old_pid) = self
                    .launched
                    .as_ref()
                    .filter(|g| g.handle.has_exited())
                    .map(|g| g.pid)
                else {
                    return;
                };
                // Check for replacement
                if let Some(game) = find_game() {
                    let pid = game.pid;
                    self.launched = Some(game);
                    self.events.push(GamingEvent::GameLaunched {
                        pid,
                        profile: self.selected_profile.clone(),
                    });
                    self.append_log(format!("[Launcher] Game PID changed → {pid}"));
                } else {
                    self.append_log(format!("[Launcher] Game (PID {old_pid}) exited."));
                    if self.auto_restore && self.parked {
                        self.disable_gaming_mode();
                    }
                    self.watch_phase = WatchPhase::Idle;
                    self.launched = None;
                    self.watch_status = String::new();
                }
            }
        }
    }

    /// Overlay visibility also changes outside this tab, from the CLI and
    /// the shortcut. Take the shared value every frame, so the checkbox
    /// shows it and later edits do not carry a stale one.
    pub fn follow_overlay_shown(&mut self, shared: bool) {
        self.config.gaming_mode.overlay.show_overlay = shared;
        self.overlay_shown_seen = shared;
    }

    /// The overlay visibility to store with a config this tab sent: its own
    /// if it changed the value since it last took the shared one, otherwise
    /// the shared value, which may have changed in between.
    pub fn overlay_shown(&self, sent: bool, shared: bool) -> bool {
        if sent != self.overlay_shown_seen {
            sent
        } else {
            shared
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

        if self.power_profile_rx.is_none()
            && self.power_read_at.elapsed() >= std::time::Duration::from_secs(2)
        {
            self.refresh_power_status();
        }
        if let Some(rx) = &self.power_profile_rx {
            match rx.try_recv() {
                Ok(msg) => {
                    self.append_log(msg);
                    self.refresh_power_status();
                    self.power_profile_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.power_profile_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
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
                            let enabled =
                                has_asym && self.helper_ok && self.gaming_request.is_none();
                            let label = if self.parked {
                                "Disable Gaming Mode"
                            } else {
                                "Enable Gaming Mode"
                            };
                            let btn =
                                egui::Button::new(RichText::new(label).font(th::bold_font(tokens::FONT_BODY)).color(s.on_accent))
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
                        let idle = self.power_profile_rx.is_none();
                        ui.add_enabled_ui(self.helper_ok && idle, |ui| {
                            if let Some(i) =
                                th::segmented(ui, &["Performance", "Balanced", "Power save"], sel)
                            {
                                let (tx, rx) = std::sync::mpsc::channel();
                                self.power_profile_rx = Some(rx);
                                let profile = profiles[i];
                                std::thread::spawn(move || {
                                    let _ = tx.send(cpu_park::apply_power_profile(profile).1);
                                });
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
                            egui::Button::new(RichText::new("Launch").font(th::bold_font(tokens::FONT_BODY)).color(s.on_accent))
                                .fill(s.accent);
                        if ui.add_enabled(can_launch, launch).clicked() {
                            self.launch_game();
                        }
                        let can_kill = self.watch_phase != WatchPhase::Idle;
                        if ui
                            .add_enabled(can_kill, egui::Button::new("Force quit game"))
                            .clicked()
                        {
                            if let Some(game) = self.launched.take() {
                                let pid = game.pid;
                                match game.handle.signal(nix::sys::signal::Signal::SIGTERM) {
                                    Ok(()) => self.append_log(format!(
                                        "[Launcher] Sent SIGTERM to PID {pid}"
                                    )),
                                    Err(nix::errno::Errno::ESRCH) => self.append_log(format!(
                                        "[Launcher] Game (PID {pid}) had already exited"
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
                            self.watch_status = String::new();
                        }
                        ui.checkbox(&mut self.auto_restore, "Disable Gaming Mode when the game exits");
                        if !self.watch_status.is_empty() {
                            ui.colored_label(if self.launch_error { s.negative } else { s.ok }, &self.watch_status);
                        }
                    });
                });
            }

            // ── Recording, sensors, and the overlay switch with its
            //    customization submenu ─────────────────────────────────
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
                    if ui.checkbox(&mut global, "Automatically show in detected Vulkan games").on_hover_text("Detects Steam/Proton games. Terminal windows and desktop applications are skipped. Restart applications after changing this setting. Other games can opt in with ARGUS_LASSO_HUD=1; disable an individual game with ARGUS_LASSO_HUD_DISABLE=1.").changed() {
                        match crate::gui::overlay_install::set_global(global) {
                            Ok(()) => { self.config.ui.global_overlay = global; overlay_changed = true; self.overlay_install_status = "Saved. Restart running games to change layer loading.".into(); }
                            Err(e) => self.overlay_install_status = format!("Could not change layer loading: {e}"),
                        }
                    }
                    th::help_text(ui, "For manual activation, add this to the game’s Steam launch options:");
                    ui.monospace("ARGUS_LASSO_HUD=1 %command%");
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
                // Turning Gaming Mode off also brings every parked CPU back
                // online, including ones parked outside Gaming Mode.
                if self.parked || !get_offline_cpus().is_empty() {
                    self.append_log("[Reset] Unparking CPUs…".into());
                    self.request_gaming(false, Parking::Keep);
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
                self.enable_gaming_mode();
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
        self.launch_error = false;
        let parts = match parse_launch_command(&self.command) {
            Ok(parts) => parts,
            Err(error) => {
                self.launch_error = true;
                self.watch_status = error.clone();
                self.append_log(format!("[Launcher] {error}"));
                return;
            }
        };
        let enabled_for_launch = !self.parked && self.enable_gaming_mode();
        match std::process::Command::new(&parts[0])
            .args(&parts[1..])
            .env("ARGUS_LASSO_HUD", "1")
            .spawn()
        {
            Ok(mut child) => {
                // Read before the reaper below can free the PID.
                self.launch_start_ticks = crate::fast_proc::read_stat(child.id(), &mut [0; 1024])
                    .map_or(0, |s| s.starttime);
                // Reap the launcher even when it exits before the actual game.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.append_log(format!(
                    "[Launcher] Launching '{}': {}",
                    self.game_name, self.command
                ));
                self.watch_phase = WatchPhase::Waiting;
                self.watch_status = "Waiting for game process…".into();
                self.last_poll = std::time::Instant::now();
            }
            Err(error) => {
                if enabled_for_launch {
                    self.disable_gaming_mode();
                }
                self.launch_error = true;
                self.watch_status = format!("Could not launch {}: {error}", parts[0]);
                self.append_log(format!("[Launcher] {}", self.watch_status));
            }
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
    const CELL: f32 = 40.0;
    const GAP: f32 = 4.0;
    let max_cols = ((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize;
    let columns = max_cols.clamp(1, 16);

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

    let rows = cells.len().div_ceil(columns);
    let cols = cells.len().min(columns);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            cols as f32 * (CELL + GAP) - GAP,
            rows as f32 * (CELL + GAP) - GAP,
        ),
        egui::Sense::hover(),
    );

    for (i, &(cpu, is_pref)) in cells.iter().enumerate() {
        let (r, c) = (i / columns, i % columns);
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
        if clickable {
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    kept,
                    format!("Keep CPU {cpu} online"),
                )
            });
        }
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
            cell.center() - egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_CENTER,
            cpu.to_string(),
            th::num_font(tokens::FONT_LABEL),
            text_col,
        );
        let tag = if smt.contains(&cpu) { "SMT" } else { "CPU" };
        ui.painter().text(
            cell.center() + egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_CENTER,
            tag,
            egui::FontId::proportional(tokens::FONT_SMALL),
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

/// Launch infrastructure that starts alongside a game and is never the game,
/// although its name or command line often contains the game's: `sh` inside
/// "Dishonored", or a reaper/Proton command line naming the game's path.
/// Normalized, and truncated to 15 bytes as the kernel truncates `comm`.
const LAUNCH_HELPERS: &[&str] = &[
    "sh",
    "bash",
    "dash",
    "zsh",
    "env",
    "python",
    "python3",
    "reaper",
    "steam",
    "steamwebhelper",
    "pressurevessel",
    "pvbwrap",
    "srtbwrap",
    "bwrap",
    "wine",
    "wine64",
    "wineserver",
    "winepreloader",
    "wine64preloade",
    "proton",
    "umurun",
    "gamescope",
    "gamemoderun",
    "mangohud",
    "lutris",
    "heroic",
];

fn proc_name_matches(game_name: &str, pid: u32) -> bool {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    is_game_process(game_name, &comm, &cmdline)
}

/// Whether a process with this `comm` and `cmdline` is the game `game_name`.
fn is_game_process(game_name: &str, comm: &str, cmdline: &[u8]) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let name_n = norm(game_name);
    let comm_n = norm(comm.trim());
    // Every string contains "", so an empty side would match any process —
    // including one that exited between the /proc listing and these reads.
    if name_n.is_empty() || comm_n.is_empty() || LAUNCH_HELPERS.contains(&comm_n.as_str()) {
        return false;
    }
    // `comm` is truncated to 15 bytes, so a long title can contain it. A
    // short one would be found inside almost any title.
    if comm_n.contains(&name_n) || (comm_n.len() >= 4 && name_n.contains(&comm_n)) {
        return true;
    }
    norm(&String::from_utf8_lossy(cmdline)).contains(&name_n)
}

fn parse_launch_command(command: &str) -> Result<Vec<String>, String> {
    let parts =
        shlex::split(command).ok_or("Invalid command: close all quotes and escape sequences.")?;
    if parts.first().is_none_or(String::is_empty) {
        return Err("Enter a program to launch.".into());
    }
    Ok(parts)
}

#[cfg(test)]
mod launcher_tests {
    use super::{is_game_process, parse_launch_command, LaunchedGame};

    /// Runs `sleep` under `name`, which becomes its process name; killed and
    /// reaped when dropped.
    struct Named(std::process::Child);

    impl Named {
        fn spawn(dir: &std::path::Path, name: &str) -> Self {
            use std::os::unix::process::CommandExt;
            let link = dir.join(name);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink("/usr/bin/sleep", &link).unwrap();
            // argv[0] stays "sleep" for a sleep that picks its tool by name.
            let child = std::process::Command::new(&link)
                .arg0("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            // spawn() returns once the child's memory is replaced, which the
            // kernel does before it renames the process: until then /proc
            // still shows the parent's name. It failed on a busy aarch64 CI
            // runner that way.
            let comm = format!("/proc/{}/comm", child.id());
            let renamed = (0..400).any(|_| {
                std::fs::read_to_string(&comm).is_ok_and(|c| c.trim() == name) || {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    false
                }
            });
            assert!(renamed, "{name} never showed its name");
            Self(child)
        }

        fn start(&self) -> u64 {
            crate::fast_proc::read_stat(self.0.id(), &mut [0; 1024])
                .unwrap()
                .starttime
        }
    }

    impl Drop for Named {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// With several processes of the game's name, the one latched onto
    /// depended on the unspecified order of /proc.
    #[test]
    fn the_earliest_started_game_process_is_taken() {
        let dir = std::env::temp_dir().join(format!("argus-latch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!("aglatch{}", std::process::id() % 10_000_000);
        let first = Named::spawn(&dir, &name);
        // Start times are counted in 10 ms clock ticks.
        while Named::spawn(&dir, "aglatchprobe").start() <= first.start() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let second = Named::spawn(&dir, &name);
        assert!(second.start() > first.start());

        assert_eq!(LaunchedGame::find(&name, 0).unwrap().pid, first.0.id());
        let later = LaunchedGame::find(&name, second.start()).unwrap();
        assert_eq!(
            later.pid,
            second.0.id(),
            "one started before the launch is not the game"
        );
        drop((first, second));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An exited process nobody has reaped keeps its /proc entry, and was
    /// taken for a running game.
    #[test]
    fn a_zombie_is_not_a_running_game() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let zombie = (0..400).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            crate::fast_proc::read_stat(pid, &mut [0; 1024]).is_some_and(|s| s.state == b'Z')
        });
        let opened = LaunchedGame::open(pid, 0).is_some();
        child.wait().unwrap();
        assert!(zombie, "the child never became a zombie");
        assert!(!opened);
    }

    #[test]
    fn game_names_match_the_game_and_not_its_launch_helpers() {
        let title = "Shadow of the Tomb Raider";
        let exe = b"Z:\\games\\Shadow of the Tomb Raider\\SOTTR.exe\0";
        assert!(is_game_process(title, "SOTTR.exe", exe));
        // Short names are found inside almost any title.
        assert!(!is_game_process(title, "sh", b"sh\0-c\0true\0"));
        assert!(!is_game_process("Catan", "cat", b"cat\0notes.txt\0"));
        // Wrappers name the game's path but are not the game.
        assert!(!is_game_process(title, "reaper", exe));
        assert!(!is_game_process(title, "pressure-vessel", exe));
        // A vanished process reads back empty.
        assert!(!is_game_process(title, "", b""));
        // comm truncated to 15 bytes still matches the long executable name.
        assert!(is_game_process("Cyberpunk 2077", "Cyberpunk2077.e", b""));
        // A title that extends the executable's name.
        assert!(is_game_process("Factorio Space Age", "factorio", b""));
    }
    #[test]
    fn quoted_arguments_and_paths_are_preserved_without_shell_execution() {
        assert_eq!(
            parse_launch_command(r#""/games/My Game/game" --name 'Player One' "" '$HOME'"#)
                .unwrap(),
            ["/games/My Game/game", "--name", "Player One", "", "$HOME"]
        );
        assert!(parse_launch_command("game 'unterminated").is_err());
        assert!(parse_launch_command("  ").is_err());
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::GamingModeTab;

    /// `argus-lasso toggle-overlay` hid the HUD, then the next change to any
    /// overlay setting sent this tab's stale copy and showed it again.
    #[test]
    fn a_toggle_made_elsewhere_survives_the_next_overlay_edit() {
        let mut config = crate::config::Config::default();
        config.gaming_mode.overlay.show_overlay = true;
        let mut tab = GamingModeTab::new(config);

        // The CLI hides it; the next frame takes that over.
        tab.follow_overlay_shown(false);
        assert!(!tab.config.gaming_mode.overlay.show_overlay);

        // An edit of another setting sends `false` back unchanged, while
        // the shortcut has meanwhile shown it again: the shared value wins.
        assert!(tab.overlay_shown(false, true));
        // The user switching it on in this tab wins over the shared value.
        assert!(tab.overlay_shown(true, false));
    }
}
