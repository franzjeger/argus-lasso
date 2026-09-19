//! Monitor daemon thread: process scanning, rule enforcement, ProBalance.
//!
//! Mirrors Python monitor.py MonitorThread:
//!   - 0.5s base tick (bounded by rule_enforce_interval_ms)
//!   - Every 0.5s (rule_enforce_interval_ms): enforce all rules on running processes
//!   - Every 1.0s: ProBalance tick
//!   - Every 2.0s (display_refresh_interval_ms): update AppState snapshot
//!   - New PIDs: apply matching rules or default affinity
//!   - Gaming Mode: nice -1 via helper for rule-matched processes
//!   - Manual affinity override: 30s suppression after user sets affinity

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::config::Config;
use crate::cpu_park;
use crate::hw_monitor::{HwCollector, HwMonitorData};
use crate::probalance::{ProBalance, ProcSnapshot};
use crate::rules::RuleEngine;
use crate::utils;

// ── Commands from GUI → daemon ────────────────────────────────────────────────

#[derive(Debug)]
pub enum DaemonCmd {
    UpdateConfig(Box<Config>),
    GameLaunched {
        pid: u32,
        profile: String,
    },
    SetGamingMode {
        active: bool,
        elevate_nice: bool,
        park: bool,
    },
    SetManualOverride {
        pid: u32,
        duration_secs: f64,
    },
    ResetAffinities,
    ReapplyDefaults,
    /// Restore everything we changed (nices, throttles, parked CPUs) before
    /// the process exits; sets AppState::shutdown_complete when done.
    Shutdown,
}

// ── Shared state (GUI reads this) ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: std::sync::Arc<str>,
    /// Share of total available CPU time (0-100%, including multicore processes).
    pub cpu_percent: f32,
    pub gpu_percent: f32,
    pub mem_rss: u64, // bytes
    pub nice: i32,
    pub affinity: std::sync::Arc<str>,
    pub ionice: std::sync::Arc<str>,
    pub disk_read_bps: u64,  // bytes/s
    pub disk_write_bps: u64, // bytes/s
    pub cmdline: std::sync::Arc<String>,
}

impl Default for ProcInfo {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            name: "".into(),
            cpu_percent: 0.0,
            gpu_percent: 0.0,
            mem_rss: 0,
            nice: 0,
            affinity: "".into(),
            ionice: "".into(),
            disk_read_bps: 0,
            disk_write_bps: 0,
            cmdline: std::sync::Arc::new(String::new()),
        }
    }
}

#[derive(Debug, Default)]
pub struct AppState {
    /// Behind an Arc so publishing and reading are both pointer copies. It
    /// used to be deep-cloned twice per display tick — once into this struct
    /// and once out of it by the GUI — which is four allocations per process,
    /// per clone, for data neither side mutates.
    pub snapshot: std::sync::Arc<Vec<ProcInfo>>,
    /// Per-CPU utilisation % (indexed by cpu number, parked CPUs = 0.0)
    pub cpu_percents: Vec<f32>,
    /// Monotonic counter incremented each time cpu_percents is updated by the daemon.
    /// GUI tracks this to avoid pushing duplicate samples to the history widget.
    pub cpu_generation: u64,
    /// Rolling average CPU history (120 samples)
    pub cpu_history: std::collections::VecDeque<f32>,
    /// Rolling totals across all disks: (read MiB/s, write MiB/s), 120 samples
    pub disk_io_history: std::collections::VecDeque<(f32, f32)>,
    /// Rolling totals across all NICs: (rx MiB/s, tx MiB/s), 120 samples
    pub net_io_history: std::collections::VecDeque<(f32, f32)>,
    /// Throttled PID set from ProBalance
    pub throttled_pids: HashSet<u32>,
    /// Detailed throttle info for ProBalance tab live view
    pub throttle_infos: Vec<crate::probalance::ThrottleInfo>,
    /// Log lines ring buffer (max 2000)
    pub log_lines: std::collections::VecDeque<String>,
    /// Current config (read by GUI for settings display)
    pub config: Config,
    /// Is Gaming Mode currently active?
    pub gaming_active: bool,
    /// Hardware sensor data (updated every display_refresh_interval)
    pub hw_monitor: HwMonitorData,
    /// System-wide average CPU % (used by tray tooltip)
    pub cpu_avg: f32,
    /// Per-PID CPU usage history (last 30 samples)
    pub proc_cpu_history: HashMap<u32, std::collections::VecDeque<f32>>,
    /// CPU model string from /proc/cpuinfo
    pub cpu_model: String,
    /// PIDs manually suspended via SIGSTOP from the GUI
    pub suspended_pids: std::collections::HashSet<u32>,
    /// Set by the daemon once a Shutdown command has finished restoring state
    pub shutdown_complete: bool,
    /// Notable events (throttles, alerts, gaming mode, kills) for the
    /// status-bar notification center — small ring buffer, newest last.
    pub notable_events: std::collections::VecDeque<String>,
}

pub fn read_cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':').map(|x| x.1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "Unknown CPU".to_string())
}

impl AppState {
    pub fn append_log(&mut self, msg: String) {
        let ts = chrono_ts();
        let line = format!("[{ts}] {msg}");
        crate::logfile::append(&line);
        // Feed the status-bar notification center with the events a user
        // actually wants surfaced (not routine rule/default churn).
        const NOTABLE: &[&str] = &[
            "[ProBalance] THROTTLE",
            "[ProBalance] RESTORE",
            "[HW Alert]",
            "[Gaming Mode]",
            "[Shutdown]",
            "illed ", // "Killed" / "Force killed"
            "[Park]",
            "[Power]",
        ];
        if NOTABLE.iter().any(|m| msg.contains(m)) {
            self.notable_events.push_back(line.clone());
            while self.notable_events.len() > 50 {
                self.notable_events.pop_front();
            }
        }
        self.log_lines.push_back(line);
        while self.log_lines.len() > 2000 {
            self.log_lines.pop_front();
        }
    }
}

