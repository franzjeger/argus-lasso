//! eframe App: shared state, tab routing, purple theme, system tray.

use std::sync::{Arc, Mutex};

use eframe::egui;
use egui::{Context, RichText};

use crossbeam_channel::Sender;

use crate::config::{self, Config};
use crate::gui::bench_tab::BenchTab;
use crate::gui::dialogs::{AffinityDialog, IoNiceDialog, NiceDialog};
use crate::gui::gaming_mode_tab::{GamingEvent, GamingModeTab};
use crate::gui::hw_monitor_tab::HwMonitorTab;
use crate::gui::log_tab::LogTab;
use crate::gui::overview_tab::OverviewTab;
use crate::gui::probalance_tab::ProBalanceTab;
use crate::gui::process_tab::{ProcessTab, TableAction};
use crate::gui::rules_tab::RulesTab;
use crate::gui::settings_tab::SettingsTab;
use crate::monitor::{AppState, DaemonCmd};
use crate::rules::RuleEngine;
use crate::utils;

// ── Active tab ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tab {
    Overview,
    Processes,
    Rules,
    ProBalance,
    GamingMode,
    HwMonitor,
    Benchmark,
    Settings,
    Log,
}

// ── CPU temperature ───────────────────────────────────────────────────────────

/// Read CPU temperature from hwmon sysfs. Returns degrees Celsius or None.
fn read_cpu_temp() -> Option<f32> {
    const KNOWN_NAMES: &[&str] = &["k10temp", "zenpower", "coretemp"];

    let hwmon_dir = std::path::Path::new("/sys/class/hwmon");
    let entries = std::fs::read_dir(hwmon_dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let name_path = path.join("name");
        let name = std::fs::read_to_string(&name_path)
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let is_match = KNOWN_NAMES.contains(&name.as_str()) || name.starts_with("it8");

        if !is_match {
            continue;
        }

        // Collect all temp*_input files and return the highest value
        let mut max_temp: Option<f32> = None;
        if let Ok(dir_entries) = std::fs::read_dir(&path) {
            for de in dir_entries.flatten() {
                let fname = de.file_name();
                let fname_str = fname.to_string_lossy();
                if fname_str.starts_with("temp") && fname_str.ends_with("_input") {
                    if let Ok(raw) = std::fs::read_to_string(de.path()) {
                        if let Ok(val) = raw.trim().parse::<i64>() {
                            let celsius = val as f32 / 1000.0;
                            max_temp = Some(max_temp.map_or(celsius, |m: f32| m.max(celsius)));
                        }
                    }
                }
            }
        }

        if max_temp.is_some() {
            return max_temp;
        }
    }

    None
}

// ── ArgusLassoApp ─────────────────────────────────────────────────────────────

pub struct ArgusLassoApp {
    state: Arc<Mutex<AppState>>,
    cmd_tx: Sender<DaemonCmd>,
    rule_engine: Arc<Mutex<RuleEngine>>,

    active_tab: Tab,
    process_tab: ProcessTab,
    rules_tab: RulesTab,
    probalance_tab: ProBalanceTab,
    /// In-app update check / self-install.
    updates: crate::updater::UpdateState,
    gaming_mode_tab: GamingModeTab,
    hw_monitor_tab: HwMonitorTab,
    bench_tab: BenchTab,
    overview_tab: OverviewTab,
    settings_tab: SettingsTab,
    log_tab: LogTab,

    // Per-process dialogs — each tracks its own target PID so two open
    // dialogs can never apply one process's settings to another.
    dialog_manager: crate::gui::dialog_manager::DialogManager,

    // Process count for tab title
    proc_count: usize,
    throttled_count: usize,

    // Generation counter: only push CPU history when daemon emits new data
    last_cpu_gen: u64,

    // Wayland compositor-side opacity via wp_alpha_modifier_v1
    wayland_opacity: Option<crate::wayland_opacity::WaylandOpacity>,
    // Current window opacity (0.1–1.0); tracked so we only call set() when it changes
    opacity: f32,
    // Native pixels-per-point at startup (for HiDPI scaling)
    native_ppp: f32,

    // Repaint rate diagnostics
    repaint_count: u32,
    last_repaint_log: std::time::Instant,

    // Track last persisted opacity/theme to detect changes for immediate save
    last_saved_opacity: f32,
    last_saved_theme: String,

    // CPU temperature read from hwmon sysfs
    cpu_temp: Option<f32>,
    // Pending kill awaiting undo
    pending_kill: Option<crate::gui::process_tab::PendingKill>,
    // Pending "create a rule from this manual change?" offer
    detail_window: crate::gui::detail_window::DetailWindow,
    // How many notable events the user has seen (bell badge = len - seen)
    events_seen: usize,
    // CPU model string for status bar
    cpu_model: String,
    // Debounce config saves during column-resize drags: cols_dirty is true on
    // every frame while a divider is dragged, so saving immediately would
    // fsync the config ~60×/sec. Persist at most once per 300ms instead.
    // (The live value is already in shared state each frame; only the disk
    // write is throttled.)
    last_col_save: std::time::Instant,
    /// Set only by --ui-tour: drives the app through every screen, capturing
    /// each, then exits. None in every normal run.
    tour: Option<crate::ui_tour::Tour>,
}

