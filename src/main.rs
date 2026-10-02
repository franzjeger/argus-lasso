//! Argus-Lasso Linux — Rust edition entry point.

mod app;
mod cgroup;
mod config;
mod cpu_park;
mod fast_proc;
mod file_dialog;
mod game_benchmark;
mod game_library;
mod gui;
mod hw_monitor;
mod icon;
mod logfile;
mod mem_bench;
mod monitor;
mod overlay_toggle;
mod probalance;
mod process_control;
mod rules;
mod sensor_access;
mod sensor_data;
mod ui_tour;
mod updater;
mod utils;
mod wayland_opacity;

use std::sync::{Arc, Mutex};

use clap::Parser;

// ── App icon (embedded at compile time from assets/icon.png via build.rs) ─────

const ICON_RGBA_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon_rgba.bin"));

fn make_icon_rgba() -> Vec<u8> {
    ICON_RGBA_BYTES.to_vec()
}

// ── System tray (KDE/freedesktop StatusNotifierItem via D-Bus) ─────────────────

struct ArgusLassoTray {
    state: Arc<Mutex<monitor::AppState>>,
    cmd_tx: crossbeam_channel::Sender<monitor::DaemonCmd>,
    context: gui::SharedContext,
}

impl ArgusLassoTray {
    /// Whether the icon is on screen, which Close to tray depends on.
    fn set_shown(&self, shown: bool) {
        if let Ok(mut s) = self.state.lock() {
            s.tray_available = shown;
        }
    }
}

/// Convert embedded RGBA bytes to ARGB32 network-byte-order as required by D-Bus SNI.
fn make_tray_icon() -> ksni::Icon {
    let mut data = crate::icon::RGBA.to_vec();
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1); // [R,G,B,A] → [A,R,G,B]
    }
    ksni::Icon {
        width: crate::icon::W as i32,
        height: crate::icon::H as i32,
        data,
    }
}

impl ksni::Tray for ArgusLassoTray {
    fn id(&self) -> String {
        "argus-lasso".into()
    }
    fn icon_name(&self) -> String {
        // Named icon in the system theme (works after `make install`).
        // icon_pixmap() provides the embedded fallback.
        "argus-lasso".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![make_tray_icon()]
    }
    fn title(&self) -> String {
        let avg = self.state.lock().map(|s| s.cpu_avg).unwrap_or(0.0);
        format!("Argus-Lasso  CPU {avg:.0}%")
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let avg = self.state.lock().map(|s| s.cpu_avg).unwrap_or(0.0);
        ksni::ToolTip {
            title: format!("Argus-Lasso — CPU {avg:.0}%"),
            description: "Right-click for options".into(),
            icon_name: String::new(),
            icon_pixmap: vec![make_tray_icon()],
        }
    }

    /// The desktop's tray (its StatusNotifierWatcher) appeared, at start
    /// after Argus or after a panel restart, and shows the icon again.
    fn watcher_online(&self) {
        log::info!("Tray icon shown");
        self.set_shown(true);
    }