fn chrono_ts() -> String {
    // HH:MM:SS in local time via libc::localtime_r
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    unsafe { nix::libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

// ── Daemon thread ─────────────────────────────────────────────────────────────

/// Ask the daemon to restore everything it changed (nices, throttles, parked
/// CPUs) and wait briefly for it to report back.
///
/// Every path that ends this process image must go through here — window
/// close, tray Quit, and the updater's `exec` restart. Skipping it leaves
/// parked CPUs offline and throttled processes at a raised nice, with the
/// original values lost: they live only in this process's memory.
pub fn shutdown_and_wait(state: &Arc<Mutex<AppState>>, cmd_tx: &Sender<DaemonCmd>) {
    let _ = cmd_tx.send(DaemonCmd::Shutdown);
    for _ in 0..30 {
        // A poisoned lock means the daemon is already gone; don't hang on it.
        let done = state.lock().map(|s| s.shutdown_complete).unwrap_or(true);
        if done {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log::warn!("daemon did not confirm shutdown within 3s; continuing anyway");
}

// ── Overlay IPC Server ────────────────────────────────────────────────────────

mod ipc_server {
    use argus_ipc::IpcMessage;
    use std::io::Write;
    use std::os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    };
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;
    #[derive(Default)]
    struct Latest {
        config: Option<Vec<u8>>,
        telemetry: Option<Vec<u8>>,
        generation: u64,
    }
    pub struct Broadcaster {
        latest: Arc<(Mutex<Latest>, Condvar)>,
    }
    impl Broadcaster {
        pub fn start() -> Self {
            let latest = Arc::new((Mutex::new(Latest::default()), Condvar::new()));
            for path in argus_ipc::socket_paths() {
                let shared = latest.clone();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(parent);
                }
                // Do not unlink another listener (including an exempt UI-tour instance).
                if UnixStream::connect(&path).is_ok() {
                    log::error!("Overlay socket already has a listener: {}", path.display());
                    continue;
                }
                // The process-wide flock is held before the monitor starts.
                let _ = std::fs::remove_file(&path);
                match UnixListener::bind(&path) {
                    Ok(listener) => {
                        let _ =
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                        log::warn!(
                            "Overlay IPC build={} protocol={} socket={}",
                            argus_ipc::BUILD_ID,
                            argus_ipc::PROTOCOL_VERSION,
                            path.display()
                        );
                        std::thread::spawn(move || {
                            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                            for mut stream in listener.incoming().flatten() {
                                if count.load(std::sync::atomic::Ordering::Relaxed) >= 16 {
                                    continue;
                                }
                                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let count = count.clone();
                                let shared = shared.clone();
                                std::thread::spawn(move || {
                                    let _ =
                                        stream.set_write_timeout(Some(Duration::from_millis(250)));
                                    let mut cred: nix::libc::ucred = unsafe { std::mem::zeroed() };
                                    let mut len =
                                        std::mem::size_of_val(&cred) as nix::libc::socklen_t;
                                    use std::os::fd::AsRawFd;
                                    let ok = unsafe {
                                        nix::libc::getsockopt(
                                            stream.as_raw_fd(),
                                            nix::libc::SOL_SOCKET,
                                            nix::libc::SO_PEERCRED,
                                            &mut cred as *mut _ as *mut _,
                                            &mut len,
                                        )
                                    } == 0;
                                    let hello = IpcMessage::Hello {
                                        build: argus_ipc::BUILD_ID.into(),
                                        protocol: argus_ipc::PROTOCOL_VERSION,
                                        host_pid: if ok { cred.pid as u32 } else { 0 },
                                    };
                                    if argus_ipc::write_message(&mut stream, &hello).is_ok() {
                                        let mut generation = u64::MAX;
                                        loop {
                                            let (lock, changed) = &*shared;
                                            let mut latest = lock.lock().unwrap();
                                            while latest.generation == generation {
                                                latest = changed.wait(latest).unwrap();
                                            }
                                            generation = latest.generation;
                                            let packets =
                                                [latest.config.clone(), latest.telemetry.clone()];
                                            drop(latest);
                                            if packets
                                                .iter()
                                                .flatten()
                                                .any(|packet| stream.write_all(packet).is_err())
                                            {
                                                break;
                                            }
                                        }
                                    }
                                    count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                                });
                            }
                        });
                    }
                    Err(e) => log::error!("Overlay socket {}: {e}", path.display()),
                }
            }
            Self { latest }
        }
        pub fn broadcast(&self, msg: &IpcMessage) {
            let Ok(packet) = argus_ipc::encode(msg) else {
                return;
            };
            let mut latest = self.latest.0.lock().unwrap();
            match msg {
                IpcMessage::Config(_) => latest.config = Some(packet),
                IpcMessage::Telemetry(_) => latest.telemetry = Some(packet),
                _ => return,
            }
            latest.generation = latest.generation.wrapping_add(1);
            self.latest.1.notify_all();
        }
    }
}

pub fn spawn(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<DaemonCmd>,
    initial_config: Config,
    rule_engine: Arc<Mutex<RuleEngine>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        run_loop(state, cmd_rx, initial_config, rule_engine);
    })
}

/// Read-only snapshots for screenshot QA. Never starts IPC or applies policies.
pub fn spawn_preview(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<DaemonCmd>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut hw = HwCollector::new();
        let mut cpu_sampler = CpuSampler::default();
        let mut times = HashMap::new();
        let mut total = 0;
        let mut caches = SnapshotCaches::default();
        let mut last = Instant::now();
        let mut snapshot = Vec::new();
        loop {
            let elapsed = last.elapsed().as_secs_f32().max(0.001);
            last = Instant::now();
            let (next_times, next_total) =
                collect_snapshot(&mut snapshot, &mut times, total, &mut caches, true, elapsed);
            times = next_times;
            total = next_total;
            hw.update();
            let (mut cpus, cpu_total) = cpu_sampler.sample(read_percpu_stats());
            cpus.resize(utils::get_cpu_count() as usize, 0.0);
            if let Ok(mut s) = state.lock() {
                s.snapshot = Arc::new(snapshot.clone());
                if let Some(total) = cpu_total {
                    s.cpu_avg = total;
                }
                s.cpu_percents = cpus;
                s.cpu_generation += 1;
                let avg = s.cpu_avg;
                s.cpu_history.push_back(avg);
                if s.cpu_history.len() > 120 {
                    s.cpu_history.pop_front();
                }
                s.hw_monitor = hw.data.clone();
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match cmd_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(DaemonCmd::Shutdown)
                    | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        if let Ok(mut s) = state.lock() {
                            s.shutdown_complete = true;
                        }
                        return;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                    _ => {} // Read-only: deliberately ignore all policy commands.
                }
            }
        }
    })
}

/// Bounded join: wait up to `timeout` for the daemon thread to finish so a
/// restore still in flight (unparking CPUs, restoring nices) gets a real
/// chance to complete before the process image is torn down. Returns true if
/// the thread finished in time. A thread that never finishes is left alone —
/// the process exit will reap it, but at least we logged that it was stuck.
pub fn join_daemon(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = handle.join();
        let _ = tx.send(());
    });
    rx.recv_timeout(timeout).is_ok()
}

fn get_cpu_name() -> String {
    if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            if line.starts_with("model name") {
                if let Some(name) = line.split(':').nth(1) {
                    return name.trim().to_string();
                }
            }
        }
    }
    "Unknown CPU".to_string()
}

fn get_ram_speed_mts() -> Option<u32> {
    use std::sync::OnceLock;
    static RAM_SPEED: OnceLock<Option<u32>> = OnceLock::new();
    *RAM_SPEED.get_or_init(|| {
        // First try the root sensor daemon, which runs dmidecode for us
        if let Some(mts) = crate::sensor_data::read()
            .ok()
            .and_then(|s| s.ram_speed_mts)
        {
            return Some(mts);
        }

        // Fallback to calling dmidecode directly if we happen to have privileges
        let out = std::process::Command::new("dmidecode")
            .arg("-t")
            .arg("memory")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if line.contains("Configured Memory Speed:") && !line.contains("Unknown") {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() == 2 {
                    if let Ok(mts) = parts[1]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .parse::<u32>()
                    {
                        return Some(mts);
                    }
                }
            }
        }
        None
    })
}

fn sensor_access_status(path: &str) -> String {
    match std::fs::File::open(path) {
        Ok(_) => "available".into(),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        Err(_) => "unsupported/unavailable".into(),
    }
}
fn logical_cpus(usages: &[f32]) -> Vec<argus_ipc::LogicalCpu> {
    let mut cpus = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/devices/system/cpu") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(id) = name.strip_prefix("cpu").and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let read = |file: &str| {
                std::fs::read_to_string(entry.path().join(file))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
            };
            let online = read("online").unwrap_or(1) != 0;
            cpus.push(argus_ipc::LogicalCpu {
                id,
                package_id: read("topology/physical_package_id"),
                core_id: read("topology/core_id"),
                online,
                usage: if online {
                    usages.get(id as usize).map(|v| *v as u8)
                } else {
                    None
                },
                frequency_mhz: if online {
                    read("cpufreq/scaling_cur_freq").map(|v| v / 1000)
                } else {
                    None
                },
            });
        }
    }
    cpus.sort_by_key(|c| c.id);
    cpus
}

fn get_ram_info() -> (f32, f32) {
    let mut mem_total = 0.0;
    let mut mem_available = 0.0;
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 {
                    mem_total = parts[1].parse::<f32>().unwrap_or(0.0) / (1024.0 * 1024.0);
                    // GiB
                }
            } else if line.starts_with("MemAvailable:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 {
                    mem_available = parts[1].parse::<f32>().unwrap_or(0.0) / (1024.0 * 1024.0);
                    // GiB
                }
            }
        }
    }
    let mem_used = mem_total - mem_available;
    (mem_used.max(0.0), mem_total)
}