impl ArgusLassoApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        state: Arc<Mutex<AppState>>,
        cmd_tx: Sender<DaemonCmd>,
        rule_engine: Arc<Mutex<RuleEngine>>,
        config: Config,
        tour_dir: Option<std::path::PathBuf>,
    ) -> Self {
        // native_pixels_per_point is set by the platform integration before new() is called.
        let native_ppp = cc.egui_ctx.pixels_per_point();
        let startup_theme = crate::gui::theme::AppTheme::from_str(&config.ui.theme);
        crate::gui::theme::apply_theme(&cc.egui_ctx, native_ppp, &startup_theme);

        let mut updates = crate::updater::UpdateState::default();
        if config.ui.check_updates_on_start {
            updates.start_check();
        }

        let probalance_tab = ProBalanceTab::new(config.probalance.clone());
        let gaming_mode_tab = GamingModeTab::new(config.clone());
        let mut settings_tab = SettingsTab::new(config.clone());
        settings_tab.native_ppp = native_ppp;

        // Initialise Wayland compositor-side opacity via wp_alpha_modifier_v1.
        // Extract the raw wl_display* and wl_surface* that eframe already holds.
        use raw_window_handle::{
            HasDisplayHandle as _, HasWindowHandle as _, RawDisplayHandle, RawWindowHandle,
        };
        let display_ptr: *mut std::ffi::c_void = cc
            .display_handle()
            .ok()
            .and_then(|dh| match dh.as_raw() {
                RawDisplayHandle::Wayland(h) => Some(h.display.as_ptr()),
                _ => None,
            })
            .unwrap_or(std::ptr::null_mut());
        let surface_ptr: *mut std::ffi::c_void = cc
            .window_handle()
            .ok()
            .and_then(|wh| match wh.as_raw() {
                RawWindowHandle::Wayland(h) => Some(h.surface.as_ptr()),
                _ => None,
            })
            .unwrap_or(std::ptr::null_mut());

        let wayland_opacity = crate::wayland_opacity::WaylandOpacity::new(display_ptr, surface_ptr);
        if wayland_opacity.is_none() {
            log::warn!(
                "Wayland opacity unavailable — compositor does not support wp_alpha_modifier_v1"
            );
        }

        // Restore saved opacity; apply immediately so it takes effect on first frame.
        let saved_opacity = config.ui.opacity.clamp(0.1, 1.0);
        if (saved_opacity - 1.0).abs() > 0.001 {
            if let Some(ref wo) = wayland_opacity {
                wo.set(saved_opacity);
            }
        }

        // Sync state config
        if let Ok(mut s) = state.lock() {
            s.config = config.clone();
        }

        let last_saved_opacity = saved_opacity;
        let last_saved_theme = startup_theme.to_str().to_string();
        let cpu_temp = read_cpu_temp();
        let cpu_model = crate::monitor::read_cpu_model();

        Self {
            state,
            cmd_tx,
            rule_engine,
            active_tab: Tab::Overview,
            process_tab: ProcessTab::new(&config.ui.col_widths, &config.ui.hidden_columns),
            rules_tab: RulesTab::new(),
            probalance_tab,
            updates,
            gaming_mode_tab,
            hw_monitor_tab: HwMonitorTab::new_with_widths(&config.ui.hw_mon_col_widths),
            bench_tab: BenchTab::new(),
            overview_tab: OverviewTab::new(),
            settings_tab,
            log_tab: LogTab::new(),
            dialog_manager: Default::default(),
            proc_count: 0,
            throttled_count: 0,
            last_cpu_gen: 0,
            wayland_opacity,
            opacity: saved_opacity,
            native_ppp,
            repaint_count: 0,
            last_repaint_log: std::time::Instant::now(),
            last_saved_opacity,
            last_saved_theme,
            cpu_temp,
            pending_kill: None,
            detail_window: Default::default(),
            tour: tour_dir.map(|d| match crate::ui_tour::Tour::new(d) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("--ui-tour: {e}");
                    std::process::exit(1);
                }
            }),
            events_seen: 0,
            cpu_model,
            last_col_save: std::time::Instant::now(),
        }
    }

    /// Debounce gate for column-resize saves. `cols_dirty` is true on every
    /// frame of a divider drag (including the release frame that applies the
    /// final delta), so saving immediately would fsync the config ~60×/sec.
    /// Allow at most one save per 300ms; the live width is already in shared
    /// state each frame, so throttling only the disk write loses nothing.
    fn col_save_due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_col_save) < std::time::Duration::from_millis(300) {
            return false;
        }
        self.last_col_save = now;
        true
    }

    fn send(&self, cmd: DaemonCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn save_config(&self) {
        let cfg = if let Ok(s) = self.state.lock() {
            s.config.clone()
        } else {
            return;
        };
        if let Err(e) = config::save(&cfg) {
            log::warn!("Config save failed: {e}");
        }
    }

    /// Surface a user-action failure: a log line (always) plus a desktop
    /// notification when the user has them enabled. Previously several
    /// failure paths (signal send, affinity/nice/ionice apply) were silent,
    /// so the user believed the change had taken effect.

    /// Send the actual kill signal. The target was SIGSTOPped for the undo
    /// window, and a stopped process never sees SIGTERM — so always follow up
    /// with SIGCONT to deliver it (harmless for SIGKILL).
    fn deliver_kill(pid: u32, force: bool) -> Result<(), nix::Error> {
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

    /// Put the UI into the state the current tour step documents.
    ///
    /// Re-applied every frame rather than once on entry: the dialogs close
    /// themselves when their own `open` flag flips, and a settle period spans
    /// several frames, so a one-shot setup would photograph an empty screen.
    fn apply_tour_step(&mut self) {
        use crate::ui_tour::Step;
        let Some(step) = self.tour.as_ref().and_then(|t| t.current()) else {
            return;
        };

        // Close everything the previous step opened, keeping only what this
        // step owns. Clearing per-overlay rather than "only on non-dialog
        // steps" matters: the coarser version left the nice dialog standing
        // through the ionice step, and the two captures came out identical.
        // Keeping this step's own overlay alive across the settle frames is
        // what stops it being rebuilt — and re-running topology detection —
        // on every frame.
        self.dialog_manager = Default::default();
        if !matches!(step, Step::ProcessDetails) {
            self.detail_window = Default::default();
        }
        if !matches!(step, Step::KillToast) {
            self.pending_kill = None;
        }

        // A real PID with real values, so the dialogs are not full of zeroes.
        // Our own process is always present, which keeps the tour reproducible.
        let pid = std::process::id();

        match step {
            Step::Overview => self.active_tab = Tab::Overview,
            Step::Processes => self.active_tab = Tab::Processes,
            Step::Rules => self.active_tab = Tab::Rules,
            Step::ProBalance => self.active_tab = Tab::ProBalance,
            Step::GamingMode => self.active_tab = Tab::GamingMode,
            Step::HwMonitor => self.active_tab = Tab::HwMonitor,
            Step::Benchmark => self.active_tab = Tab::Benchmark,
            Step::Log => self.active_tab = Tab::Log,
            Step::Settings => self.active_tab = Tab::Settings,

            Step::ProcessDetails => {
                self.active_tab = Tab::Processes;
                if self.detail_window.detail_pid != Some(pid) {
                    self.detail_window.set_pid(pid);
                }
            }
            Step::KillToast => {
                self.active_tab = Tab::Processes;
                // Far enough out that the countdown never reaches zero and
                // fires a real SIGTERM mid-tour.
                if self.pending_kill.is_none() {
                    self.pending_kill = Some(crate::gui::process_tab::PendingKill {
                        pid,
                        name: "argus-lasso".into(),
                        force: false,
                        deadline: std::time::Instant::now() + std::time::Duration::from_secs(3600),
                    });
                }
            }
            Step::RuleOffer => {
                self.active_tab = Tab::Processes;
                if self.dialog_manager.rule_offer.is_none() {
                    self.dialog_manager.rule_offer = Some(crate::gui::dialog_manager::RuleOffer {
                        proc_name: "argus-lasso".into(),
                        affinity: Some("0-7".into()),
                        nice: Some(-5),
                        ionice: Some((2, 4)),
                    });
                }
            }
        }
    }
}

impl eframe::App for ArgusLassoApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Closing the window exits the whole process (daemon included) — ask
        // the daemon to restore nices/throttles/parked CPUs and wait briefly.
        crate::monitor::shutdown_and_wait(&self.state, &self.cmd_tx);
    }

    /// 0.34 makes `ui` the required entry point and deprecates `update`.
    /// The body still works in terms of a `Context` — panels are the only
    /// thing that had to change — so it is taken from the `Ui` we are given.
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root_ui.ctx().clone();
        // --ui-tour drives the UI from a script rather than from the user.
        // Applied before the frame is built so the capture at the end of it
        // shows the screen this step is meant to document.
        if self.tour.is_some() {
            self.apply_tour_step();
        }
        // Repaint rate diagnostics — log repaints/sec approximately every 10s
        self.repaint_count += 1;
        let elapsed = self.last_repaint_log.elapsed();
        if elapsed >= std::time::Duration::from_secs(10) {
            let rate = self.repaint_count as f32 / elapsed.as_secs_f32();
            log::debug!(
                "repaint rate: {:.1}/sec ({} in {:.1}s)",
                rate,
                self.repaint_count,
                elapsed.as_secs_f32()
            );
            self.repaint_count = 0;
            self.last_repaint_log = std::time::Instant::now();
        }

        // Pull snapshot from shared state — lock held only for this clone block.
        // Expensive clones (log_lines, hw_monitor) only when the relevant tab is active.
        let on_log_tab = self.active_tab == Tab::Log;
        let on_hw_tab = self.active_tab == Tab::HwMonitor;
        let on_pb_tab = self.active_tab == Tab::ProBalance;
        // The details window (any tab) also needs the per-PID CPU history —
        // otherwise its sparkline vanishes when switching away from Processes.
        let on_proc_tab = self.active_tab == Tab::Processes
            || self.active_tab == Tab::Overview
            || self.detail_window.detail_pid.is_some();
        let on_overview_tab = self.active_tab == Tab::Overview;
        let (
            snapshot,
            cpu_pcts,
            cpu_gen,
            throttled_pids,
            suspended_pids,
            throttle_infos,
            log_lines,
            config,
            gaming_active,
            hw_monitor,
            proc_cpu_history,
            cpu_history,
            disk_io_history,
            net_io_history,
            notable_events,
            cpu_avg,
        ) = {
            if let Ok(s) = self.state.lock() {
                (
                    s.snapshot.clone(),
                    s.cpu_percents.clone(),
                    s.cpu_generation,
                    s.throttled_pids.clone(),
                    s.suspended_pids.clone(),
                    if on_pb_tab {
                        s.throttle_infos.clone()
                    } else {
                        Default::default()
                    },
                    if on_log_tab {
                        s.log_lines.clone()
                    } else {
                        Default::default()
                    },
                    s.config.clone(),
                    s.gaming_active,
                    if on_hw_tab {
                        s.hw_monitor.clone()
                    } else {
                        Default::default()
                    },
                    if on_proc_tab {
                        s.proc_cpu_history.clone()
                    } else {
                        Default::default()
                    },
                    if on_overview_tab {
                        s.cpu_history.clone()
                    } else {
                        Default::default()
                    },
                    if on_overview_tab {
                        s.disk_io_history.clone()
                    } else {
                        Default::default()
                    },
                    if on_overview_tab {
                        s.net_io_history.clone()
                    } else {
                        Default::default()
                    },
                    s.notable_events.clone(),
                    s.cpu_avg,
                )
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
                return;
            }
        };

        self.proc_count = snapshot.len();
        self.throttled_count = throttled_pids.len();

        // Only push CPU bars + history when the daemon has emitted a new sample.
        // The hwmon temp scan (a full /sys/class/hwmon walk) also lives here —
        // it's far too expensive to run on every 60fps repaint.
        if cpu_gen != self.last_cpu_gen && !cpu_pcts.is_empty() {
            self.last_cpu_gen = cpu_gen;
            self.process_tab.update_cpu(cpu_pcts.clone());
            self.cpu_temp = read_cpu_temp();
        }

        // Poll active dialogs
        let notif_enabled = config.ui.notifications_enabled;
        let notify_error = move |msg: &str| {
            log::error!("{msg}");
            if notif_enabled {
                let _ = notify_rust::Notification::new()
                    .summary("Argus-Lasso Error")
                    .body(msg)
                    .timeout(notify_rust::Timeout::Milliseconds(5000))
                    .show();
            }
        };
        self.dialog_manager.poll_dialogs(
            ctx,
            self.opacity,
            &self.state,
            &self.cmd_tx,
            &self.rule_engine,
            &notify_error,
        );

        // Per-process details window
        self.detail_window
            .show(ctx, &snapshot, &proc_cpu_history, cpu_gen);

        // Check pending kill
        if let Some(ref pk) = self.pending_kill {
            if std::time::Instant::now() >= pk.deadline {
                let name = pk.name.clone();
                let pid = pk.pid;
                let force = pk.force;
                let msg = match crate::gui::action_handler::ActionHandler::deliver_kill(pid, force)
                {
                    Ok(_) => format!(
                        "{}illed {} ({})",
                        if force { "Force k" } else { "K" },
                        name,
                        pid
                    ),
                    Err(e) => format!("Kill failed for {} ({}): {e}", name, pid),
                };
                if config.ui.notifications_enabled {
                    let _ = notify_rust::Notification::new()
                        .summary("Argus-Lasso")
                        .body(&msg)
                        .timeout(notify_rust::Timeout::Milliseconds(3000))
                        .show();
                }
                if let Ok(mut s) = self.state.lock() {
                    s.append_log(msg);
                }
                self.pending_kill = None;
            }
        }

        // ── Top-level panels ─────────────────────────────────────────────
        // Build pending-kill display info before the panel closure (avoids borrow issues)
        let pending_kill_info: Option<(u32, String, u64)> = self.pending_kill.as_ref().map(|pk| {
            let remaining = pk
                .deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_secs();
            (pk.pid, pk.name.clone(), remaining)
        });
        let mut undo_requested = false;

        egui::Panel::bottom("status_bar").show_inside(root_ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Processes: {}", self.proc_count));
                ui.separator();
                let avg = if cpu_pcts.is_empty() {
                    0.0
                } else {
                    cpu_pcts.iter().sum::<f32>() / cpu_pcts.len() as f32
                };
                ui.label(format!("CPU avg: {avg:.0}%"));
                if let Some(temp) = self.cpu_temp {
                    ui.separator();
                    ui.label(format!("CPU temp: {temp:.0}°C"));
                }
                if !self.cpu_model.is_empty() {
                    ui.separator();
                    // Core count moved here from the Overview CPU card: it is
                    // a static machine fact, and it belongs next to the model
                    // rather than in a tile showing live load.
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  ·  {} cores",
                            self.cpu_model,
                            utils::get_cpu_count()
                        ))
                        .weak(),
                    );
                }
                ui.separator();
                if gaming_active {
                    ui.colored_label(crate::gui::theme::Breeze::POSITIVE, "⚡ Gaming Mode ACTIVE");
                }

                // ── Notification center (right-aligned bell with unseen badge)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let unseen = notable_events.len().saturating_sub(self.events_seen);
                    let bell_label = if unseen > 0 {
                        format!("🔔 {unseen}")
                    } else {
                        "🔔".to_string()
                    };
                    let bell = if unseen > 0 {
                        RichText::new(bell_label)
                            .color(crate::gui::theme::Breeze::WARNING)
                            .strong()
                    } else {
                        RichText::new(bell_label).weak()
                    };
                    let resp = ui
                        .add(egui::Label::new(bell).sense(egui::Sense::click()))
                        .on_hover_text("Recent events (throttles, alerts, gaming mode)");
                    if resp.clicked() {
                        self.events_seen = notable_events.len();
                    }
                    egui::Popup::from_toggle_button_response(&resp)
                        .align(egui::RectAlign::TOP_END)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            ui.set_min_width(420.0);
                            ui.label(RichText::new("Recent events").strong());
                            ui.separator();
                            if notable_events.is_empty() {
                                ui.label(RichText::new("Nothing yet.").weak());
                            }
                            for line in notable_events.iter().rev().take(12) {
                                ui.label(
                                    RichText::new(line)
                                        .size(crate::gui::theme::tokens::FONT_SMALL)
                                        .monospace(),
                                );
                            }
                        });
                });
            });
        });

        // ── Kill-undo toast (bottom-right, above the rule-offer slot) ──────
        if let Some((_, ref kill_name, remaining)) = pending_kill_info {
            egui::Window::new("kill_toast")
                .id(egui::Id::new("kill_toast_window"))
                .title_bar(false)
                .resizable(false)
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -140.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            crate::gui::theme::Breeze::WARNING,
                            format!("Killing '{}' in {}s", kill_name, remaining + 1),
                        );
                        if ui.button("Undo").clicked() {
                            undo_requested = true;
                        }
                    });
                });
        }

        if undo_requested {
            if let Some(ref pk) = self.pending_kill {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                let cont = signal::kill(Pid::from_raw(pk.pid as i32), Signal::SIGCONT);
                let name = pk.name.clone();
                let pid = pk.pid;
                if let Ok(mut s) = self.state.lock() {
                    match cont {
                        Ok(()) => {
                            s.append_log(format!("Kill cancelled — resumed {} ({})", name, pid))
                        }
                        Err(e) => s.append_log(format!(
                            "Kill cancelled but resume failed ({}); {} ({}) is still suspended",
                            e, name, pid
                        )),
                    }
                }
            }
            self.pending_kill = None;
        }

        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            // Tab bar: five primary workflow tabs on the left; the occasional
            // tools live behind a "Tools ▾" menu and Settings behind the gear,
            // so nine equal flat tabs no longer bury the ones people live in.
            ui.horizontal(|ui| {
                use crate::gui::theme as th;
                let s = th::sem(ui);

                // One tab: accent background + 2px bottom underline when active,
                // with an optional count pill badge inside the label.
                // A plain fn returning "was clicked" — a closure capturing the
                // result slot would hold a mutable borrow across the whole bar.
                let active_now = self.active_tab.clone();
                let mut clicked_tab: Option<Tab> = None;
                fn tab_button(
                    ui: &mut egui::Ui,
                    label: &str,
                    selected: bool,
                    badge: Option<usize>,
                ) -> bool {
                    use crate::gui::theme::{self as th, tokens};
                    let s = th::sem(ui);
                    let text_col = if selected {
                        s.accent
                    } else {
                        ui.visuals().text_color()
                    };
                    let galley = ui.painter().layout_no_wrap(
                        label.to_string(),
                        egui::FontId::proportional(tokens::FONT_BODY),
                        text_col,
                    );
                    let badge_txt = badge.map(|n| n.to_string());
                    let badge_galley = badge_txt.as_ref().map(|t| {
                        ui.painter().layout_no_wrap(
                            t.clone(),
                            egui::FontId::proportional(tokens::FONT_SMALL),
                            if selected { s.on_accent } else { s.accent },
                        )
                    });
                    let badge_w = badge_galley
                        .as_ref()
                        .map(|g| g.size().x + 12.0 + 6.0)
                        .unwrap_or(0.0);
                    let pad = egui::vec2(10.0, 5.0);
                    let size = egui::vec2(
                        galley.size().x + badge_w + pad.x * 2.0,
                        galley.size().y + pad.y * 2.0,
                    );
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        if selected {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius {
                                    nw: 3,
                                    ne: 3,
                                    sw: 0,
                                    se: 0,
                                },
                                th::tint(s.accent, 38),
                            );
                            // 2px accent underline
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(rect.left(), rect.bottom() - 2.0),
                                    egui::vec2(rect.width(), 2.0),
                                ),
                                0.0,
                                s.accent,
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(3),
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        ui.painter()
                            .galley(rect.min + pad, galley, egui::Color32::WHITE);
                        if let Some(bg) = badge_galley {
                            let bw = bg.size().x + 12.0;
                            let brect = egui::Rect::from_min_size(
                                egui::pos2(rect.max.x - pad.x - bw, rect.center().y - 8.0),
                                egui::vec2(bw, 16.0),
                            );
                            let fill = if selected {
                                s.accent
                            } else {
                                th::tint(s.accent, 46)
                            };
                            ui.painter()
                                .rect_filled(brect, egui::CornerRadius::same(8), fill);
                            let off = (brect.size() - bg.size()) * 0.5;
                            ui.painter()
                                .galley(brect.min + off, bg, egui::Color32::WHITE);
                        }
                    }
                    resp.clicked()
                }

                let pick = |ui: &mut egui::Ui,
                            label: &str,
                            tab: Tab,
                            badge: Option<usize>,
                            clicked_tab: &mut Option<Tab>| {
                    if tab_button(ui, label, active_now == tab, badge) {
                        *clicked_tab = Some(tab);
                    }
                };

                pick(ui, "Overview", Tab::Overview, None, &mut clicked_tab);
                pick(
                    ui,
                    "Processes",
                    Tab::Processes,
                    Some(self.proc_count),
                    &mut clicked_tab,
                );
                pick(ui, "Rules", Tab::Rules, None, &mut clicked_tab);
                pick(
                    ui,
                    "ProBalance",
                    Tab::ProBalance,
                    (self.throttled_count > 0).then_some(self.throttled_count),
                    &mut clicked_tab,
                );
                pick(ui, "Gaming Mode", Tab::GamingMode, None, &mut clicked_tab);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right-to-left: Settings first (rightmost), then Tools menu.
                    pick(ui, "⚙  Settings", Tab::Settings, None, &mut clicked_tab);
                    let tools_active =
                        matches!(active_now, Tab::HwMonitor | Tab::Benchmark | Tab::Log);
                    let tools_label = if tools_active {
                        RichText::new("Tools ▾").color(s.accent).strong()
                    } else {
                        RichText::new("Tools ▾")
                    };
                    // menu_button paints a full button frame, which read as
                    // "this control is pressed" next to the frameless tabs —
                    // the design review flagged it as looking active with the
                    // menu closed. Strip the frame so it sits in the bar like
                    // the tabs do; the accent text still marks a Tools tab.
                    // Scoped to this one button (a global visuals_mut() here
                    // would strip every button's frame app-wide).
                    egui::containers::menu::MenuButton::from_button(
                        egui::Button::new(tools_label).frame(false),
                    )
                    .ui(ui, |ui| {
                        for (label, tab) in [
                            ("HW Monitor", Tab::HwMonitor),
                            ("Benchmark", Tab::Benchmark),
                            ("Log", Tab::Log),
                        ] {
                            if ui.selectable_label(active_now == tab, label).clicked() {
                                clicked_tab = Some(tab);
                                ui.close();
                            }
                        }
                    });
                });
                if let Some(tab) = clicked_tab {
                    self.active_tab = tab;
                }
            });
            ui.separator();

            // ── Update banner ────────────────────────────────────────────
            // Poll first: the worker runs off-thread, so without an explicit
            // repaint the result would sit unseen until the next input event.
            if self.updates.poll() {
                ctx.request_repaint();
            }
            if let Some(update) = self.updates.available.as_ref() {
                if !self.updates.banner_dismissed {
                    let s = crate::gui::theme::sem(ui);
                    let text = if self.updates.installed {
                        format!("{} installed — restart to run it", update.tag)
                    } else {
                        format!(
                            "{} is available (you have v{})",
                            update.tag,
                            crate::updater::current_version()
                        )
                    };
                    let action = if self.updates.installed {
                        "Restart now"
                    } else if self.updates.busy {
                        "Working…"
                    } else {
                        "Update now"
                    };
                    ui.add_space(4.0);
                    let (act, dismiss) = crate::gui::theme::banner_dismissible(
                        ui,
                        s.accent,
                        &text,
                        Some(action),
                        true,
                    );
                    if dismiss {
                        self.updates.banner_dismissed = true;
                    }
                    if act && !self.updates.busy {
                        if self.updates.installed {
                            self.updates.restart_requested = true;
                        } else {
                            self.updates.start_install();
                        }
                    }
                    ui.add_space(4.0);
                }
            }

            // ── Tab content ──────────────────────────────────────────────
            match self.active_tab {
                Tab::Overview => {
                    self.overview_tab.show(
                        ui,
                        &cpu_history,
                        cpu_avg,
                        &snapshot,
                        &disk_io_history,
                        &net_io_history,
                        self.cpu_temp,
                        self.throttled_count,
                    );
                }

                Tab::Processes => {
                    let action = self.process_tab.show(
                        ui,
                        &snapshot,
                        &throttled_pids,
                        &suspended_pids,
                        &self.cmd_tx,
                        &self.rule_engine,
                        gaming_active,
                        &proc_cpu_history,
                    );
                    let mut trigger_rule = None;
                    crate::gui::action_handler::ActionHandler::handle(
                        action,
                        &snapshot,
                        &self.state,
                        &mut self.pending_kill,
                        &mut self.dialog_manager,
                        &mut self.detail_window,
                        &notify_error,
                        &mut trigger_rule,
                    );
                    if let Some(rule) = trigger_rule {
                        self.rules_tab.open_add_dialog(Some(rule));
                        self.active_tab = Tab::Rules;
                    }
                    // Persist col_widths when user drags a column divider
                    // (debounced — see col_save_due).
                    if self.process_tab.cols_dirty {
                        if let Ok(mut s) = self.state.lock() {
                            s.config.ui.col_widths = self.process_tab.col_widths.clone();
                        }
                        if self.col_save_due() {
                            self.save_config();
                        }
                    }
                    // Persist column visibility from the header context menu
                    if self.process_tab.hidden_dirty {
                        self.process_tab.hidden_dirty = false;
                        let mut hidden: Vec<String> =
                            self.process_tab.hidden_cols.iter().cloned().collect();
                        hidden.sort();
                        if let Ok(mut s) = self.state.lock() {
                            s.config.ui.hidden_columns = hidden;
                        }
                        self.save_config();
                    }
                }

                Tab::Rules => {
                    let mut rules_changed = false;
                    let mut profiles_changed = false;
                    let mut rule_profiles = config.rule_profiles.clone();
                    // Process names for the rule dialog's live match count.
                    let proc_names: Vec<String> = snapshot.iter().map(|p| p.name.clone()).collect();
                    self.rules_tab.show(
                        ui,
                        ctx,
                        &self.rule_engine,
                        &mut rules_changed,
                        self.opacity,
                        &proc_names,
                        &mut rule_profiles,
                        &mut profiles_changed,
                    );
                    if rules_changed {
                        // Never nest the engine lock inside the state lock —
                        // the daemon nests them the other way around.
                        let rules_cfg = self
                            .rule_engine
                            .lock()
                            .map(|re| re.to_config_list())
                            .unwrap_or_default();
                        if let Ok(mut s) = self.state.lock() {
                            s.config.rules = rules_cfg;
                        }
                        self.send(DaemonCmd::ReapplyDefaults);
                        self.save_config();
                    }
                    if profiles_changed {
                        if let Ok(mut s) = self.state.lock() {
                            s.config.rule_profiles = rule_profiles;
                        }
                        self.save_config();
                    }
                }

                Tab::ProBalance => {
                    if let Some(pb_cfg) = self.probalance_tab.show(ui, &snapshot, &throttle_infos) {
                        if let Ok(mut s) = self.state.lock() {
                            s.config.probalance = pb_cfg.clone();
                        }
                        let mut updated = config.clone();
                        updated.probalance = pb_cfg;
                        self.send(DaemonCmd::UpdateConfig(Box::new(updated)));
                        self.save_config();
                    }
                }

                Tab::GamingMode => {
                    self.gaming_mode_tab.show(ui, ctx, self.opacity);
                    // Drain events
                    let events: Vec<GamingEvent> = std::mem::take(&mut self.gaming_mode_tab.events);
                    for event in events {
                        match event {
                            GamingEvent::GamingModeChanged {
                                active,
                                elevate_nice,
                            } => {
                                self.send(DaemonCmd::SetGamingMode {
                                    active,
                                    elevate_nice,
                                    park: false,
                                });
                                if active {
                                    self.send(DaemonCmd::ReapplyDefaults);
                                }
                            }
                            GamingEvent::ResetAll => {
                                self.send(DaemonCmd::ResetAffinities);
                            }
                            GamingEvent::LogMessage(msg) => {
                                if let Ok(mut s) = self.state.lock() {
                                    s.append_log(msg);
                                }
                            }
                            GamingEvent::ConfigChanged(cfg) => {
                                if let Ok(mut s) = self.state.lock() {
                                    s.config.clone_from(&cfg);
                                }
                                self.send(DaemonCmd::UpdateConfig(cfg));
                                self.save_config();
                            }
                        }
                    }
                }

                Tab::HwMonitor => {
                    self.hw_monitor_tab.show(ui, &hw_monitor);
                    if self.hw_monitor_tab.cols_dirty {
                        let widths = self.hw_monitor_tab.col_widths.to_vec();
                        if let Ok(mut s) = self.state.lock() {
                            s.config.ui.hw_mon_col_widths = widths;
                        }
                        if self.col_save_due() {
                            self.save_config();
                        }
                    }
                }

                Tab::Benchmark => {
                    self.bench_tab.show(ui);
                }

                Tab::Settings => {
                    let config_changed =
                        self.settings_tab
                            .show(ui, ctx, self.opacity, &mut self.updates);

                    // Live opacity preview — apply every frame the slider moves,
                    // regardless of whether the Apply button was clicked.
                    let new_opacity = self.settings_tab.opacity;
                    if (new_opacity - self.opacity).abs() > 0.001 {
                        self.opacity = new_opacity;
                        eprintln!("[opacity] applying opacity={new_opacity:.3}");
                        if let Some(ref wo) = self.wayland_opacity {
                            wo.set(new_opacity);
                        } else {
                            // Fallback: control opacity via window_fill alpha so the
                            // compositor sees a semi-transparent clear colour.
                            let alpha = (new_opacity * 255.0) as u8;
                            let theme = &self.settings_tab.theme;
                            ctx.global_style_mut(|s| {
                                let (r, g, b) = crate::gui::theme::window_bg_rgb(theme);
                                let col = egui::Color32::from_rgba_unmultiplied(r, g, b, alpha);
                                s.visuals.window_fill = col;
                                s.visuals.panel_fill = col;
                            });
                        }
                    }

                    if let Some(updated) = config_changed {
                        if let Ok(mut s) = self.state.lock() {
                            s.config = updated.clone();
                        }
                        // Re-apply full theme (resets window_fill to opaque if needed)
                        crate::gui::theme::apply_theme(
                            ctx,
                            self.native_ppp,
                            &self.settings_tab.theme,
                        );
                        // Then re-apply opacity on top of the fresh theme
                        if let Some(ref wo) = self.wayland_opacity {
                            wo.set(self.opacity);
                        }
                        self.send(DaemonCmd::UpdateConfig(Box::new(updated.clone())));
                        self.send(DaemonCmd::ReapplyDefaults);
                        self.last_saved_opacity = self.settings_tab.opacity;
                        self.last_saved_theme = self.settings_tab.theme.to_str().to_string();
                        self.save_config();
                    }

                    // Detect live theme/opacity changes and persist immediately (no Apply needed)
                    let cur_opacity = self.settings_tab.opacity;
                    let cur_theme = self.settings_tab.theme.to_str().to_string();
                    if (cur_opacity - self.last_saved_opacity).abs() > 0.001
                        || cur_theme != self.last_saved_theme
                    {
                        self.last_saved_opacity = cur_opacity;
                        self.last_saved_theme = cur_theme.clone();
                        if let Ok(mut s) = self.state.lock() {
                            s.config.ui.opacity = cur_opacity;
                            s.config.ui.theme = cur_theme;
                        }
                        self.save_config();
                    }
                }

                Tab::Log => {
                    let (clear, save) = self.log_tab.show_with_clear(ui, &log_lines);
                    if clear {
                        if let Ok(mut s) = self.state.lock() {
                            s.log_lines.clear();
                        }
                    }
                    if save {
                        // Run the picker + write on a background thread — the
                        // dialog subprocess blocks until closed and would
                        // freeze the whole UI (same pattern as rules import).
                        let content = log_lines.iter().cloned().collect::<Vec<_>>().join("\n");
                        let state = self.state.clone();
                        std::thread::spawn(move || {
                            if let Some(p) =
                                crate::file_dialog::save("argus-lasso.log", "*.log *.txt")
                            {
                                let msg = match std::fs::write(&p, content) {
                                    Ok(_) => format!("Log saved to {}", p.display()),
                                    Err(e) => format!("Log save FAILED: {e}"),
                                };
                                if let Ok(mut s) = state.lock() {
                                    s.append_log(msg);
                                }
                            }
                        });
                    }
                }
            }
        });

        // ── --ui-tour capture ───────────────────────────────────────────────
        // Last thing in the frame: the screen is fully laid out by now, so the
        // framebuffer egui hands back is the one this step is documenting.
        if let Some(mut tour) = self.tour.take() {
            let done = tour.tick(ctx, self.proc_count > 0);
            let dir = tour.dir().display().to_string();
            // Take the failures only once, at the end: draining them every
            // frame threw away every skipped step and made a partial run
            // report as a clean one.
            let failures = if done {
                std::mem::take(&mut tour.failures)
            } else {
                Vec::new()
            };
            self.tour = Some(tour);
            if done {
                let wrote = crate::ui_tour::STEPS.len() - failures.len();
                println!("--ui-tour: wrote {wrote} screens to {dir}");
                for f in &failures {
                    eprintln!("--ui-tour: {f}");
                }
                crate::monitor::shutdown_and_wait(&self.state, &self.cmd_tx);
                std::process::exit(if failures.is_empty() { 0 } else { 1 });
            }
        }

        // ── Restart into a freshly installed update ─────────────────────────
        // Both the banner and the Settings card only set the flag; the exec
        // happens here, after the daemon has restored nices, throttles and
        // parked CPUs. exec() replaces the process image outright, so on_exit
        // never runs — without this, updating with Gaming Mode active would
        // leave CPUs offline and no way to learn what the original nice
        // values were. restart() only returns when it failed.
        if self.updates.restart_requested {
            self.updates.restart_requested = false;
            crate::monitor::shutdown_and_wait(&self.state, &self.cmd_tx);
            // The daemon has stopped for good by now, so a failed exec leaves
            // the window up but no longer monitoring — say so plainly.
            self.updates.message = format!(
                "{} — monitoring has stopped, please quit and start Argus-Lasso again.",
                crate::updater::restart()
            );
        }

        // Repaint when next display refresh is due — avoids continuous 60fps rendering.
        // While a kill countdown is pending, repaint fast enough that the
        // countdown updates and the SIGTERM actually fires near its deadline
        // (with a long refresh interval it could otherwise fire seconds late).
        let repaint_ms = if self.pending_kill.is_some() {
            250
        } else {
            // A 0 (or a hand-edited config with a bogus value) would request a
            // repaint every frame — unbounded 60fps. Clamp to a sane floor.
            config.monitor.display_refresh_interval_ms.max(100)
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(repaint_ms));
    }
}