    /// The desktop's tray went away or was not there yet. The service waits
    /// for it to come back instead of ending: giving up left Argus without an
    /// icon for the whole session whenever it started before the panel.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        log::warn!("Tray icon not shown until the desktop's tray returns: {reason:?}");
        self.set_shown(false);
        true
    }

    /// Left click on the tray icon.
    fn activate(&mut self, _x: i32, _y: i32) {
        gui::request_main_window(&self.state, &self.context);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let gaming_active = self.state.lock().map(|s| s.gaming_active).unwrap_or(false);

        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Open Argus-Lasso".into(),
                activate: Box::new(|tray: &mut Self| {
                    gui::request_main_window(&tray.state, &tray.context);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Checkmark(ksni::menu::CheckmarkItem {
                label: "Gaming Mode".into(),
                checked: gaming_active,
                activate: Box::new(|tray: &mut Self| {
                    let currently = tray.state.lock().map(|s| s.gaming_active).unwrap_or(false);
                    let _ = tray.cmd_tx.send(monitor::DaemonCmd::SetGamingMode {
                        active: !currently,
                        elevate_nice: true,
                        parking: monitor::Parking::NonPreferred,
                    });
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    // Let the GUI close normally so pending Undo actions and
                    // debounced settings are flushed before daemon shutdown.
                    if let Ok(mut s) = tray.state.lock() {
                        s.quit_requested = true;
                    }
                    if let Ok(context) = tray.context.lock() {
                        if let Some(ctx) = context.as_ref() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            ctx.request_repaint();
                        }
                    }
                }),
                ..Default::default()
            }),
        ]
    }
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Report the app identity for matched app/layer packaging.
    BuildInfo,
    /// Restore the previous app and overlay (close Argus first).
    RollbackUpdate,
    /// Toggle a per-frame game capture without starting another daemon.
    Record {
        #[arg(long, default_value_t = 60)]
        seconds: u32,
    },
    /// Kill a process by PID (sends SIGTERM, or SIGKILL with --force)
    Kill {
        /// PID to kill
        pid: u32,
        /// Use SIGKILL instead of SIGTERM
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Set CPU affinity for a process by PID
    SetAffinity {
        /// PID to modify
        pid: u32,
        /// CPU list, e.g. "0-7" or "0,2,4"
        mask: String,
    },
    /// Toggle the visibility of the overlay HUD
    ToggleOverlay,
    /// Install or update the CPU control helpers (parking, power profile,
    /// renice). pkexec asks for authentication: in a dialog on the desktop,
    /// or in this terminal over SSH.
    InstallHelpers,
    /// Print a JSON status snapshot (system + top processes) and exit
    Status {
        /// Include only the top N processes by CPU (0 = all)
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "argus-lasso",
    version,
    about = "Argus-Lasso — Linux process manager"
)]
struct Args {
    /// Start minimised to system tray
    #[arg(long, default_value_t = false)]
    minimized: bool,

    /// Disable system tray icon
    #[arg(long, default_value_t = false)]
    no_tray: bool,

    /// Developer aid: walk every screen, write a PNG of each to DIR, and
    /// exit. Used to regenerate the README screenshots consistently.
    #[arg(long, value_name = "DIR", hide = true)]
    ui_tour: Option<std::path::PathBuf>,

    /// Theme for the --ui-tour captures, by its name in the configuration.
    /// Without it the tour uses the configured theme.
    #[arg(long, value_name = "THEME", hide = true, requires = "ui_tour",
          value_parser = clap::builder::PossibleValuesParser::new(gui::theme::AppTheme::NAMES))]
    tour_theme: Option<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

/// Acquire a process-wide single-instance lock via flock(2).
///
/// Argus runs as a `--minimized` tray service *and* can be launched from the
/// app menu. Without a guard, two instances each hold their own copy of the
/// config and race to write `config.toml`, so one instance silently reverts
/// the other's changes (e.g. a deleted rule reappears). The returned lock is
/// held for the process lifetime; `Ok(None)` means another instance owns it.
fn acquire_single_instance_lock() -> std::io::Result<Option<nix::fcntl::Flock<std::fs::File>>> {
    // Both candidates are private to this user. A fixed name in the shared
    // temp directory could be created first by another user, and every
    // start would then report "already running".
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(config::config_dir);
    lock_instance_in(&dir)
}

fn lock_instance_in(
    dir: &std::path::Path,
) -> std::io::Result<Option<nix::fcntl::Flock<std::fs::File>>> {
    use nix::fcntl::{Flock, FlockArg};
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("argus-lasso.lock"))?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, errno)) => Err(errno.into()),
    }
}