fn run_loop(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<DaemonCmd>,
    initial_config: Config,
    rule_engine: Arc<Mutex<RuleEngine>>,
) {
    let mut config = initial_config;
    let ipc = ipc_server::Broadcaster::start();
    ipc.broadcast(&argus_ipc::IpcMessage::Config(
        config.gaming_mode.overlay.clone(),
    ));

    // Build closures that push log messages into shared state
    let state_log = state.clone();
    let log_cb = move |msg: String| {
        if let Ok(mut s) = state_log.lock() {
            s.append_log(msg);
        }
    };

    let mut probalance = ProBalance::new(config.probalance.clone());
    let mut hw_collector = HwCollector::new();
    let mut last_sensors = Instant::now() - Duration::from_secs(1);
    let mut cpu_percents = Vec::new();
    let mut cpu_sampler = CpuSampler::default();
    let mut avg = 0.0;

    let log_cb2 = log_cb.clone();
    probalance.set_log_callback(log_cb2);

    {
        let log_cb3 = log_cb.clone();
        if let Ok(mut re) = rule_engine.lock() {
            re.set_log_callback(log_cb3);
        }
    }

    // Startup log entry so users can see the log is working
    log_cb(format!(
        "Argus-Lasso started — ProBalance: {}  |  Display refresh: {}ms  |  Rule enforce: {}ms",
        if config.probalance.enabled {
            "on"
        } else {
            "off"
        },
        config.monitor.display_refresh_interval_ms,
        config.monitor.rule_enforce_interval_ms,
    ));

    let mut known_pids: HashSet<u32> = HashSet::new();
    let mut first_snapshot = true;
    // Track previously throttled PIDs for change-based notifications
    let mut prev_throttled: HashSet<u32> = HashSet::new();
    // pid → original affinity set before we changed it; pruned every snapshot cycle
    let mut original_affinities: HashMap<u32, HashSet<u32>> = HashMap::new();
    // pid → expiry Instant (suppress rule re-enforcement after manual change)
    let mut manual_overrides: HashMap<u32, Instant> = HashMap::new();
    // Gaming Mode nice tracking: pid → original nice before we elevated
    let mut gaming_mode = false;
    let mut launch_profiles = Vec::<argus_ipc::LaunchProfile>::new();
    let mut gaming_elevate_nice = false;
    let mut gaming_niced: HashMap<u32, i32> = HashMap::new();
    // Did WE auto-enable Gaming Mode? (never auto-disable a manual activation)
    let mut auto_gaming = false;
    // Consecutive snapshots without a detected game before auto-disabling
    let mut game_absent_snapshots: u32 = 0;
    // Per-PID caches: immutable metadata, display-only fields, I/O counters.
    let mut caches = SnapshotCaches::default();
    let mut last_io_sample = Instant::now();
    // HW alert cooldown: sensor_label → last alert time
    let mut last_alert_times: HashMap<String, Instant> = HashMap::new();

    let mut last_enforce = Instant::now();
    let mut last_pb = Instant::now();
    let mut last_snapshot = Instant::now();
    let mut last_pb_tick = Instant::now();

    // CPU percentage tracking: previous jiffies per process for delta
    let mut prev_cpu_times: HashMap<u32, u64> = HashMap::new();
    let mut prev_sys_total: u64 = 0;
    // Cached snapshot — rebuilt only on enforce/display cadence
    let mut raw_snapshot: Vec<ProcInfo> = Vec::new();
    // (rule_id, pid) pairs whose set_nice failed during enforcement — retried
    // once, not every 500ms tick; pruned when the PID dies.
    let mut enforce_nice_failed: HashSet<(String, u32)> = HashSet::new();

    let toggle_dir = crate::config::config_dir();
    loop {
        // ── Check for CLI overlay toggle ────────────────────────────────────
        if !crate::overlay_toggle::drain(&toggle_dir).is_multiple_of(2) {
            config.gaming_mode.overlay.show_overlay = !config.gaming_mode.overlay.show_overlay;
            if let Err(e) = crate::config::save(&config) {
                log::error!("Failed to save config after toggling overlay: {e}");
            }
            ipc.broadcast(&argus_ipc::IpcMessage::Config(
                config.gaming_mode.overlay.clone(),
            ));
            if let Ok(mut s) = state.lock() {
                s.config = config.clone();
            }
            log_cb(format!(
                "Overlay visibility toggled to {}",
                config.gaming_mode.overlay.show_overlay
            ));
        }

        // ── Drain commands from GUI ─────────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DaemonCmd::GameLaunched { pid, profile } => {
                    if let Some(stat) = crate::fast_proc::read_stat(pid, &mut [0; 1024]) {
                        launch_profiles.retain(|p| p.pid != pid);
                        launch_profiles.push(argus_ipc::LaunchProfile {
                            pid,
                            start_ticks: stat.starttime,
                            profile,
                        });
                    }
                }
                DaemonCmd::UpdateConfig(cfg) => {
                    let cfg = *cfg;
                    probalance.update_config(cfg.probalance.clone());
                    config = cfg.clone();
                    ipc.broadcast(&argus_ipc::IpcMessage::Config(
                        config.gaming_mode.overlay.clone(),
                    ));
                    log_cb(format!(
                        "Config updated — ProBalance: {}  |  Notifications: {}",
                        if config.probalance.enabled {
                            "on"
                        } else {
                            "off"
                        },
                        if config.ui.notifications_enabled {
                            "on"
                        } else {
                            "off"
                        },
                    ));
                    if let Ok(mut s) = state.lock() {
                        s.config = cfg;
                    }
                }
                DaemonCmd::SetGamingMode {
                    active,
                    elevate_nice,
                    park,
                } => {
                    gaming_mode = active;
                    gaming_elevate_nice = elevate_nice;
                    // A manual toggle takes ownership: the auto-detector must
                    // not later auto-disable a manually (re-)enabled mode.
                    auto_gaming = false;
                    game_absent_snapshots = 0;
                    if !active && !gaming_niced.is_empty() {
                        restore_gaming_nices(&mut gaming_niced, &log_cb);
                    }
                    if let Ok(mut s) = state.lock() {
                        s.gaming_active = active;
                    }
                    if park {
                        if active {
                            let topo = cpu_park::detect_topology();
                            if topo.has_asymmetry() && cpu_park::is_helper_installed() {
                                let to_park: HashSet<u32> =
                                    topo.non_preferred.iter().copied().collect();
                                log_cb(format!("[Gaming Mode] Parking CPUs {:?}…", {
                                    let mut v: Vec<_> = to_park.iter().copied().collect();
                                    v.sort_unstable();
                                    v
                                }));
                                if cpu_park::park_cpus(&to_park, &log_cb) {
                                    log_cb(
                                        "[Gaming Mode] ACTIVE — non-preferred CPUs offline.".into(),
                                    );
                                } else {
                                    log_cb("[Gaming Mode] Parking failed — check log.".into());
                                }
                            }
                        } else {
                            log_cb("[Gaming Mode] Unparking all CPUs…".into());
                            cpu_park::unpark_all(&log_cb);
                            log_cb("[Gaming Mode] Disabled — all CPUs online.".into());
                        }
                    }
                }
                DaemonCmd::SetManualOverride { pid, duration_secs } => {
                    manual_overrides
                        .insert(pid, Instant::now() + Duration::from_secs_f64(duration_secs));
                }
                DaemonCmd::ResetAffinities => {
                    reset_all_affinities(&mut original_affinities, &log_cb);
                }
                DaemonCmd::ReapplyDefaults => {
                    // Rules may have changed — failed nice attempts get a fresh
                    // chance (an edited rule can now have an achievable nice).
                    enforce_nice_failed.clear();
                    reapply_defaults(&config, &rule_engine, &known_pids, &log_cb);
                }
                DaemonCmd::Shutdown => {
                    log_cb("[Shutdown] Restoring system state…".into());
                    if !gaming_niced.is_empty() {
                        restore_gaming_nices(&mut gaming_niced, &log_cb);
                    }
                    probalance.shutdown();
                    if !utils::get_offline_cpus().is_empty() {
                        cpu_park::unpark_all(&log_cb);
                    }
                    if let Ok(mut s) = state.lock() {
                        s.shutdown_complete = true;
                    }
                    // Stop the loop entirely: if it kept running, the very
                    // next ProBalance/auto-gaming tick could re-throttle or
                    // re-park in the window before process exit — and a
                    // cgroup re-throttle would outlive us until logout,
                    // since nothing would ever restore it.
                    return;
                }
            }
        }

        let now = Instant::now();
        let enforce_interval = Duration::from_millis(config.monitor.rule_enforce_interval_ms);
        // With no enabled rules and no default affinity there is nothing for
        // an enforce pass to do, so it should not be a reason to walk /proc.
        // On a default install that halves the walks: two a second down to
        // ProBalance's one. Cheap to recheck — the engine holds a Vec.
        let enforcing = config.cpu.default_affinity.is_some()
            || rule_engine
                .lock()
                .map(|re| re.get_rules().iter().any(|r| r.enabled))
                .unwrap_or(false);
        let needs_snapshot = (enforcing && now.duration_since(last_enforce) >= enforce_interval)
            || now.duration_since(last_snapshot)
                >= Duration::from_millis(config.monitor.display_refresh_interval_ms)
            || now.duration_since(last_pb) >= Duration::from_secs(1);

        // ── Collect process snapshot (only when needed) ─────────────────────
        if needs_snapshot {
            // Affinity, I/O priority and disk rates are rendered by the
            // process table and nothing else, so only pay for them on the
            // pass that will actually be published.
            let detail = now.duration_since(last_snapshot)
                >= Duration::from_millis(config.monitor.display_refresh_interval_ms);
            let io_elapsed = if detail {
                let e = now.duration_since(last_io_sample).as_secs_f32();
                last_io_sample = now;
                e
            } else {
                0.0
            };
            let (new_cpu_times, sys_total) = collect_snapshot(
                &mut raw_snapshot,
                &mut prev_cpu_times,
                prev_sys_total,
                &mut caches,
                detail,
                io_elapsed,
            );
            prev_cpu_times = new_cpu_times;
            prev_sys_total = sys_total;

            let current_pids: HashSet<u32> = raw_snapshot.iter().map(|p| p.pid).collect();

            // Prune dead PIDs from per-PID maps: avoids unbounded growth, and —
            // for gaming_niced — stops a reused PID from getting an unrelated
            // process's nice restored onto it when Gaming Mode is disabled.
            original_affinities.retain(|pid, _| current_pids.contains(pid));
            gaming_niced.retain(|pid, _| current_pids.contains(pid));
            caches.retain_live(&current_pids);
            enforce_nice_failed.retain(|(_, pid)| current_pids.contains(pid));

            // ── New PIDs: apply rules or default affinity ───────────────────
            let new_pids: HashSet<u32> = current_pids.difference(&known_pids).copied().collect();
            if !new_pids.is_empty() {
                for proc in raw_snapshot.iter().filter(|p| new_pids.contains(&p.pid)) {
                    apply_new_pid(
                        proc,
                        &config,
                        &rule_engine,
                        &mut original_affinities,
                        gaming_mode,
                        gaming_elevate_nice,
                        &mut gaming_niced,
                        &log_cb,
                    );
                }
            }
            if first_snapshot {
                log_cb(format!(
                    "Initial scan: {} processes found.",
                    raw_snapshot.len()
                ));
                first_snapshot = false;
            }
            known_pids = current_pids;

            // ── Auto Gaming Mode (Steam/Proton detection) ───────────────────
            if config.gaming_mode.auto_detect {
                let game = raw_snapshot.iter().find(|p| is_game_process(p));
                if let Some(game) = game {
                    game_absent_snapshots = 0;
                    if !gaming_mode {
                        gaming_mode = true;
                        gaming_elevate_nice = true;
                        auto_gaming = true;
                        log_cb(format!(
                            "[Gaming Mode] Auto-enabled — game detected: {} ({})",
                            game.name, game.pid
                        ));
                        if let Ok(mut s) = state.lock() {
                            s.gaming_active = true;
                        }
                        if config.gaming_mode.auto_park {
                            park_non_preferred(&log_cb);
                        }
                    }
                } else if auto_gaming && gaming_mode {
                    // Require a couple of game-free snapshots before restoring,
                    // so a brief exec/restart doesn't bounce the CPUs.
                    game_absent_snapshots += 1;
                    if game_absent_snapshots >= 2 {
                        gaming_mode = false;
                        auto_gaming = false;
                        game_absent_snapshots = 0;
                        log_cb("[Gaming Mode] Auto-disabled — game exited.".into());
                        if !gaming_niced.is_empty() {
                            restore_gaming_nices(&mut gaming_niced, &log_cb);
                        }
                        if let Ok(mut s) = state.lock() {
                            s.gaming_active = false;
                        }
                        if config.gaming_mode.auto_park {
                            cpu_park::unpark_all(&log_cb);
                        }
                    }
                }
            }
        }

        // ── Rule enforcement every enforce_interval ─────────────────────────
        if enforcing && now.duration_since(last_enforce) >= enforce_interval {
            // Expire stale manual overrides
            manual_overrides.retain(|_, exp| *exp > now);
            // Clone the rules and enforce WITHOUT holding the engine lock:
            // enforcement does procfs reads and renice/ionice subprocess spawns
            // per process, and the GUI thread locks the same engine to edit
            // rules — holding it here would freeze the UI for the whole pass.
            let rules: Vec<crate::rules::Rule> = rule_engine
                .lock()
                .map(|re| re.get_rules().to_vec())
                .unwrap_or_default();
            if !rules.is_empty() {
                for proc in &raw_snapshot {
                    if manual_overrides.contains_key(&proc.pid) {
                        continue;
                    }
                    crate::rules::apply_rules(
                        &rules,
                        proc.pid,
                        &proc.name,
                        Some(proc.nice),
                        crate::utils::get_ionice_raw(proc.pid), // Could be cached, but only queried if rule matches
                        &mut enforce_nice_failed,
                        &log_cb,
                    );
                }
            }
            last_enforce = now;
        }

        // ── ProBalance every 1s ────────────────────────────────────────────
        if now.duration_since(last_pb) >= Duration::from_secs(1) {
            let pb_tick = now.duration_since(last_pb_tick).as_secs_f32();
            last_pb_tick = now;
            let pb_snap: Vec<ProcSnapshot> = raw_snapshot
                .iter()
                .map(|p| ProcSnapshot {
                    pid: p.pid,
                    name: p.name.to_string(),
                    cpu_percent: p.cpu_percent,
                    nice: p.nice,
                })
                .collect();
            let (readings, system_cpu) = cpu_sampler.sample(read_percpu_stats());
            cpu_percents = readings;
            cpu_percents.resize(utils::get_cpu_count() as usize, 0.0);
            if let Some(total) = system_cpu {
                avg = total;
            }
            let protected = protected_processes(&raw_snapshot, &launch_profiles, &manual_overrides);
            probalance.tick(&pb_snap, pb_tick, system_cpu, &protected);

            // Fire desktop notifications for newly throttled / restored PIDs
            let cur_throttled = probalance.throttled_pids();
            if cur_throttled != prev_throttled && config.ui.notifications_enabled {
                // Build a name lookup from the current snapshot
                let name_map: HashMap<u32, &str> = raw_snapshot
                    .iter()
                    .map(|p| (p.pid, p.name.as_ref()))
                    .collect();

                // Newly throttled
                for &pid in cur_throttled.difference(&prev_throttled) {
                    let name = name_map.get(&pid).copied().unwrap_or("unknown");
                    let _ = notify_rust::Notification::new()
                        .summary("ProBalance")
                        .body(&format!("Throttled: {name} (PID {pid})"))
                        .timeout(notify_rust::Timeout::Milliseconds(3000))
                        .show();
                }
                // Restored
                for &pid in prev_throttled.difference(&cur_throttled) {
                    let name = name_map.get(&pid).copied().unwrap_or("unknown");
                    let _ = notify_rust::Notification::new()
                        .summary("ProBalance")
                        .body(&format!("Restored: {name} (PID {pid})"))
                        .timeout(notify_rust::Timeout::Milliseconds(3000))
                        .show();
                }
            }
            prev_throttled = cur_throttled;

            last_pb = now;
        }

        if now.duration_since(last_sensors) >= Duration::from_secs(1) {
            // Update hardware sensor readings
            hw_collector.update();

            // Per-process GPU utilization (empty map without NVIDIA/NVML)
            let gpu_util = crate::hw_monitor::collect_gpu_process_util();
            if !gpu_util.is_empty() {
                for p in &mut raw_snapshot {
                    p.gpu_percent = gpu_util.get(&p.pid).copied().unwrap_or(0.0);
                }
            }

            // Check temperature alerts
            check_hw_alerts(
                &hw_collector.data,
                &config.hw_alerts,
                config.ui.notifications_enabled,
                &mut last_alert_times,
                &log_cb,
            );

            // Broadcast overlay telemetry
            let parked_cores = utils::get_offline_cpus().len() as u32;
            let active_profile = if gaming_mode {
                "Gaming".to_string()
            } else {
                "Normal".to_string()
            };
            let gpu_usage = hw_collector.data.get_gpu_usage();
            let gpu_temp = hw_collector.data.get_gpu_temp();
            let cpu_temp = hw_collector.data.get_cpu_temp();
            let gpu_power = hw_collector.data.get_gpu_power();
            let extended = crate::sensor_data::read().ok();
            let cpu_power = extended
                .as_ref()
                .and_then(|s| s.cpu_power_w)
                .or_else(|| hw_collector.data.get_cpu_power());
            let gpu_name = hw_collector.data.get_gpu_name();
            let cpu_name = get_cpu_name();
            let (ram_used, ram_total) = get_ram_info();
            let vram_used = hw_collector.data.get_vram_usage_gb();
            let vram_total = hw_collector.data.get_vram_total_gb();

            ipc.broadcast(&argus_ipc::IpcMessage::Telemetry(
                argus_ipc::TelemetryFrame {
                    cpu_name,
                    cpu_usage_percent: avg as u8,
                    cpu_temp_c: cpu_temp,
                    cpu_power_w: cpu_power,
                    gpu_name,
                    gpu_usage_percent: gpu_usage,
                    gpu_temp_c: gpu_temp,
                    gpu_power_w: gpu_power,
                    gpu_core_clock_mhz: hw_collector.data.get_gpu_core_clock(),
                    gpu_mem_clock_mhz: hw_collector.data.get_gpu_mem_clock(),
                    gpu_fan_speed_percent: hw_collector.data.get_gpu_fan_speed(),
                    cpu_freq_mhz: hw_collector.data.get_cpu_freq(),
                    ram_speed_mts: extended
                        .as_ref()
                        .and_then(|s| s.ram_speed_mts)
                        .or_else(get_ram_speed_mts),
                    ram_used_gb: ram_used,
                    ram_total_gb: ram_total,
                    vram_used_gb: vram_used,
                    vram_total_gb: vram_total,
                    active_profile,
                    game: None,
                    launch_profiles: {
                        launch_profiles.retain(|p| {
                            crate::fast_proc::read_stat(p.pid, &mut [0; 1024])
                                .is_some_and(|s| s.starttime == p.start_ticks)
                        });
                        launch_profiles.clone()
                    },
                    probalance_pids: probalance.throttled_pids().into_iter().collect(),
                    parked_cores,
                    cpus: logical_cpus(&cpu_percents),
                    sample_unix_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    sample_interval_ms: 1000,
                    cpu_power_status: extended
                        .as_ref()
                        .map(|s| s.cpu_power_status.clone())
                        .unwrap_or_else(|| {
                            sensor_access_status("/sys/class/powercap/intel-rapl:0/energy_uj")
                        }),
                    ram_speed_status: extended
                        .as_ref()
                        .map(|s| s.ram_speed_status.clone())
                        .unwrap_or_else(|| sensor_access_status("/sys/firmware/dmi/tables/DMI")),
                },
            ));

            last_sensors = now;
        }

        // ── Snapshot emit every display_refresh_interval ───────────────────
        let refresh = Duration::from_millis(config.monitor.display_refresh_interval_ms);
        if now.duration_since(last_snapshot) >= refresh {
            let throttled = probalance.throttled_pids();
            let pb_snap_for_infos: Vec<crate::probalance::ProcSnapshot> = raw_snapshot
                .iter()
                .map(|p| crate::probalance::ProcSnapshot {
                    pid: p.pid,
                    name: p.name.to_string(),
                    cpu_percent: p.cpu_percent,
                    nice: p.nice,
                })
                .collect();
            let throttle_infos = probalance.throttle_infos(&pb_snap_for_infos);

            if let Ok(mut s) = state.lock() {
                s.snapshot = std::sync::Arc::new(raw_snapshot.clone());
                s.cpu_percents = cpu_percents.clone();
                s.cpu_generation = s.cpu_generation.wrapping_add(1);
                s.throttled_pids = throttled;
                s.throttle_infos = throttle_infos;
                s.cpu_avg = avg;
                s.cpu_history.push_back(avg);
                while s.cpu_history.len() > 120 {
                    s.cpu_history.pop_front();
                }
                // Aggregate disk/net totals for the Overview graphs
                let (disk, net) = hw_io_totals(&hw_collector.data);
                s.disk_io_history.push_back(disk);
                while s.disk_io_history.len() > 120 {
                    s.disk_io_history.pop_front();
                }
                s.net_io_history.push_back(net);
                while s.net_io_history.len() > 120 {
                    s.net_io_history.pop_front();
                }
                s.hw_monitor = hw_collector.data.clone();
                // Update per-PID CPU history
                let current_pids: std::collections::HashSet<u32> =
                    raw_snapshot.iter().map(|p| p.pid).collect();
                for p in &raw_snapshot {
                    let hist = s
                        .proc_cpu_history
                        .entry(p.pid)
                        .or_insert_with(|| std::collections::VecDeque::with_capacity(30));
                    hist.push_back(p.cpu_percent);
                    while hist.len() > 30 {
                        hist.pop_front();
                    }
                }
                s.proc_cpu_history
                    .retain(|pid, _| current_pids.contains(pid));
            }
            last_snapshot = now;
        }

        // Sleep no longer than the enforcement interval so a sub-500ms
        // rule_enforce_interval_ms is honoured instead of silently ignored.
        let tick = enforce_interval.clamp(Duration::from_millis(50), Duration::from_millis(500));
        std::thread::sleep(tick);
    }
}

