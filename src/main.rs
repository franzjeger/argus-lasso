//! Argus-Lasso Linux — Rust edition entry point.

mod app;
mod cgroup;
mod config;
mod cpu_park;
mod fast_proc;
mod file_dialog;
mod game_benchmark;
mod gui;
mod hw_monitor;
mod icon;
mod logfile;
mod mem_bench;
mod monitor;
mod probalance;
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

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let gaming_active = self.state.lock().map(|s| s.gaming_active).unwrap_or(false);

        vec![
            ksni::MenuItem::Checkmark(ksni::menu::CheckmarkItem {
                label: "Gaming Mode".into(),
                checked: gaming_active,
                activate: Box::new(|tray: &mut Self| {
                    let currently = tray.state.lock().map(|s| s.gaming_active).unwrap_or(false);
                    let _ = tray.cmd_tx.send(monitor::DaemonCmd::SetGamingMode {
                        active: !currently,
                        elevate_nice: true,
                        park: true,
                    });
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    // Ask the daemon to restore everything (nices, throttles,
                    // parked CPUs), then wait for its completion flag instead
                    // of sleeping a fixed interval.
                    monitor::shutdown_and_wait(&tray.state, &tray.cmd_tx);
                    std::process::exit(0);
                }),
                ..Default::default()
            }),
        ]
    }
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
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

    #[command(subcommand)]
    command: Option<Cmd>,
}

/// Acquire a process-wide single-instance lock via flock(2).
///
/// Argus runs as a `--minimized` tray service *and* can be launched from the
/// app menu. Without a guard, two instances each hold their own copy of the
/// config and race to write `config.toml`, so one instance silently reverts
/// the other's changes (e.g. a deleted rule reappears). The returned lock is
/// held for the process lifetime; `None` means another instance already owns it.
fn acquire_single_instance_lock() -> Option<nix::fcntl::Flock<std::fs::File>> {
    use nix::fcntl::{Flock, FlockArg};
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("argus-lasso.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).ok()
}

fn main() {
    env_logger::init();

    let args = Args::parse();

    // Handle CLI subcommands — run action and exit without launching the GUI.
    if let Some(cmd) = args.command {
        match cmd {
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
                if utils::set_affinity(pid, &mask) {
                    println!("Affinity set to '{mask}' for PID {pid}");
                } else {
                    eprintln!("Failed to set affinity for PID {pid}");
                    std::process::exit(1);
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
                        "name": p.name,
                        "cpu_percent": (p.cpu_percent as f64 * 10.0).round() / 10.0,
                        "mem_bytes": p.mem_rss,
                        "nice": p.nice,
                        "affinity": p.affinity,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
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
            Some(lock) => Some(lock),
            None => {
                eprintln!("Argus-Lasso is already running; exiting this instance.");
                return;
            }
        }
    };

    // Build icon RGBA once; reused for window decoration icon.
    let icon_rgba = make_icon_rgba();

    // Load config
    let cfg = config::load();

    // Build shared state
    let state = Arc::new(Mutex::new(monitor::AppState::default()));
    {
        if let Ok(mut s) = state.lock() {
            s.config = cfg.clone();
            s.cpu_model = monitor::read_cpu_model();
        }
    }

    // Build rule engine
    let rule_engine = {
        let mut re = rules::RuleEngine::new();
        let state_clone = state.clone();
        re.set_log_callback(move |msg| {
            if let Ok(mut s) = state_clone.lock() {
                s.append_log(msg);
            }
        });
        re.load_rules(&cfg.rules);
        Arc::new(Mutex::new(re))
    };

    // Spawn daemon thread. Keep the JoinHandle: on exit we join it (bounded)
    // so a daemon stuck mid-restore is detected and logged rather than
    // silently abandoned when the process image is torn down.
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let daemon_handle = if args.ui_tour.is_some() {
        monitor::spawn_preview(Arc::clone(&state), cmd_rx)
    } else {
        monitor::spawn(
            Arc::clone(&state),
            cmd_rx,
            cfg.clone(),
            Arc::clone(&rule_engine),
        )
    };

    // System tray via D-Bus StatusNotifierItem (KDE/freedesktop, no libxdo).
    // Spawned after state + cmd_tx exist so the menu can read/toggle gaming mode.
    let _tray_handle = if !args.no_tray && args.ui_tour.is_none() {
        use ksni::blocking::TrayMethods;
        match (ArgusLassoTray {
            state: Arc::clone(&state),
            cmd_tx: cmd_tx.clone(),
        })
        .spawn()
        {
            Ok(h) => {
                log::info!("SNI tray icon registered");
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

    // Launch GUI
    // transparent: true enables per-pixel alpha compositing on Wayland/X11 so the
    // fallback opacity path (ctx.visuals window_fill alpha) works when the compositor
    // does not support wp_alpha_modifier_v1.
    let window_icon = egui::IconData {
        rgba: icon_rgba,
        width: crate::icon::W,
        height: crate::icon::H,
    };

    let native_options = eframe::NativeOptions {
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
            .with_visible(!args.minimized)
            .with_icon(window_icon),
        ..Default::default()
    };

    let state_gui = Arc::clone(&state);
    let re_gui = Arc::clone(&rule_engine);
    let cfg_gui = cfg.clone();
    let cmd_tx_gui = cmd_tx.clone();
    let tour_dir = args.ui_tour.clone();

    eframe::run_native(
        "Argus-Lasso",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::ArgusLassoApp::new(
                cc, state_gui, cmd_tx_gui, re_gui, cfg_gui, tour_dir,
            )))
        }),
    )
    .expect("eframe launch failed");

    // The window is closed and on_exit already asked the daemon to restore
    // state (shutdown_and_wait). Give it a bounded grace period to finish
    // (unparking CPUs, restoring nices) rather than tearing down the process
    // image mid-restore.
    if !monitor::join_daemon(daemon_handle, std::time::Duration::from_secs(2)) {
        log::warn!("daemon thread did not finish within 2s of exit; it may be stuck mid-restore");
    }
}
