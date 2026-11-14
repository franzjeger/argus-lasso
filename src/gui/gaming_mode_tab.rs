//! Gaming Mode tab: CPU topology display, parking, game launcher, profiles.

use egui::{Color32, RichText, Ui};
use std::collections::{HashMap, HashSet};

use crate::config::Config;
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

    /// The game stored under `key` and called `title`, started since
    /// `not_before`. Several processes can be it (a stub and the game, a
    /// crash handler); the one started first is taken, ties by PID, so which
    /// one is watched does not depend on the unspecified order of /proc.
    fn find(key: &str, title: &str, not_before: u64) -> Option<Self> {
        Self::all(key, title, not_before).into_iter().next()
    }

    /// Every process of that game, oldest first.
    fn all(key: &str, title: &str, not_before: u64) -> Vec<Self> {
        let mut games: Vec<Self> = std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            // The cheap check first: few processes are younger than the launch.
            .filter_map(|pid| Self::open(pid, not_before))
            .filter(|game| is_launched_game(key, title, game.pid))
            .collect();
        games.sort_by_key(|game| (game.start, game.pid));
        games
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
    Games,
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
    /// The name being typed on the Games page.
    new_game_name: String,
    /// Choosing a game to add from the Steam library.
    steam_picker: Option<crate::gui::dialogs::SteamGamePickerDialog>,
    lutris_picker: Option<crate::gui::dialogs::LutrisGamePickerDialog>,
    /// Installed Steam games as (appid, name), for the cards' titles; read
    /// when the Games page is first shown.
    steam_library: Option<Vec<(String, String)>>,
    /// Games whose layer is connected now, from the daemon.
    overlay_clients: Vec<crate::monitor::OverlayClient>,
    /// The shared frame-rate cap as last taken, likewise.
    fps_limit_seen: u32,
    /// The cap the checkbox turns on: the last one used, or 60.
    fps_limit_choice: u32,
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
    /// The key of the game "Launch" started and is watching.
    watch_key: Option<String>,
    /// Launching it turned Gaming Mode on, to turn off when it exits.
    enabled_for_launch: bool,
    pub auto_restore: bool,
    pub watch_phase: WatchPhase,
    pub(crate) launched: Option<LaunchedGame>,
    /// Start ticks of the process the launcher spawned. The game is that
    /// process or one started after it.
    launch_start_ticks: u64,
    pub watch_status: String,
    launch_error: bool,
    pub last_poll: std::time::Instant,

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
            new_game_name: String::new(),
            steam_picker: None,
            lutris_picker: None,
            steam_library: None,
            overlay_clients: Vec::new(),
            fps_limit_seen: config.gaming_mode.overlay.fps_limit,
            fps_limit_choice: match config.gaming_mode.overlay.fps_limit {
                0 => 60,
                fps => fps,
            },
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
            watch_key: None,
            enabled_for_launch: false,
            auto_restore: true,
            watch_phase: WatchPhase::Idle,
            launched: None,
            launch_start_ticks: 0,
            watch_status: String::new(),
            launch_error: false,
            last_poll: std::time::Instant::now(),
            show_install_dialog: false,
            power_status_text: String::new(),
            power_governor: String::new(),
            install_result_rx: None,
            power_profile_rx: None,
            power_read_at: std::time::Instant::now(),
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
        self.request_gaming_boosted(active, parking, self.elevate_nice);
    }

    fn request_gaming_boosted(&mut self, active: bool, parking: Parking, boost: bool) {
        self.gaming_request = Some(active);
        self.events.push(GamingEvent::GamingModeChanged {
            active,
            elevate_nice: active && boost,
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

    /// Turn Gaming Mode on for a game "Launch" starts, as its profile has
    /// it. Returns whether a request was sent.
    fn enable_gaming_for(&mut self, game: &crate::config::GameProfile) -> bool {
        let boost = self.elevate_nice && !game.no_boost;
        if game.park == Some(false) {
            self.append_log("[Gaming Mode] On for the launched game, no CPUs parked.".into());
            self.request_gaming_boosted(true, Parking::Keep, boost);
            return true;
        }
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
        self.request_gaming_boosted(true, Parking::Exactly(to_park), boost);
        true
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

        let Some(key) = self.watch_key.clone() else {
            self.watch_phase = WatchPhase::Idle;
            return;
        };
        let title = self.game_label(&key);
        let not_before = self.launch_start_ticks;
        let find_game = || LaunchedGame::find(&key, &title, not_before);

        match self.watch_phase {
            WatchPhase::Idle => {}
            WatchPhase::Waiting => {
                if let Some(game) = find_game() {
                    let pid = game.pid;
                    self.launched = Some(game);
                    self.events.push(GamingEvent::GameLaunched {
                        pid,
                        profile: title.clone(),
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
                        profile: title.clone(),
                    });
                    self.append_log(format!("[Launcher] Game PID changed → {pid}"));
                } else {
                    self.append_log(format!("[Launcher] Game (PID {old_pid}) exited."));
                    self.end_watch();
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

    /// The frame-rate cap also changes from the CLI; see
    /// `follow_overlay_shown`.
    pub fn follow_fps_limit(&mut self, shared: u32) {
        self.config.gaming_mode.overlay.fps_limit = shared;
        self.fps_limit_seen = shared;
        if shared != 0 {
            self.fps_limit_choice = shared;
        }
    }

    /// The cap to store with a config this tab sent; see `overlay_shown`.
    pub fn fps_limit(&self, sent: u32, shared: u32) -> u32 {
        if sent != self.fps_limit_seen {
            sent
        } else {
            shared
        }
    }

    /// The games connected now, as the daemon reports them.
    pub fn follow_overlay_clients(&mut self, clients: &[crate::monitor::OverlayClient]) {
        if self.overlay_clients != clients {
            self.overlay_clients = clients.to_vec();
        }
    }

    /// The Games page: one card per game with what differs for it.
    fn games_section(&mut self, ui: &mut Ui) {
        use crate::gui::theme as th;
        let s = th::sem(ui);
        let mut changed = false;
        th::card(ui, "Games", |ui| {
            th::help_text(
                ui,
                "Your games, each with what differs for it: how it is launched, how Gaming \
                 Mode treats it and what its overlay shows. Choose a game from the Steam or \
                 Lutris library, or name any other by its process name, as the Processes \
                 page shows it. Games showing the overlay are listed while they run.",
            );
            ui.horizontal(|ui| {
                ui.label("Add a game");
                if ui
                    .button("Steam library…")
                    .on_hover_text("Its settings then cover every process of that Steam game.")
                    .clicked()
                {
                    self.steam_picker = Some(crate::gui::dialogs::SteamGamePickerDialog::new());
                }
                if ui.button("Lutris…").clicked() {
                    self.lutris_picker = Some(crate::gui::dialogs::LutrisGamePickerDialog::new());
                }
                ui.label("or");
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut self.new_game_name)
                        .hint_text("process name, e.g. Game.exe")
                        .desired_width(220.0),
                );
                let name = self.new_game_name.trim().to_owned();
                let enter = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui
                    .add_enabled(!name.is_empty(), egui::Button::new("Add"))
                    .clicked()
                    || enter)
                    && !name.is_empty()
                {
                    self.add_game(name, String::new());
                    self.new_game_name.clear();
                }
            });
            ui.checkbox(
                &mut self.auto_restore,
                "Turn Gaming Mode off again when a game launched here exits",
            )
            .on_hover_text("Applies when launching the game is what turned Gaming Mode on.");
        });
        let rows = game_rows(&self.overlay_clients, &self.config.gaming_mode.games);
        let library = self
            .steam_library
            .get_or_insert_with(crate::game_library::steam_games)
            .clone();
        let general_limit = self.config.gaming_mode.overlay.fps_limit;
        let auto_park = self.config.gaming_mode.auto_park;
        let auto_detect = self.config.gaming_mode.auto_detect;
        let idle = self.watch_phase == WatchPhase::Idle;
        let mut launch = None;
        let mut force_quit = false;
        for row in rows {
            let stored = self.config.gaming_mode.games.get(&row.name);
            let is_stored = stored.is_some();
            let mut game = stored.cloned().unwrap_or_default();
            let before = game.clone();
            let mut remove = false;
            let (title, known_as) =
                game_title(&row.name, &game.title, row.process.as_deref(), &library);
            let watched = self.watch_key.as_deref() == Some(&row.name);
            let steam = crate::config::steam_app_of(&row.name).is_some();
            th::card(ui, &title, |ui| {
                if let Some(known_as) = known_as {
                    ui.weak(known_as);
                }
                match &row.status {
                    Some((true, detail)) => ui.label(detail),
                    Some((false, detail)) => ui.colored_label(th::Breeze::WARNING, detail),
                    None => ui.weak("Not showing the overlay now"),
                };
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let command = crate::config::launch_command(&row.name, &game);
                    let button = egui::Button::new(
                        RichText::new("Launch")
                            .font(th::bold_font(th::tokens::FONT_BODY))
                            .color(s.on_accent),
                    )
                    .fill(s.accent);
                    if ui
                        .add_enabled(idle && command.is_some(), button)
                        .on_disabled_hover_text(if idle {
                            "Enter a launch command first."
                        } else {
                            "A launched game is still being watched."
                        })
                        .clicked()
                    {
                        launch = Some(row.name.clone());
                    }
                    let implied = crate::config::launch_command(&row.name, &Default::default());
                    ui.add(
                        egui::TextEdit::singleline(&mut game.command)
                            .hint_text(implied.as_deref().unwrap_or("launch command"))
                            .desired_width(320.0),
                    )
                    .on_hover_text(
                        "What Launch runs. A Steam or Lutris game needs none; one given \
                         here is used instead.",
                    );
                });
                if watched {
                    ui.horizontal(|ui| {
                        if !self.watch_status.is_empty() {
                            ui.colored_label(
                                if self.launch_error { s.negative } else { s.ok },
                                &self.watch_status,
                            );
                        }
                        if !idle && ui.button("Force quit game").clicked() {
                            force_quit = true;
                        }
                    });
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Gaming Mode").strong());
                ui.checkbox(&mut game.no_gaming_mode, "Do not turn on Gaming Mode")
                    .on_hover_text(
                        "For a game that does not need it. Gaming Mode switched on by hand \
                         stays on.",
                    );
                ui.add_enabled_ui(!game.no_gaming_mode, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("CPU parking");
                        let text = |park: Option<bool>| match park {
                            None if auto_park => "As in general (park)",
                            None => "As in general (do not park)",
                            Some(true) => "Park non-preferred CPUs",
                            Some(false) => "Do not park",
                        };
                        egui::ComboBox::from_id_salt(("game_park", &row.name))
                            .selected_text(text(game.park))
                            .show_ui(ui, |ui| {
                                for choice in [None, Some(true), Some(false)] {
                                    ui.selectable_value(&mut game.park, choice, text(choice));
                                }
                            });
                    });
                    let mut boost = !game.no_boost;
                    ui.checkbox(&mut boost, "Raise the priority of game processes");
                    game.no_boost = !boost;
                });
                if !steam || !auto_detect {
                    ui.weak(if steam {
                        "Automatic detection is off: this applies when the game is launched \
                         from here."
                    } else {
                        "Automatic detection only finds Steam and Proton games: for others \
                         this applies when the game is launched from here."
                    });
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Overlay").strong());
                ui.horizontal(|ui| {
                    let mut own = game.fps_limit.is_some();
                    ui.checkbox(&mut own, "Own frame rate limit").on_hover_text(
                        "Instead of the general limit, for this game only. 0 means no limit. \
                         ARGUS_LASSO_FPS_LIMIT in the game's launch options still wins.",
                    );
                    let mut fps = game.fps_limit.unwrap_or(general_limit);
                    ui.add_enabled(
                        own,
                        egui::DragValue::new(&mut fps)
                            // Typing 144 must not cap the game to 1, then 14, on the way.
                            .update_while_editing(false)
                            .range(0..=argus_ipc::FPS_LIMIT_MAX)
                            .suffix(" FPS"),
                    );
                    game.fps_limit = own.then_some(match fps {
                        0 => 0,
                        fps => fps.max(argus_ipc::FPS_LIMIT_MIN),
                    });
                });
                ui.checkbox(&mut game.hide_hud, "Hide HUD");
                if is_stored
                    && ui
                        .button("Remove game")
                        .on_hover_text("Take it off this page and forget its settings.")
                        .clicked()
                {
                    remove = true;
                }
            });
            let games = &mut self.config.gaming_mode.games;
            if remove {
                games.remove(&row.name);
                changed = true;
            } else if game != before {
                // A running game that was not on the page is kept from its
                // first setting on, under the name its library has for it.
                if !is_stored && game.title.is_empty() && steam {
                    game.title = title;
                }
                games.insert(row.name, game);
                changed = true;
            }
        }
        if changed {
            self.events
                .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
        }
        if force_quit {
            self.force_quit_game();
        }
        if let Some(key) = launch {
            self.launch_game(&key);
        }
    }

    /// The frame-rate cap's switch and value. True when either changed.
    fn fps_limit_row(&mut self, ui: &mut Ui) -> bool {
        let limit = &mut self.config.gaming_mode.overlay.fps_limit;
        let mut on = *limit != 0;
        let mut changed = false;
        ui.horizontal(|ui| {
            let hover = "Holds every game that loads the overlay layer to this frame rate, \
                         and applies at once to running games. A game's own limiter gives \
                         lower input latency; use that where there is one. For one game, \
                         add ARGUS_LASSO_FPS_LIMIT=<fps> to its launch options (0 exempts it).";
            if ui
                .checkbox(&mut on, "Limit frame rate")
                .on_hover_text(hover)
                .changed()
            {
                *limit = if on { self.fps_limit_choice } else { 0 };
                changed = true;
            }
            let mut fps = self.fps_limit_choice;
            let range = argus_ipc::FPS_LIMIT_MIN..=argus_ipc::FPS_LIMIT_MAX;
            if ui
                .add_enabled(
                    on,
                    egui::DragValue::new(&mut fps)
                        // Typing 144 must not cap the game to 1, then 14, on the way.
                        .update_while_editing(false)
                        .range(range)
                        .suffix(" FPS"),
                )
                .changed()
            {
                self.fps_limit_choice = fps;
                *limit = fps;
                changed = true;
            }
        });
        changed
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
        self.poll_background(ctx);

        crate::gui::theme::section_nav(
            ui,
            &mut self.section,
            &[
                (GamingSection::Cpu, "CPU & performance"),
                (GamingSection::Games, "Games"),
                (GamingSection::Overlay, "Overlay"),
                (GamingSection::Recording, "Recording"),
                (GamingSection::Sensors, "Sensors"),
            ],
        );
        egui::ScrollArea::vertical()
            .id_salt(("gaming_body", self.section as u8))
            .show(ui, |ui| match self.section {
                GamingSection::Cpu => self.cpu_section(ui),
                GamingSection::Games => self.games_section(ui),
                GamingSection::Overlay => self.overlay_section(ui),
                GamingSection::Recording => {
                    crate::gui::theme::card_untitled(ui, |ui| self.benchmark.show(ui))
                }
                GamingSection::Sensors => {
                    crate::gui::theme::card_untitled(ui, |ui| self.sensor_access.show(ui))
                }
            });
        self.install_dialog(ctx);
        self.game_pickers(ctx, opacity);
    }

    /// Take in what the background helper install and power-profile change
    /// have reported since the last frame.
    fn poll_background(&mut self, ctx: &egui::Context) {
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
    }

    /// CPU & performance: the Gaming Mode switch, which threads it keeps, and
    /// what else it does.
    fn cpu_section(&mut self, ui: &mut Ui) {
        use crate::gui::theme::{self as th, tokens};
        let s = th::sem(ui);
        let has_asym = self
            .topo
            .as_ref()
            .map(|t| t.has_asymmetry())
            .unwrap_or(false);
        let mut reset_clicked = false;

        self.helper_banner(ui);
        self.status_hero(ui, has_asym);
        self.core_map_card(ui, has_asym);
        self.behaviour_card(ui);

        ui.add_space(tokens::SPACE_S);
        if ui
            .button("Restore all CPU assignments")
            .on_hover_text("Restores every process CPU affinity and brings all CPUs online.")
            .clicked()
        {
            reset_clicked = true;
        }
        egui::CollapsingHeader::new("Gaming activity").show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log_lines {
                        ui.monospace(line);
                    }
                });
        });

        // ── Footer: helper status ─────────────────────────────────────
        if self.helper_ok {
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
    }

    /// The blocking prerequisite gets one clear action.
    fn helper_banner(&mut self, ui: &mut Ui) {
        use crate::gui::theme::{self as th, tokens};
        let s = th::sem(ui);
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
    }

    /// State, topology summary and the one primary action.
    fn status_hero(&mut self, ui: &mut Ui, has_asym: bool) {
        use crate::gui::theme::{self as th, tokens};
        let s = th::sem(ui);
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
                    let enabled = has_asym && self.helper_ok && self.gaming_request.is_none();
                    let label = if self.parked {
                        "Disable Gaming Mode"
                    } else {
                        "Enable Gaming Mode"
                    };
                    let btn = egui::Button::new(
                        RichText::new(label)
                            .font(th::bold_font(tokens::FONT_BODY))
                            .color(s.on_accent),
                    )
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
    }

    /// Which threads stay online in Gaming Mode.
    fn core_map_card(&mut self, ui: &mut Ui, has_asym: bool) {
        use crate::gui::theme::{self as th, tokens};
        let s = th::sem(ui);
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
    }

    /// What happens when Gaming Mode is on, and the power profile.
    fn behaviour_card(&mut self, ui: &mut Ui) {
        use crate::gui::theme::{self as th, tokens};
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
    }

    /// The overlay switch with its customization submenu.
    fn overlay_section(&mut self, ui: &mut Ui) {
        use crate::gui::theme as th;
        let mut overlay_changed = false;
        th::card(ui, "In-game overlay", |ui| {
            overlay_changed = crate::gui::overlay_settings::show(
                ui,
                &mut self.config.gaming_mode.overlay,
                &mut self.overlay_settings_open,
                crate::utils::get_cpu_count(),
            );
            overlay_changed |= self.fps_limit_row(ui);
            th::help_text(
                ui,
                "A game can have its own limit and its HUD hidden: see the Games page, \
                 which also shows why a game has no HUD.",
            );
            ui.separator();
            let mut global = self.config.ui.global_overlay;
            if ui.checkbox(&mut global, "Automatically show in detected Vulkan games").on_hover_text("Detects Steam/Proton games. Terminal windows and desktop applications are skipped. Restart applications after changing this setting. Other games can opt in with ARGUS_LASSO_HUD=1; disable an individual game with ARGUS_LASSO_HUD_DISABLE=1.").changed() {
                match crate::gui::overlay_install::set_global(global) {
                    Ok(()) => { self.config.ui.global_overlay = global; overlay_changed = true; self.overlay_install_status = "Saved. Restart running games to change layer loading.".into(); }
                    Err(e) => self.overlay_install_status = format!("Could not change layer loading: {e}"),
                }
            }
            th::help_text(
                ui,
                "For manual activation, add this to the game’s Steam launch options:",
            );
            ui.monospace("ARGUS_LASSO_HUD=1 %command%");
            if !self.overlay_install_status.is_empty() {
                ui.label(&self.overlay_install_status);
            }
        });
        if overlay_changed {
            self.events
                .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
        }
    }

    fn install_dialog(&mut self, ctx: &egui::Context) {
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
    }

    fn game_pickers(&mut self, ctx: &egui::Context, opacity: f32) {
        if let Some(picker) = &mut self.steam_picker {
            if let Some(result) = picker.show(ctx, opacity) {
                // The picker just read the library: titles for new installs too.
                self.steam_library = Some(std::mem::take(&mut picker.games));
                self.steam_picker = None;
                if let Some((appid, name)) = result {
                    self.add_game(crate::config::steam_profile_key(&appid), name);
                }
            }
        }
        if let Some(picker) = &mut self.lutris_picker {
            if let Some(result) = picker.show(ctx, opacity) {
                self.lutris_picker = None;
                if let Some((slug, name)) = result {
                    self.add_game(
                        crate::config::lutris_profile_key(&slug),
                        lutris_title(&name),
                    );
                }
            }
        }
    }

    /// Put a game on the Games page, where it stays until removed.
    fn add_game(&mut self, key: String, title: String) {
        let games = &mut self.config.gaming_mode.games;
        if games.contains_key(&key) {
            return;
        }
        games.insert(
            key,
            crate::config::GameProfile {
                title,
                ..Default::default()
            },
        );
        self.events
            .push(GamingEvent::ConfigChanged(Box::new(self.config.clone())));
    }

    /// What the game stored under `key` is called.
    fn game_label(&self, key: &str) -> String {
        let stored = self.config.gaming_mode.games.get(key);
        let library = self.steam_library.as_deref().unwrap_or_default();
        game_title(key, stored.map_or("", |game| &game.title), None, library).0
    }

    /// Stop watching the launched game, and turn off the Gaming Mode its
    /// launch turned on.
    fn end_watch(&mut self) {
        if std::mem::take(&mut self.enabled_for_launch) && self.auto_restore && self.parked {
            self.disable_gaming_mode();
        }
        self.watch_phase = WatchPhase::Idle;
        self.launched = None;
        self.watch_key = None;
        self.watch_status = String::new();
    }

    /// Ask every process of the launched game to end.
    fn force_quit_game(&mut self) {
        if let Some(key) = self.watch_key.clone() {
            let title = self.game_label(&key);
            let mut games = LaunchedGame::all(&key, &title, self.launch_start_ticks);
            // The one being watched, if the game was not found again.
            games.extend(self.launched.take());
            let mut signalled = HashSet::new();
            for game in games {
                let pid = game.pid;
                if !signalled.insert(pid) {
                    continue;
                }
                match game.handle.signal(nix::sys::signal::Signal::SIGTERM) {
                    Ok(()) => self.append_log(format!("[Launcher] Sent SIGTERM to PID {pid}")),
                    Err(nix::errno::Errno::ESRCH) => {}
                    Err(e) => {
                        self.append_log(format!("[Launcher] SIGTERM to PID {pid} failed: {e}"))
                    }
                }
            }
        }
        self.end_watch();
    }

    fn launch_game(&mut self, key: &str) {
        let games = &self.config.gaming_mode.games;
        let game = games.get(key).cloned().unwrap_or_default();
        let label = self.game_label(key);
        self.launch_error = false;
        self.watch_key = Some(key.to_owned());
        let parts = match crate::config::launch_command(key, &game)
            .ok_or_else(|| "Enter a launch command first.".to_owned())
            .and_then(|command| parse_launch_command(&command))
        {
            Ok(parts) => parts,
            Err(error) => {
                self.launch_error = true;
                self.watch_status = error.clone();
                self.append_log(format!("[Launcher] {error}"));
                return;
            }
        };
        // Detection finds a Steam game by itself and follows its profile.
        // Anything else gets Gaming Mode from here, as its profile has it.
        let by_detection =
            self.config.gaming_mode.auto_detect && crate::config::steam_app_of(key).is_some();
        self.enabled_for_launch =
            !game.no_gaming_mode && !by_detection && !self.parked && self.enable_gaming_for(&game);
        match std::process::Command::new(&parts[0])
            .args(&parts[1..])
            .env("ARGUS_LASSO_HUD", "1")
            .env(crate::utils::LAUNCHED_GAME_ENV, key)
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
                    "[Launcher] Launching '{label}': {}",
                    parts.join(" ")
                ));
                self.watch_phase = WatchPhase::Waiting;
                self.watch_status = "Waiting for game process…".into();
                self.last_poll = std::time::Instant::now();
            }
            Err(error) => {
                if std::mem::take(&mut self.enabled_for_launch) {
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

/// Whether `pid` belongs to the launched game stored under `key`: a Steam
/// game by the app Steam started it as, a Lutris game by what "Launch" put
/// in its environment or by its title (a Lutris that was already running
/// starts the game itself), any other by its process name.
fn is_launched_game(key: &str, title: &str, pid: u32) -> bool {
    if let Some(appid) = crate::config::steam_app_of(key) {
        return crate::utils::steam_app_id(pid).as_deref() == Some(appid);
    }
    if crate::config::lutris_slug_of(key).is_some() {
        return crate::utils::game_identity(pid).1.as_deref() == Some(key)
            || proc_name_matches(title, pid);
    }
    proc_name_matches(key, pid)
}

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

    /// A library game is found by what its processes carry in their
    /// environment, whatever they are called: the app Steam started them
    /// as, or the key "Launch" gave what it started.
    #[test]
    fn a_library_game_is_found_by_its_environment() {
        let spawn = |var: &str, value: &str| {
            std::process::Command::new("sleep")
                .arg("30")
                .env(var, value)
                .spawn()
                .unwrap()
        };
        // Unlikely to be a Steam app running on the machine testing this.
        let appid = format!("99{}", std::process::id());
        let mut steam = spawn("SteamAppId", &appid);
        let key = format!("lutris:argus-test-{}", std::process::id());
        let mut lutris = spawn(crate::utils::LAUNCHED_GAME_ENV, &key);
        let found = |key: &str| {
            // The environment is the child's own once it has exec'd.
            (0..400).find_map(|_| {
                LaunchedGame::find(key, "", 0).or_else(|| {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    None
                })
            })
        };
        let steam_key = crate::config::steam_profile_key(&appid);
        assert_eq!(found(&steam_key).map(|g| g.pid), Some(steam.id()));
        assert_eq!(found(&key).map(|g| g.pid), Some(lutris.id()));
        assert_eq!(LaunchedGame::all(&key, "", 0).len(), 1);
        assert!(LaunchedGame::find("steam:1", "", 0).is_none());
        for child in [&mut steam, &mut lutris] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Runs a shell process renamed to `name` via `/proc/self/comm`; killed and
    /// reaped when dropped. Symlinking `/usr/bin/sleep` fails on Ubuntu 26.04+
    /// where uutils coreutils rejects symlinked names outside its tool list.
    struct Named(std::process::Child);

    impl Named {
        fn spawn(name: &str) -> Self {
            let child = std::process::Command::new("sh")
                .args([
                    "-c",
                    "printf '%s' \"$1\" > /proc/self/comm; read -r _",
                    "sh",
                    name,
                ])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
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
        let name = format!("aglatch{}", std::process::id() % 10_000_000);
        let first = Named::spawn(&name);
        // Start times are counted in 10 ms clock ticks.
        while Named::spawn("aglatchprobe").start() <= first.start() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let second = Named::spawn(&name);
        assert!(second.start() > first.start());

        assert_eq!(LaunchedGame::find(&name, "", 0).unwrap().pid, first.0.id());
        let later = LaunchedGame::find(&name, "", second.start()).unwrap();
        assert_eq!(
            later.pid,
            second.0.id(),
            "one started before the launch is not the game"
        );
        drop((first, second));
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

/// A game in the overlay section's list.
#[derive(Debug, PartialEq)]
struct GameRow {
    /// The key of its settings: a process name, `steam:<appid>` or
    /// `lutris:<slug>`.
    name: String,
    /// Its process name, for a library game that is running.
    process: Option<String>,
    /// Whether its HUD is drawn and what its layer says, if it is running.
    status: Option<(bool, String)>,
}

/// The title the Lutris picker shows without its "  [runner]".
fn lutris_title(label: &str) -> String {
    match label.rsplit_once("  [") {
        Some((name, runner)) if runner.ends_with(']') => name.to_owned(),
        _ => label.to_owned(),
    }
}

/// A card's title, and what else the game is known as. A library game goes
/// by its title, stored when it was added or looked up in the Steam library,
/// with what it is to Steam or Lutris and its running process below.
fn game_title(
    key: &str,
    stored_title: &str,
    process: Option<&str>,
    steam_library: &[(String, String)],
) -> (String, Option<String>) {
    let origin = match (
        crate::config::steam_app_of(key),
        crate::config::lutris_slug_of(key),
    ) {
        (Some(appid), _) => format!("Steam app {appid}"),
        (_, Some(slug)) => format!("Lutris game {slug}"),
        _ => return (key.to_owned(), None),
    };
    let known_as = match process {
        Some(process) => format!("{origin} · {process}"),
        None => origin.clone(),
    };
    let in_library = crate::config::steam_app_of(key)
        .and_then(|appid| steam_library.iter().find(|(id, _)| id == appid))
        .map(|(_, title)| title.as_str());
    match in_library.or(Some(stored_title).filter(|title| !title.is_empty())) {
        Some(title) => (title.to_owned(), Some(known_as)),
        None => (origin, process.map(str::to_owned)),
    }
}

/// The games to list: those connected now, then the others on the page;
/// each once. A running game is listed as the game it has settings as; a
/// Steam game without any, as the Steam game.
fn game_rows(
    clients: &[crate::monitor::OverlayClient],
    games: &std::collections::BTreeMap<String, crate::config::GameProfile>,
) -> Vec<GameRow> {
    let mut rows: Vec<GameRow> = Vec::new();
    for client in clients {
        // A game can hold several connections (one per process, or a
        // launcher beside it): the one that draws speaks for the name.
        let name = crate::config::profile_key(
            games,
            &client.name,
            client.steam_app.as_deref(),
            client.launched.as_deref(),
        )
        .or_else(|| {
            client
                .steam_app
                .as_deref()
                .map(crate::config::steam_profile_key)
        })
        .unwrap_or_else(|| client.name.clone());
        let process = (name != client.name).then(|| client.name.clone());
        match rows.iter_mut().find(|row| row.name == name) {
            Some(row) => {
                if client.drawing && !matches!(row.status, Some((true, _))) {
                    row.status = Some((true, client.detail.clone()));
                    row.process = process;
                }
            }
            None => rows.push(GameRow {
                name,
                process,
                status: Some((client.drawing, client.detail.clone())),
            }),
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    for name in games.keys() {
        if !rows.iter().any(|row| row.name == *name) {
            rows.push(GameRow {
                name: name.clone(),
                process: None,
                status: None,
            });
        }
    }
    rows
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

    #[test]
    fn the_games_list_names_each_game_once_running_ones_first() {
        use super::{game_rows, GameRow};
        let client = crate::monitor::OverlayClient::for_test;
        let clients = [
            client("Zeta.exe", false, "No HUD: reason"),
            client("Alpha.exe", false, "Connected"),
            // A second process of the same game, which does draw.
            client("Alpha.exe", true, "HUD on, 3440x1440 HDR10"),
        ];
        let games = [
            ("Alpha.exe".to_owned(), Default::default()),
            ("Stored.exe".to_owned(), Default::default()),
            ("lutris:own".to_owned(), Default::default()),
        ]
        .into();
        let row = |name: &str, status: Option<(bool, &str)>| GameRow {
            name: name.into(),
            process: None,
            status: status.map(|(drawing, detail)| (drawing, detail.to_owned())),
        };
        assert_eq!(
            game_rows(&clients, &games),
            [
                row("Alpha.exe", Some((true, "HUD on, 3440x1440 HDR10"))),
                row("Zeta.exe", Some((false, "No HUD: reason"))),
                row("Stored.exe", None),
                row("lutris:own", None),
            ]
        );
        // Launched from the page as the Lutris game: listed as that game.
        let mut launched = client("wine-game", true, "HUD on");
        launched.launched = Some("lutris:own".into());
        let rows = game_rows(&[launched], &games);
        assert_eq!(rows[0].name, "lutris:own");
        assert_eq!(rows[0].process.as_deref(), Some("wine-game"));
        assert_eq!(rows.len(), 3);
    }

    /// A running Steam game is one card under its library title, where a
    /// profile then covers all its processes.
    #[test]
    fn a_running_steam_game_is_listed_as_the_steam_game() {
        use super::{game_rows, game_title, GameRow};
        let client = crate::monitor::OverlayClient::for_test;
        let clients = [
            client("Game.exe", true, "HUD on").of_steam_app("620"),
            // Its process name has a profile already: that one stays.
            client("Own.exe", true, "HUD on").of_steam_app("400"),
        ];
        let games = [
            ("Own.exe".to_owned(), Default::default()),
            ("steam:10".to_owned(), Default::default()),
        ]
        .into();
        let rows = game_rows(&clients, &games);
        assert_eq!(
            rows,
            [
                GameRow {
                    name: "Own.exe".into(),
                    process: None,
                    status: Some((true, "HUD on".into())),
                },
                GameRow {
                    name: "steam:620".into(),
                    process: Some("Game.exe".into()),
                    status: Some((true, "HUD on".into())),
                },
                GameRow {
                    name: "steam:10".into(),
                    process: None,
                    status: None,
                },
            ]
        );
        let library = [("620".to_owned(), "Portal 2".to_owned())];
        let title = |row: &GameRow, stored: &str| {
            game_title(&row.name, stored, row.process.as_deref(), &library)
        };
        assert_eq!(title(&rows[0], ""), ("Own.exe".to_owned(), None));
        assert_eq!(
            title(&rows[1], ""),
            (
                "Portal 2".to_owned(),
                Some("Steam app 620 · Game.exe".to_owned())
            )
        );
        // No longer installed: the title it was added under, or failing
        // that the app number.
        assert_eq!(
            title(&rows[2], "Old Game"),
            ("Old Game".to_owned(), Some("Steam app 10".to_owned()))
        );
        assert_eq!(title(&rows[2], ""), ("Steam app 10".to_owned(), None));
        assert_eq!(
            game_title("lutris:the-witcher-3", "The Witcher 3", None, &library),
            (
                "The Witcher 3".to_owned(),
                Some("Lutris game the-witcher-3".to_owned())
            )
        );
        assert_eq!(
            super::lutris_title("The Witcher 3  [wine]"),
            "The Witcher 3"
        );
        assert_eq!(super::lutris_title("Plain"), "Plain");
    }

    /// The same for the frame-rate cap, which `argus-lasso fps-limit` sets.
    #[test]
    fn a_cap_set_elsewhere_survives_the_next_overlay_edit() {
        let mut tab = GamingModeTab::new(crate::config::Config::default());
        assert_eq!(tab.fps_limit_choice, 60);

        tab.follow_fps_limit(120);
        assert_eq!(tab.config.gaming_mode.overlay.fps_limit, 120);
        // The checkbox now turns on the cap that was last in use.
        assert_eq!(tab.fps_limit_choice, 120);

        // Another edit sends 120 back while the CLI lifted the cap.
        assert_eq!(tab.fps_limit(120, 0), 0);
        // A cap chosen in this tab wins.
        assert_eq!(tab.fps_limit(90, 120), 90);

        // Lifting the cap keeps the value for the next time it is ticked.
        tab.follow_fps_limit(0);
        assert_eq!(tab.fps_limit_choice, 120);
    }
}