/// One-shot two-sample process snapshot for CLI use (`status --json`).
/// Samples 500ms apart so CPU% deltas are meaningful.
pub fn oneshot_snapshot() -> Vec<ProcInfo> {
    let mut prev_times: HashMap<u32, u64> = HashMap::new();
    let mut caches = SnapshotCaches::default();
    // The CLI prints affinity, so both passes ask for detail.
    let mut snap = Vec::new();
    let (times, sys_total) =
        collect_snapshot(&mut snap, &mut prev_times, 0, &mut caches, true, 0.0);
    prev_times = times;
    std::thread::sleep(Duration::from_millis(500));
    let (_, _) = collect_snapshot(
        &mut snap,
        &mut prev_times,
        sys_total,
        &mut caches,
        true,
        0.5,
    );
    snap
}

/// Sum current disk (read, write) and network (rx, tx) MiB/s across all
/// devices from the hw-monitor readings, for the Overview graphs.
fn hw_io_totals(data: &HwMonitorData) -> ((f32, f32), (f32, f32)) {
    let mut disk = (0.0f32, 0.0f32);
    let mut net = (0.0f32, 0.0f32);
    for group in &data.groups {
        if !group.name.starts_with("I/O [") {
            continue;
        }
        for s in &group.sensors {
            match (group.category, s.label) {
                ("Storage", "Read") => disk.0 += s.value,
                ("Storage", "Write") => disk.1 += s.value,
                ("Network", "Receive") => net.0 += s.value,
                ("Network", "Transmit") => net.1 += s.value,
                _ => {}
            }
        }
    }
    (disk, net)
}