fn main() {
    env_logger::init();

    let args = Args::parse();

    // Handle CLI subcommands — run action and exit without launching the GUI.
    if let Some(cmd) = args.command {
        match cmd {
            Cmd::BuildInfo => {
                println!(
                    "{}",
                    serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "build_id": argus_ipc::BUILD_ID,
                    "protocol": argus_ipc::PROTOCOL_VERSION, "arch": std::env::consts::ARCH })
                );
                return;
            }
            Cmd::RollbackUpdate => {
                let _lock = match acquire_single_instance_lock() {
                    Ok(Some(lock)) => lock,
                    Ok(None) => {
                        eprintln!("Close Argus before rollback.");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Cannot confirm Argus is closed ({e}); rollback cancelled.");
                        std::process::exit(1);
                    }
                };
                match updater::rollback_update() {
                    Ok(true) => {
                        println!("Previous app and overlay restored. Restart Argus and games.")
                    }
                    Ok(false) => {
                        eprintln!("No previous installation is available.");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Rollback failed: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            Cmd::Record { seconds } => {
                match argus_ipc::capture::toggle(seconds) {
                    Ok(c) => println!(
                        "{}",
                        if c.active {
                            "Recording requested"
                        } else {
                            "Recording stopped"
                        }
                    ),
                    Err(e) => {
                        eprintln!("Recording failed: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            Cmd::Kill { pid, force } => {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                let sig = if force {
                    Signal::SIGKILL
                } else {
                    Signal::SIGTERM
                };
                match signal::kill(Pid::from_raw(pid as i32), sig) {
                    Ok(_) => println!("{}illed PID {pid}", if force { "Force k" } else { "K" }),
                    Err(e) => {
                        eprintln!("Kill failed: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            Cmd::SetAffinity { pid, mask } => {
                match utils::set_affinity(pid, &mask) {
                    Ok(()) => println!("Affinity set to '{mask}' for PID {pid}"),
                    Err(e) => {
                        eprintln!("Failed to set affinity for PID {pid}: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            Cmd::Status { top } => {
                let mut procs = monitor::oneshot_snapshot();
                procs.sort_by(|a, b| {
                    b.cpu_percent
                        .partial_cmp(&a.cpu_percent)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.pid.cmp(&b.pid))
                });
                let total_count = procs.len();
                if top > 0 {
                    procs.truncate(top);
                }
                let load = std::fs::read_to_string("/proc/loadavg")
                    .ok()
                    .map(|s| {
                        s.split_whitespace()
                            .take(3)
                            .filter_map(|v| v.parse::<f64>().ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut offline: Vec<u32> = utils::get_offline_cpus().into_iter().collect();
                offline.sort_unstable();
                let json = serde_json::json!({
                    "cpu_percent_scale": "share_of_total_available_capacity",
                    "cpu_model": monitor::read_cpu_model(),
                    "cpus_online": utils::get_online_cpus().len(),
                    "cpus_offline": offline,
                    "load_avg": load,
                    "process_count": total_count,
                    "processes": procs.iter().map(|p| serde_json::json!({
                        "pid": p.pid,
                        "name": p.name.as_ref(),
                        "cpu_percent": (p.cpu_percent as f64 * 10.0).round() / 10.0,
                        "mem_bytes": p.mem_rss,
                        "nice": p.nice,
                        "affinity": p.affinity.as_ref(),
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
                return;
            }
            Cmd::ToggleOverlay => {
                if let Err(e) = overlay_toggle::request(&config::config_dir()) {
                    eprintln!("Failed to request toggle: {e}");
                    std::process::exit(1);
                }
                println!("Overlay toggle requested.");
                return;
            }
            Cmd::InstallHelpers => {
                let (installed, message) = cpu_park::install_helper_via_pkexec();
                println!("{message}");
                if !installed {
                    std::process::exit(1);
                }
                return;
            }
        }
    }

    // ── Single-instance guard ──────────────────────────────────────────────
    // Prevents a second instance (e.g. launched from the app menu while the
    // tray service runs) from clobbering config.toml. Held for the whole
    // process lifetime; released automatically on exit.
    // --ui-tour is a throwaway render pass that never writes config, so it
    // is exempt: requiring the lock would mean stopping the running instance
    // just to re-take screenshots.
    let _instance_lock = if args.ui_tour.is_some() {
        None
    } else {
        match acquire_single_instance_lock() {
            Ok(Some(lock)) => {
                // Requests left while no instance ran are not for this one:
                // a --minimized start must not come up showing because of
                // an old launch.
                overlay_toggle::drain_show_window(&config::config_dir());
                Some(lock)
            }
            Ok(None) => {
                // Launching from the app menu while the tray service runs
                // should bring up the existing window, not do nothing. A
                // --minimized launch is a session start, which has nothing
                // to show.
                if args.minimized {
                    eprintln!("Argus-Lasso is already running.");
                } else {
                    match overlay_toggle::request_show_window(&config::config_dir()) {
                        Ok(()) => eprintln!("Argus-Lasso is already running; showing its window."),
                        Err(e) => eprintln!(
                            "Argus-Lasso is already running, and could not be asked to show its window: {e}"
                        ),
                    }
                }
                return;
            }
            // Not being able to create the lock is not evidence of another
            // instance; say what is unprotected and carry on.
            Err(e) => {
                eprintln!(
                    "Warning: no single-instance lock ({e}); a second instance \
                     could overwrite this one's settings."
                );
                None
            }
        }
    };

    if args.ui_tour.is_none() {
        match updater::recover_pending_update() {
            Ok(true) => {
                eprintln!("Recovered interrupted update. {}", updater::restart());
                std::process::exit(1);
            }
            Ok(false) => {}
            // Exiting here failed every start (a restart loop under
            // systemd) with no way out; the running binary is complete.
            Err(e) => eprintln!("Warning: update recovery skipped: {e}"),
        }
    }

    // Build icon RGBA once; reused for window decoration icon.
    let icon_rgba = make_icon_rgba();

    // Load config
    // The tour renders a throwaway session: it must not write a config,
    // which migrating an old one would.
    let (mut cfg, load_error) = config::load(args.ui_tour.is_none());
    if args.ui_tour.is_some() {
        ui_tour::prepare_config(&mut cfg, args.tour_theme.as_deref());
    }
    // Set the unreadable file aside before anything can save over it — but
    // never from the read-only tour, which must not write configuration.
    let load_notice = match load_error {
        Some(error) if args.ui_tour.is_none() => Some(config::preserve_unreadable(&error)),
        Some(error) => Some(format!("Settings {error}. Defaults are in use.")),
        None => None,
    };

    // Termination signals are taken by one thread, below: blocked here,
    // before any other thread starts, so every thread inherits the mask.
    let stop_signals = stop_signal_set();
    if args.ui_tour.is_none() {
        if let Err(e) = stop_signals.thread_block() {
            eprintln!("Warning: could not take termination signals ({e}); stopping the service will not restore CPUs and priorities.");
        }
    }

    // The app's log also goes to a file. The throwaway tour's does not, and
    // nothing else turns it on, so tests log in memory only.
    if args.ui_tour.is_none() {
        if let Some(path) = logfile::default_path() {
            logfile::enable(path);
        }
    }

    // Build shared state
    let state = Arc::new(Mutex::new(monitor::AppState::default()));
    {
        if let Ok(mut s) = state.lock() {
            s.config = cfg.clone();
            s.cpu_model = monitor::read_cpu_model();
            if let Some(notice) = load_notice {
                s.append_log(notice.clone());
                s.operation_error = Some(notice);
            }
        }
    }

    // Build rule engine
    let rule_engine = {
        let mut re = rules::RuleEngine::new();
        re.load_rules(&cfg.rules);
        Arc::new(Mutex::new(re))
    };

    // Spawn daemon thread. Keep the JoinHandle: on exit we join it (bounded)
    // so a daemon stuck mid-restore is detected and logged rather than
    // silently abandoned when the process image is torn down.
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    // Filled in once eframe creates the context; the tray and the monitor use
    // it to show the window on request.
    let gui_context: gui::SharedContext = Arc::new(Mutex::new(None));
    let daemon_handle = if args.ui_tour.is_some() {
        monitor::spawn_preview(Arc::clone(&state), cmd_rx)
    } else {
        monitor::spawn(
            Arc::clone(&state),
            cmd_rx,
            cfg.clone(),
            Arc::clone(&rule_engine),
            Arc::clone(&gui_context),
        )
    };

    if args.ui_tour.is_none() {
        spawn_stop_signal_handler(
            stop_signals,
            Arc::clone(&state),
            cmd_tx.clone(),
            Arc::clone(&gui_context),
        );
    }

    // System tray via D-Bus StatusNotifierItem (KDE/freedesktop, no libxdo).
    // Spawned after state + cmd_tx exist so the menu can read/toggle gaming mode.
    let _tray_handle = if !args.no_tray && args.ui_tour.is_none() {
        use ksni::blocking::TrayMethods;
        // Shown unless the tray says otherwise while it registers.
        if let Ok(mut s) = state.lock() {
            s.tray_available = true;
        }
        match (ArgusLassoTray {
            state: Arc::clone(&state),
            cmd_tx: cmd_tx.clone(),
            context: Arc::clone(&gui_context),
        })
        .assume_sni_available(true)
        .spawn()
        {
            Ok(h) => {
                // Shown or waiting for the desktop's tray; the tray logs which.
                log::info!("Tray icon service started");
                Some(h)
            }
            Err(e) => {
                log::warn!("Tray icon unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    if _tray_handle.is_none() {
        if let Ok(mut s) = state.lock() {
            s.tray_available = false;
        }
    }

    // Launch GUI
    // transparent: true enables per-pixel alpha compositing on Wayland/X11 so the
    // fallback opacity path (ctx.visuals window_fill alpha) works when the compositor
    // does not support wp_alpha_modifier_v1.
    let window_icon = egui::IconData {
        rgba: icon_rgba,
        width: crate::icon::W,
        height: crate::icon::H,
    };

    let native_options = |visible: bool| eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Argus-Lasso — Linux")
            // app_id must match the .desktop filename (argus-lasso.desktop)
            // so KDE/KWin resolves Icon=argus-lasso from that file.
            .with_app_id("argus-lasso")
            // The tour pins a size so successive runs produce directly
            // comparable images instead of whatever the compositor last used.
            .with_inner_size(if args.ui_tour.is_some() {
                [1400.0, 900.0]
            } else {
                [1100.0, 700.0]
            })
            .with_min_inner_size([800.0, 500.0])
            .with_transparent(true)
            .with_visible(visible)
            .with_icon(window_icon.clone()),
        // The tour renders into the X server in $DISPLAY — Xvfb under
        // xvfb-run. winit prefers Wayland whenever $WAYLAND_DISPLAY is set,
        // which put the tour window on the user's desktop instead, where a
        // hidden window gets no frames and the tour waits forever.
        event_loop_builder: args
            .ui_tour
            .is_some()
            .then(|| -> eframe::EventLoopBuilderHook {
                Box::new(|builder| {
                    use winit::platform::x11::EventLoopBuilderExtX11;
                    builder.with_x11();
                })
            }),
        ..Default::default()
    };

    // eframe keeps its event loop between runs, so a window closed to the
    // tray is reopened by running it again: a new window with a new app on
    // the current configuration. Hiding the old one instead is not possible
    // on Wayland, where winit cannot unmap a window once shown.
    let mut visible = !args.minimized;
    let updates_carry = app::UpdatesCarry::default();
    loop {
        let state_gui = Arc::clone(&state);
        let re_gui = Arc::clone(&rule_engine);
        let cfg_gui = state
            .lock()
            .map(|s| s.config.clone())
            .unwrap_or_else(|_| cfg.clone());
        let cmd_tx_gui = cmd_tx.clone();
        let tour_dir = args.ui_tour.clone();
        let context_gui = Arc::clone(&gui_context);
        let carry_gui = updates_carry.clone();
        let run = eframe::run_native(
            "Argus-Lasso",
            native_options(visible),
            Box::new(move |cc| {
                if let Ok(mut context) = context_gui.lock() {
                    *context = Some(cc.egui_ctx.clone());
                }
                Ok(Box::new(app::ArgusLassoApp::new(
                    cc, state_gui, cmd_tx_gui, re_gui, cfg_gui, tour_dir, carry_gui,
                )))
            }),
        );
        // Losing the display (logout, a compositor crash) ends the window with
        // an error, perhaps before on_exit ran. Panicking here ended the process
        // in the middle of the restore a stop signal had started; restore, then
        // exit with an error so systemd restarts the app after a crash.
        if let Err(e) = run {
            eprintln!("Argus-Lasso's window failed: {e}");
            monitor::shutdown_and_wait(&state, &cmd_tx);
            monitor::join_daemon(daemon_handle, std::time::Duration::from_secs(2));
            std::process::exit(1);
        }
        if let Ok(mut context) = gui_context.lock() {
            *context = None;
        }
        if !wait_for_window_request(&state) {
            break;
        }
        visible = true;
    }

    // The window closed for good. Its on_exit asked the daemon to restore
    // state, unless it had closed to the tray and Quit came from there.
    if !state.lock().map(|s| s.shutdown_complete).unwrap_or(true) {
        monitor::shutdown_and_wait(&state, &cmd_tx);
    }
    // Give the daemon a bounded grace period to finish (unparking CPUs,
    // restoring nices) rather than tearing down the process image mid-restore.
    if !monitor::join_daemon(daemon_handle, std::time::Duration::from_secs(2)) {
        log::warn!("daemon thread did not finish within 2s of exit; it may be stuck mid-restore");
    }
}

/// After the window has closed: when it closed to the tray, wait until the
/// tray or a second launch asks for a window (true) or something asks Argus
/// to quit (false). A window that closed for good returns false at once.
fn wait_for_window_request(state: &Arc<Mutex<monitor::AppState>>) -> bool {
    loop {
        match state.lock() {
            Ok(s) if !s.window_closed || s.quit_requested => return false,
            Ok(mut s) if s.window_wanted => {
                s.window_closed = false;
                s.window_wanted = false;
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// SIGTERM (systemctl stop, logout), SIGINT (Ctrl+C) and SIGHUP (the
/// terminal going away).
fn stop_signal_set() -> nix::sys::signal::SigSet {
    use nix::sys::signal::{SigSet, Signal};
    let mut set = SigSet::empty();
    for signal in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        set.add(signal);
    }
    set
}

/// Stop the way the tray's Quit does when a termination signal arrives,
/// instead of dying with CPUs parked and priorities changed: the window
/// closes, flushes its settings and has the monitor restore everything.
/// If the window cannot (not created yet, or stuck), the restore is done
/// from here and the process exits.
fn spawn_stop_signal_handler(
    signals: nix::sys::signal::SigSet,
    state: Arc<Mutex<monitor::AppState>>,
    cmd_tx: crossbeam_channel::Sender<monitor::DaemonCmd>,
    context: gui::SharedContext,
) {
    let spawned = std::thread::Builder::new()
        .name("stop-signals".into())
        .spawn(move || {
            let Ok(signal) = signals.wait() else {
                return;
            };
            log::info!("{signal:?} received; restoring state and exiting");
            if let Ok(mut s) = state.lock() {
                s.quit_requested = true;
            }
            let window = context.lock().ok().and_then(|context| context.clone());
            if let Some(ctx) = window {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
                // The window's exit restores state; the process ends when
                // it is done. Step in only if that does not happen.
                let restored = || state.lock().map(|s| s.shutdown_complete).unwrap_or(true);
                for _ in 0..150 {
                    if restored() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            monitor::shutdown_and_wait(&state, &cmd_tx);
            std::process::exit(0);
        });
    if let Err(e) = spawned {
        eprintln!("Warning: no termination signal handler ({e}).");
    }
}

#[cfg(test)]
mod tests {
    use super::lock_instance_in;

    /// The desktop's tray can start after Argus, or go away with a panel
    /// restart. The icon used to give up for the session; now it waits, and
    /// Close to tray follows whether it is shown.
    #[test]
    fn the_tray_follows_the_desktop_tray_as_it_comes_and_goes() {
        use super::{monitor::AppState, ArgusLassoTray};
        use ksni::Tray as _;
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new(AppState {
            tray_available: true,
            ..Default::default()
        }));
        let (cmd_tx, _cmd_rx) = crossbeam_channel::unbounded();
        let tray = ArgusLassoTray {
            state: Arc::clone(&state),
            cmd_tx,
            context: Default::default(),
        };
        assert!(
            tray.watcher_offline(ksni::OfflineReason::No),
            "the icon waits for the tray to return"
        );
        assert!(!state.lock().unwrap().tray_available);
        tray.watcher_online();
        assert!(state.lock().unwrap().tray_available);
    }

    /// Closed to the tray, main waits for a window request; anything else
    /// ends the window loop.
    #[test]
    fn a_window_closed_to_the_tray_reopens_only_when_asked() {
        use super::{monitor::AppState, wait_for_window_request};
        use std::sync::{Arc, Mutex};
        let state = |closed, wanted, quit| {
            Arc::new(Mutex::new(AppState {
                window_closed: closed,
                window_wanted: wanted,
                quit_requested: quit,
                ..Default::default()
            }))
        };
        assert!(
            !wait_for_window_request(&state(false, false, false)),
            "closed for good"
        );
        assert!(
            !wait_for_window_request(&state(true, true, true)),
            "Quit wins"
        );

        let wanted = state(true, true, false);
        assert!(wait_for_window_request(&wanted));
        let s = wanted.lock().unwrap();
        assert!(
            !s.window_closed && !s.window_wanted,
            "spent on the new window"
        );
    }

    #[test]
    fn a_second_lock_reports_another_instance() {
        let dir = std::env::temp_dir().join(format!("argus-lock-{}", uuid::Uuid::new_v4()));
        let first = lock_instance_in(&dir).unwrap();
        assert!(first.is_some());
        assert!(lock_instance_in(&dir).unwrap().is_none());
        drop(first);
        assert!(lock_instance_in(&dir).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Failing to create the lock is not evidence of another instance, and
    /// must not be reported as one.
    #[test]
    fn an_unusable_directory_is_an_error_not_another_instance() {
        let file = std::env::temp_dir().join(format!("argus-lock-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"").unwrap();
        assert!(lock_instance_in(&file.join("sub")).is_err());
        std::fs::remove_file(file).unwrap();
    }
}