/// Heuristic: does this process look like a running game?
/// Matches binaries living under a Steam library ("steamapps/common") and
/// Proton wrapper invocations — the launchers/wrappers matched alongside the
/// game exit together with it, so they don't hold auto-mode on.
fn is_game_process(p: &ProcInfo) -> bool {
    let cmd = p.cmdline.replace('\\', "/").to_ascii_lowercase();
    cmd.contains("steamapps/common") || cmd.contains("/proton ")
}

/// Wayland has no portable foreground-process API. Protect known games/launch
/// trees and explicit high-priority/manual targets; user exemptions cover others.
fn protected_processes(
    snapshot: &[ProcInfo],
    launches: &[argus_ipc::LaunchProfile],
    manual: &HashMap<u32, Instant>,
) -> HashSet<u32> {
    let mut protected: HashSet<u32> = snapshot
        .iter()
        .filter(|p| {
            p.pid <= 1
                || p.pid == std::process::id()
                || p.nice < 0
                || is_game_process(p)
                || manual.contains_key(&p.pid)
        })
        .map(|p| p.pid)
        .collect();
    for launch in launches {
        if crate::fast_proc::read_stat(launch.pid, &mut [0; 1024])
            .is_some_and(|s| s.starttime == launch.start_ticks)
        {
            protected.insert(launch.pid);
        }
    }
    loop {
        let before = protected.len();
        for p in snapshot {
            if protected.contains(&p.ppid) && p.ppid > 1 {
                protected.insert(p.pid);
            }
        }
        if protected.len() == before {
            break;
        }
    }
    protected
}

/// Park the non-preferred CPUs (used by both manual SetGamingMode and
/// auto-detection). No-op without an asymmetric topology or the helper.
fn park_non_preferred(log_cb: &impl Fn(String)) {
    let topo = cpu_park::detect_topology();
    if topo.has_asymmetry() && cpu_park::is_helper_installed() {
        let to_park: HashSet<u32> = topo.non_preferred.iter().copied().collect();
        if cpu_park::park_cpus(&to_park, log_cb) {
            log_cb("[Gaming Mode] Non-preferred CPUs parked.".into());
        } else {
            log_cb("[Gaming Mode] Parking failed — check log.".into());
        }
    }
}

// ── Process collection ────────────────────────────────────────────────────────

/// Per-PID facts that never change while the process lives.
///
/// `cmdline` was re-read, re-joined and re-allocated for every process on
/// every pass — several hundred file reads a second for strings that cannot
/// have changed. `start_time` guards against PID reuse handing a recycled PID
/// the previous occupant's name.
struct ProcMeta {
    start_time: u64,
    name: std::sync::Arc<str>,
    cmdline: std::sync::Arc<String>,
}

/// Caches that let a collection pass skip work the callers do not need.
#[derive(Default)]
pub struct SnapshotCaches {
    meta: HashMap<u32, ProcMeta>,
    /// pid → (affinity, ionice) — display-only, so refreshed on the display
    /// cadence rather than the much shorter enforce cadence.
    display: HashMap<u32, (std::sync::Arc<str>, std::sync::Arc<str>)>,
    /// pid → (read_bytes, write_bytes) at the last I/O sample.
    io: HashMap<u32, (u64, u64)>,
}

impl SnapshotCaches {
    /// Drop entries for PIDs that are gone. Called with the live PID set the
    /// loop already computes, so this costs nothing extra.
    fn retain_live(&mut self, live: &HashSet<u32>) {
        self.meta.retain(|pid, _| live.contains(pid));
        self.display.retain(|pid, _| live.contains(pid));
        self.io.retain(|pid, _| live.contains(pid));
    }
}

/// Collect one pass over `/proc`.
///
/// `detail` asks for the display-only fields — affinity, I/O priority and
/// disk rates. They cost a syscall and two allocations per process each, and
/// only the process table renders them, so the enforce and ProBalance ticks
/// pass `false` and reuse whatever the last display pass saw.
///
/// `io_elapsed` is the wall time since the last I/O sample; the byte deltas
/// are divided by it so the rate is per second regardless of cadence.
fn collect_snapshot(
    snapshot: &mut Vec<ProcInfo>,
    prev_times: &mut HashMap<u32, u64>,
    prev_sys_total: u64,
    caches: &mut SnapshotCaches,
    detail: bool,
    io_elapsed: f32,
) -> (HashMap<u32, u64>, u64) {
    let mut new_times: HashMap<u32, u64> = HashMap::new();
    snapshot.clear();

    let sys_total = read_sys_cpu_total();
    let sys_delta = sys_total.saturating_sub(prev_sys_total) as f32;

    let mut stat_buf = [0u8; 1024];
    let mut cmd_buf = Vec::with_capacity(1024);

    for pid in crate::fast_proc::all_pids() {
        let stat = match crate::fast_proc::read_stat(pid, &mut stat_buf) {
            Some(s) => s,
            None => continue,
        };

        let ppid = stat.ppid;

        let same_process = caches
            .meta
            .get(&pid)
            .is_some_and(|m| m.start_time == stat.starttime);
        let meta = match caches.meta.get(&pid) {
            Some(m) if m.start_time == stat.starttime => m,
            _ => {
                let cmdline = crate::fast_proc::read_cmdline(pid, &mut cmd_buf);
                let entry = ProcMeta {
                    start_time: stat.starttime,
                    name: utils::resolve_name(&stat.comm, &cmdline).into(),
                    cmdline: std::sync::Arc::new(cmdline.join(" ")),
                };
                caches.meta.entry(pid).insert_entry(entry).into_mut()
            }
        };
        let name = meta.name.clone();
        let cmdline = std::sync::Arc::clone(&meta.cmdline);

        let proc_ticks = stat.utime + stat.stime;
        new_times.insert(pid, proc_ticks);
        let prev_ticks = if same_process {
            prev_times.get(&pid).copied().unwrap_or(proc_ticks)
        } else {
            proc_ticks
        };
        let delta_ticks = proc_ticks.saturating_sub(prev_ticks) as f32;
        let cpu_percent = cpu_share(delta_ticks, sys_delta);

        let mem_rss = stat.rss_bytes;
        let nice = stat.nice;

        let (affinity, ionice, disk_read_bps, disk_write_bps) = if detail {
            let affinity: std::sync::Arc<str> = utils::get_affinity_str(pid).into();
            let ionice: std::sync::Arc<str> = read_ionice(pid).into();
            let (r, w) = read_proc_io(pid, &mut caches.io, io_elapsed);
            caches
                .display
                .insert(pid, (affinity.clone(), ionice.clone()));
            (affinity, ionice, r, w)
        } else {
            match caches.display.get(&pid) {
                Some((a, i)) => (a.clone(), i.clone(), 0, 0),
                None => ("".into(), "".into(), 0, 0),
            }
        };

        snapshot.push(ProcInfo {
            pid,
            ppid,
            name,
            cpu_percent,
            gpu_percent: 0.0,
            mem_rss,
            nice,
            affinity,
            ionice,
            disk_read_bps,
            disk_write_bps,
            cmdline,
        });
    }

    (new_times, sys_total)
}
fn read_proc_io(pid: u32, io_cache: &mut HashMap<u32, (u64, u64)>, elapsed: f32) -> (u64, u64) {
    let mut buf = [0u8; 512];
    let text = if let Ok(mut f) = std::fs::File::open(format!("/proc/{pid}/io")) {
        use std::io::Read;
        if let Ok(n) = f.read(&mut buf) {
            std::str::from_utf8(&buf[..n]).unwrap_or("")
        } else {
            ""
        }
    } else {
        ""
    };
    if text.is_empty() {
        return (0, 0);
    }
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("read_bytes: ") {
            read_bytes = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("write_bytes: ") {
            write_bytes = v.trim().parse().unwrap_or(0);
        }
    }
    let (prev_r, prev_w) = io_cache
        .get(&pid)
        .copied()
        .unwrap_or((read_bytes, write_bytes));
    io_cache.insert(pid, (read_bytes, write_bytes));
    let per_sec = |delta: u64| {
        if elapsed > 0.01 {
            (delta as f32 / elapsed) as u64
        } else {
            0
        }
    };
    (
        per_sec(read_bytes.saturating_sub(prev_r)),
        per_sec(write_bytes.saturating_sub(prev_w)),
    )
}

fn read_sys_cpu_total() -> u64 {
    // Read first line of /proc/stat: cpu  user nice system idle iowait irq softirq ...
    if let Ok(text) = std::fs::read_to_string("/proc/stat") {
        if let Some(line) = text.lines().next() {
            return line
                .split_whitespace()
                .skip(1)
                // guest/guest_nice are already included in user/nice.
                .take(8)
                .filter_map(|s| s.parse::<u64>().ok())
                .sum();
        }
    }
    0
}

/// CPU-time share, not a frequency/performance estimate or per-core multiple.
fn cpu_share(busy: f32, total: f32) -> f32 {
    if total.is_finite() && total > 0.0 && busy.is_finite() {
        (busy / total * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

#[derive(Default)]
struct CpuSampler {
    previous: HashMap<u32, [u64; 10]>,
}

impl CpuSampler {
    /// Returns individual logical CPU loads plus a weighted system total.
    /// A new/removed CPU, counter reset or unreadable sample breaks the baseline;
    /// ProBalance must not interpret it as proof of sustained system pressure.
    fn sample(&mut self, stats: Vec<(u32, [u64; 10])>) -> (Vec<f32>, Option<f32>) {
        let count = stats
            .iter()
            .map(|(id, _)| *id as usize + 1)
            .max()
            .unwrap_or(0);
        let mut per_cpu = vec![0.0; count];
        let stable = !stats.is_empty()
            && stats.len() == self.previous.len()
            && stats.iter().all(|(id, _)| self.previous.contains_key(id));
        let mut valid = stable;
        let mut total = 0u64;
        let mut busy = 0u64;
        for (id, current) in &stats {
            let Some(previous) = self.previous.get(id) else {
                continue;
            };
            // Exclude guest fields, which are subsets of user/nice.
            let old_total: u64 = previous[..8].iter().sum();
            let new_total: u64 = current[..8].iter().sum();
            let old_idle = previous[3] + previous[4];
            let new_idle = current[3] + current[4];
            let Some(dt) = new_total.checked_sub(old_total).filter(|v| *v > 0) else {
                valid = false;
                continue;
            };
            let Some(di) = new_idle.checked_sub(old_idle).filter(|v| *v <= dt) else {
                valid = false;
                continue;
            };
            per_cpu[*id as usize] = cpu_share((dt - di) as f32, dt as f32);
            total += dt;
            busy += dt - di;
        }
        self.previous = stats.into_iter().collect();
        (
            per_cpu,
            (valid && total > 0).then(|| cpu_share(busy as f32, total as f32)),
        )
    }
}

fn read_percpu_stats() -> Vec<(u32, [u64; 10])> {
    let mut result = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/proc/stat") {
        for line in text.lines() {
            if line.starts_with("cpu") && line.len() > 3 && line.as_bytes()[3].is_ascii_digit() {
                let mut toks = line.split_whitespace();
                let label = toks.next().unwrap_or("");
                let Ok(cpu_num) = label[3..].parse::<u32>() else {
                    continue;
                };
                let mut fields = [0u64; 10];
                for (i, tok) in toks.enumerate() {
                    if i < 10 {
                        fields[i] = tok.parse().unwrap_or(0);
                    }
                }
                result.push((cpu_num, fields));
            }
        }
    }
    result
}

fn read_ionice(pid: u32) -> String {
    match utils::get_ionice_raw(pid) {
        Some((class, level)) => format!("{class}/{level}"),
        None => String::new(),
    }
}

// ── New PID handling ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn apply_new_pid(
    proc: &ProcInfo,
    config: &Config,
    rule_engine: &Arc<Mutex<RuleEngine>>,
    original_affinities: &mut HashMap<u32, HashSet<u32>>,
    gaming_mode: bool,
    gaming_elevate_nice: bool,
    gaming_niced: &mut HashMap<u32, i32>,
    log_cb: &impl Fn(String),
) {
    let pid = proc.pid;
    capture_original(pid, original_affinities);

    // "Matched" must come from the rule patterns, NOT from whether applying
    // produced actions: a matching rule whose settings are already correct
    // returns no actions, and treating that as "unmatched" would clobber the
    // rule's affinity with the default affinity below.
    let matched = if let Ok(mut re) = rule_engine.lock() {
        let m = re.matches_any(&proc.name);
        re.apply_to_process(pid, &proc.name);
        m
    } else {
        false
    };

    if matched {
        // Rule matched — if gaming mode + elevate_nice, apply nice -1 and pin to preferred cores
        if gaming_mode && gaming_elevate_nice && !gaming_niced.contains_key(&pid) {
            let orig_nice = proc.nice;
            if cpu_park::set_process_nice_via_helper(pid, -1) {
                gaming_niced.insert(pid, orig_nice);
                log_cb(format!("[Gaming Mode] nice -1 → {}({})", proc.name, pid));
            }
            // Pin game process to preferred cores (P-cores / V-Cache CCD)
            let topo = cpu_park::detect_topology();
            if topo.has_asymmetry() {
                let preferred_list = utils::cpuset_to_cpulist(&topo.preferred);
                if utils::set_affinity(pid, &preferred_list) {
                    log_cb(format!(
                        "[Gaming Mode] affinity → {} ({}) for {}({})",
                        topo.preferred_label, preferred_list, proc.name, pid
                    ));
                }
            }
        }
    } else {
        // No rule matched — apply default affinity if configured
        if let Some(ref default_aff) = config.cpu.default_affinity {
            if !default_aff.is_empty() && utils::set_affinity(pid, default_aff) {
                log_cb(format!(
                    "[Default] affinity={default_aff} → {}({pid})",
                    proc.name
                ));
            }
        }
    }
}

fn capture_original(pid: u32, original_affinities: &mut HashMap<u32, HashSet<u32>>) {
    if original_affinities.contains_key(&pid) {
        return;
    }
    use nix::sched::{sched_getaffinity, CpuSet};
    use nix::unistd::Pid;
    if let Ok(cpu_set) = sched_getaffinity(Pid::from_raw(pid as i32)) {
        let mut cpus = HashSet::new();
        for i in 0..CpuSet::count() {
            if cpu_set.is_set(i).unwrap_or(false) {
                cpus.insert(i as u32);
            }
        }
        original_affinities.insert(pid, cpus);
    }
}

// ── Reset all affinities ──────────────────────────────────────────────────────

fn reset_all_affinities(
    original_affinities: &mut HashMap<u32, HashSet<u32>>,
    log_cb: &impl Fn(String),
) {
    use nix::sched::{sched_setaffinity, CpuSet};
    use nix::unistd::Pid;

    let online = utils::get_cpu_count();
    let all_cpus: HashSet<u32> = (0..online).collect();
    let mut count = 0;

    for (pid, orig) in original_affinities.iter() {
        let mask = if orig.is_empty() { &all_cpus } else { orig };
        let mut cpu_set = CpuSet::new();
        for &c in mask {
            let _ = cpu_set.set(c as usize);
        }
        if sched_setaffinity(Pid::from_raw(*pid as i32), &cpu_set).is_ok() {
            count += 1;
        }
        // Also reset all threads
        let tids = utils::get_tids(*pid);
        for tid in tids {
            if tid != *pid {
                let _ = sched_setaffinity(Pid::from_raw(tid as i32), &cpu_set);
            }
        }
    }
    original_affinities.clear();
    log_cb(format!(
        "[Reset] Restored affinity on {count} processes to original state."
    ));
}

// ── Reapply defaults ──────────────────────────────────────────────────────────

fn reapply_defaults(
    config: &Config,
    rule_engine: &Arc<Mutex<RuleEngine>>,
    known_pids: &HashSet<u32>,
    log_cb: &impl Fn(String),
) {
    let default_aff = match &config.cpu.default_affinity {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return,
    };

    for &pid in known_pids {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let comm = comm.trim();
        let cmdline_raw: Vec<String> = std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .unwrap_or_default()
            .split('\0')
            .map(|s| s.to_string())
            .collect();
        let name = utils::resolve_name(comm, &cmdline_raw);

        let matched = if let Ok(mut re) = rule_engine.lock() {
            let m = re.matches_any(&name);
            re.apply_to_process(pid, &name);
            m
        } else {
            false
        };
        if !matched && utils::set_affinity(pid, &default_aff) {
            log_cb(format!("[Default] affinity={default_aff} → {name}({pid})"));
        }
    }
}

// ── HW temperature alerts ────────────────────────────────────────────────────

fn check_hw_alerts(
    data: &HwMonitorData,
    cfg: &crate::config::HwAlertConfig,
    notifications_enabled: bool,
    last_alert: &mut HashMap<String, Instant>,
    log_cb: &impl Fn(String),
) {
    if !cfg.enabled {
        return;
    }
    let threshold = cfg.temp_threshold_celsius;
    let cooldown = Duration::from_secs(cfg.cooldown_secs);
    let now = Instant::now();

    for group in &data.groups {
        for sensor in &group.sensors {
            if sensor.unit != "°C" {
                continue;
            }
            if sensor.value >= threshold {
                let key = format!("{}/{}", group.name, sensor.label);
                // No subtraction from Instant::now() — that can underflow and
                // panic early after boot. Absent entry = alert is due.
                let due = last_alert
                    .get(&key)
                    .is_none_or(|t| now.duration_since(*t) >= cooldown);
                if due {
                    last_alert.insert(key.clone(), now);
                    let msg = format!(
                        "[HW Alert] {} — {} {:.0}{}  (threshold: {:.0}°C)",
                        group.name, sensor.label, sensor.value, sensor.unit, threshold
                    );
                    log_cb(msg.clone());
                    if notifications_enabled {
                        let _ = notify_rust::Notification::new()
                            .summary("Argus-Lasso — Temperature Alert")
                            .body(&format!(
                                "{}: {:.0}°C (limit: {:.0}°C)",
                                key, sensor.value, threshold
                            ))
                            .timeout(notify_rust::Timeout::Milliseconds(5000))
                            .show();
                    }
                }
            }
        }
    }
}

// ── Restore gaming nices ──────────────────────────────────────────────────────

fn restore_gaming_nices(gaming_niced: &mut HashMap<u32, i32>, log_cb: &impl Fn(String)) {
    let mut count = 0;
    for (&pid, &orig_nice) in gaming_niced.iter() {
        if cpu_park::set_process_nice_via_helper(pid, orig_nice) {
            count += 1;
        }
    }
    gaming_niced.clear();
    log_cb(format!(
        "[Gaming Mode] Restored nice for {count} processes."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with_cmdline(cmd: &str) -> ProcInfo {
        ProcInfo {
            cmdline: std::sync::Arc::new(cmd.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn game_detection_matches_steam_and_proton() {
        assert!(is_game_process(&proc_with_cmdline(
            "/home/u/.local/share/Steam/steamapps/common/Hades/Hades.exe"
        )));
        assert!(is_game_process(&proc_with_cmdline(
            "/usr/bin/python3 /path/proton waitforexitandrun game.exe"
        )));
    }

    #[test]
    fn game_detection_ignores_normal_processes() {
        assert!(!is_game_process(&proc_with_cmdline("/usr/bin/firefox")));
        assert!(!is_game_process(&proc_with_cmdline(
            "/usr/lib/systemd/systemd --user"
        )));
        // Steam client itself lives outside steamapps/common
        assert!(!is_game_process(&proc_with_cmdline(
            "/home/u/.local/share/Steam/ubuntu12_32/steam"
        )));
    }

    #[test]
    fn hw_io_totals_sums_matching_groups_only() {
        use crate::hw_monitor::{Sensor, SensorGroup};

        fn sensor(label: &'static str, v: f32) -> Sensor {
            let mut s = Sensor::new(label, "MiB/s");
            s.push(v);
            s
        }

        let data = HwMonitorData {
            groups: vec![
                SensorGroup {
                    category: "Storage",
                    name: "I/O [nvme0n1]".into(),
                    sensors: vec![sensor("Read", 1.5), sensor("Write", 0.5)],
                },
                SensorGroup {
                    category: "Storage",
                    name: "I/O [sda]".into(),
                    sensors: vec![sensor("Read", 0.5), sensor("Write", 1.0)],
                },
                SensorGroup {
                    category: "Network",
                    name: "I/O [eth0]".into(),
                    sensors: vec![sensor("Receive", 2.0), sensor("Transmit", 0.25)],
                },
                // Non-I/O group must be ignored even with matching labels
                SensorGroup {
                    category: "Storage",
                    name: "nvme".into(),
                    sensors: vec![sensor("Read", 99.0)],
                },
            ],
        };
        let ((dr, dw), (rx, tx)) = hw_io_totals(&data);
        assert_eq!((dr, dw), (2.0, 1.5));
        assert_eq!((rx, tx), (2.0, 0.25));
    }
}

#[cfg(test)]
mod cpu_accounting_tests {
    use super::*;

    fn counters(busy: u64, idle: u64) -> [u64; 10] {
        [busy, 0, 0, idle, 0, 0, 0, 0, 0, 0]
    }

    #[test]
    fn one_busy_cpu_is_one_thirty_second_of_total_capacity() {
        let mut sampler = CpuSampler::default();
        assert_eq!(
            sampler
                .sample((0..32).map(|id| (id, counters(0, 0))).collect())
                .1,
            None
        );
        let (individual, total) = sampler.sample(
            (0..32)
                .map(|id| {
                    (
                        id,
                        if id == 0 {
                            counters(100, 0)
                        } else {
                            counters(0, 100)
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(individual[0], 100.0);
        assert_eq!(total, Some(3.125));
        assert_eq!(cpu_share(100.0, 3200.0), 3.125);
        assert_eq!(cpu_share(1600.0, 3200.0), 50.0);
        assert_eq!(cpu_share(3200.0, 3200.0), 100.0);
    }

    #[test]
    fn offline_cpus_do_not_dilute_load_and_hotplug_resets_baseline() {
        let mut sampler = CpuSampler::default();
        sampler.sample(vec![(0, counters(0, 0)), (7, counters(0, 0))]);
        assert_eq!(sampler.sample(vec![(0, counters(100, 0))]).1, None);
        assert_eq!(sampler.sample(vec![(0, counters(200, 0))]).1, Some(100.0));
        assert_eq!(
            sampler
                .sample(vec![(0, counters(300, 0)), (7, counters(50, 50))])
                .1,
            None
        );
        assert_eq!(
            sampler
                .sample(vec![(0, counters(400, 0)), (7, counters(50, 150))])
                .1,
            Some(50.0)
        );
    }

    #[test]
    fn system_percentage_is_weighted_and_guest_time_is_not_counted_twice() {
        let mut sampler = CpuSampler::default();
        sampler.sample(vec![(0, counters(0, 0)), (1, counters(0, 0))]);
        let mut guest = counters(100, 0);
        guest[8] = 100; // subset of user time, not extra capacity
        assert_eq!(
            sampler.sample(vec![(0, guest), (1, counters(0, 300))]).1,
            Some(25.0)
        );
    }

    #[test]
    fn missing_reset_and_zero_interval_are_not_valid_pressure_samples() {
        let mut sampler = CpuSampler::default();
        sampler.sample(vec![(0, counters(100, 100))]);
        assert_eq!(sampler.sample(vec![(0, counters(100, 100))]).1, None);
        assert_eq!(sampler.sample(vec![(0, counters(1, 1))]).1, None);
        assert_eq!(sampler.sample(vec![]).1, None);
        assert_eq!(sampler.sample(vec![(0, counters(200, 200))]).1, None);
        assert_eq!(cpu_share(50.0, 0.0), 0.0);
    }

    #[test]
    fn game_and_descendants_are_protected_without_protecting_every_process() {
        let snapshot = vec![
            ProcInfo {
                pid: 1,
                ..Default::default()
            },
            ProcInfo {
                pid: 4100,
                ppid: 1,
                cmdline: Arc::new("/games/steamapps/common/Game/game".into()),
                ..Default::default()
            },
            ProcInfo {
                pid: 4101,
                ppid: 4100,
                ..Default::default()
            },
            ProcInfo {
                pid: 4102,
                ppid: 4101,
                ..Default::default()
            },
            ProcInfo {
                pid: 4200,
                ppid: 1,
                ..Default::default()
            },
        ];
        let protected = protected_processes(&snapshot, &[], &HashMap::new());
        for pid in [4100, 4101, 4102] {
            assert!(protected.contains(&pid));
        }
        assert!(!protected.contains(&4200));
    }
}
