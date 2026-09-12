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
    pub name: String,
    pub cpu_percent: f32,
    /// GPU utilization % (NVML per-process SM util; 0.0 without NVIDIA/NVML)
    pub gpu_percent: f32,
    pub mem_rss: u64, // bytes
    pub nice: i32,
    pub affinity: String,
    pub ionice: String,
    pub disk_read_bps: u64,  // bytes/s
    pub disk_write_bps: u64, // bytes/s
    /// Reference-counted so GUI snapshot clones are O(1) for this field.
    pub cmdline: std::sync::Arc<String>,
}

impl Default for ProcInfo {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            name: String::new(),
            cpu_percent: 0.0,
            gpu_percent: 0.0,
            mem_rss: 0,
            nice: 0,
            affinity: String::new(),
            ionice: String::new(),
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
    /// Rolling totals across all disks: (read MB/s, write MB/s), 120 samples
    pub disk_io_history: std::collections::VecDeque<(f32, f32)>,
    /// Rolling totals across all NICs: (rx MB/s, tx MB/s), 120 samples
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

fn run_loop(
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<DaemonCmd>,
    initial_config: Config,
    rule_engine: Arc<Mutex<RuleEngine>>,
) {
    let mut config = initial_config;

    // Build closures that push log messages into shared state
    let state_log = state.clone();
    let log_cb = move |msg: String| {
        if let Ok(mut s) = state_log.lock() {
            s.append_log(msg);
        }
    };

    let mut probalance = ProBalance::new(config.probalance.clone());
    let mut hw_collector = HwCollector::new();
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

    loop {
        // ── Drain commands from GUI ─────────────────────────────────────────
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DaemonCmd::UpdateConfig(cfg) => {
                    let cfg = *cfg;
                    probalance.update_config(cfg.probalance.clone());
                    config = cfg.clone();
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
            let (new_snapshot, new_cpu_times, sys_total) = collect_snapshot(
                &mut prev_cpu_times,
                prev_sys_total,
                &mut caches,
                detail,
                io_elapsed,
            );
            prev_cpu_times = new_cpu_times;
            prev_sys_total = sys_total;
            raw_snapshot = new_snapshot;

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
                    name: p.name.clone(),
                    cpu_percent: p.cpu_percent,
                    nice: p.nice,
                })
                .collect();
            probalance.tick(&pb_snap, pb_tick);

            // Fire desktop notifications for newly throttled / restored PIDs
            let cur_throttled = probalance.throttled_pids();
            if cur_throttled != prev_throttled && config.ui.notifications_enabled {
                // Build a name lookup from the current snapshot
                let name_map: HashMap<u32, &str> = raw_snapshot
                    .iter()
                    .map(|p| (p.pid, p.name.as_str()))
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

        // ── Snapshot emit every display_refresh_interval ───────────────────
        let refresh = Duration::from_millis(config.monitor.display_refresh_interval_ms);
        if now.duration_since(last_snapshot) >= refresh {
            let throttled = probalance.throttled_pids();
            let pb_snap_for_infos: Vec<crate::probalance::ProcSnapshot> = raw_snapshot
                .iter()
                .map(|p| crate::probalance::ProcSnapshot {
                    pid: p.pid,
                    name: p.name.clone(),
                    cpu_percent: p.cpu_percent,
                    nice: p.nice,
                })
                .collect();
            let throttle_infos = probalance.throttle_infos(&pb_snap_for_infos);
            let cpu_percents = collect_cpu_percents();
            let avg = if cpu_percents.is_empty() {
                0.0
            } else {
                cpu_percents.iter().sum::<f32>() / cpu_percents.len() as f32
            };

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

            if let Ok(mut s) = state.lock() {
                s.snapshot = std::sync::Arc::new(raw_snapshot.clone());
                s.cpu_percents = cpu_percents;
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
    let (_, times, sys_total) = collect_snapshot(&mut prev_times, 0, &mut caches, true, 0.0);
    prev_times = times;
    std::thread::sleep(Duration::from_millis(500));
    let (snap, _, _) = collect_snapshot(&mut prev_times, sys_total, &mut caches, true, 0.5);
    snap
}

/// Sum current disk (read, write) and network (rx, tx) MB/s across all
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
    let cmd = p.cmdline.as_str();
    cmd.contains("steamapps/common") || cmd.contains("/proton ")
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
    name: String,
    cmdline: std::sync::Arc<String>,
}

/// Caches that let a collection pass skip work the callers do not need.
#[derive(Default)]
pub struct SnapshotCaches {
    meta: HashMap<u32, ProcMeta>,
    /// pid → (affinity, ionice) — display-only, so refreshed on the display
    /// cadence rather than the much shorter enforce cadence.
    display: HashMap<u32, (String, String)>,
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
    prev_times: &mut HashMap<u32, u64>,
    prev_sys_total: u64,
    caches: &mut SnapshotCaches,
    detail: bool,
    io_elapsed: f32,
) -> (Vec<ProcInfo>, HashMap<u32, u64>, u64) {
    use procfs::process::all_processes;
    use procfs::WithCurrentSystemInfo;

    let mut new_times: HashMap<u32, u64> = HashMap::new();
    let mut snapshot: Vec<ProcInfo> = Vec::new();

    // Read total system CPU jiffies for CPU% calculation.
    // sys_delta is summed across ALL CPUs, so we scale by the online CPU count
    // to get per-core percentages (100% = one core fully busy, like top).
    // Without this, a busy-loop on a 16-core machine reads ~6% and ProBalance
    // thresholds (default 85%) can never trigger.
    let sys_total = read_sys_cpu_total();
    let sys_delta = sys_total.saturating_sub(prev_sys_total) as f32;
    let n_cpus = utils::get_online_cpus().len().max(1) as f32;

    let procs = match all_processes() {
        Ok(p) => p,
        Err(_) => return (snapshot, new_times, sys_total),
    };

    for proc_result in procs {
        let proc = match proc_result {
            Ok(p) => p,
            Err(_) => continue,
        };

        let pid = proc.pid() as u32;

        let stat = match proc.stat() {
            Ok(s) => s,
            Err(_) => continue,
        };

        let ppid = stat.ppid as u32;

        // Name and command line are fixed for this process's lifetime, so
        // read them once and keep them. start_time distinguishes a reused PID
        // from the same process still running.
        let meta = match caches.meta.get(&pid) {
            Some(m) if m.start_time == stat.starttime => m,
            _ => {
                let cmdline: Vec<String> = proc.cmdline().unwrap_or_default();
                let entry = ProcMeta {
                    start_time: stat.starttime,
                    name: utils::resolve_name(&stat.comm, &cmdline),
                    cmdline: std::sync::Arc::new(cmdline.join(" ")),
                };
                caches.meta.entry(pid).insert_entry(entry).into_mut()
            }
        };
        let name = meta.name.clone();
        let cmdline = std::sync::Arc::clone(&meta.cmdline);

        let proc_ticks = stat.utime + stat.stime;
        new_times.insert(pid, proc_ticks);
        let prev_ticks = prev_times.get(&pid).copied().unwrap_or(proc_ticks);
        let delta_ticks = proc_ticks.saturating_sub(prev_ticks) as f32;
        let cpu_percent = if sys_delta > 0.0 {
            // Multithreaded processes can legitimately exceed 100% (one core);
            // cap at the machine total.
            (delta_ticks / sys_delta * n_cpus * 100.0).min(n_cpus * 100.0)
        } else {
            0.0
        };

        let mem_rss = stat.rss_bytes().get();
        let nice = stat.nice as i32;

        let (affinity, ionice, disk_read_bps, disk_write_bps) = if detail {
            let affinity = utils::get_affinity_str(pid);
            let ionice = read_ionice(pid);
            let (r, w) = read_proc_io(pid, &mut caches.io, io_elapsed);
            caches
                .display
                .insert(pid, (affinity.clone(), ionice.clone()));
            (affinity, ionice, r, w)
        } else {
            // Reuse the last display pass. A PID first seen on an enforce
            // tick simply shows blank until the next display tick, which is
            // sooner than a human can read the row anyway.
            let (a, i) = caches.display.get(&pid).cloned().unwrap_or_default();
            (a, i, 0, 0)
        };

        snapshot.push(ProcInfo {
            pid,
            ppid,
            name,
            cpu_percent,
            gpu_percent: 0.0, // filled at publish time from NVML
            mem_rss,
            nice,
            affinity,
            ionice,
            disk_read_bps,
            disk_write_bps,
            cmdline,
        });
    }

    (snapshot, new_times, sys_total)
}

/// Per-second disk read/write rates for one PID.
///
/// The deltas are divided by the elapsed sample time. They used to be
/// returned raw and labelled "bytes/s", so the figure was wrong by whatever
/// the sampling cadence happened to be — twice the real rate at the default
/// 500 ms.
fn read_proc_io(pid: u32, io_cache: &mut HashMap<u32, (u64, u64)>, elapsed: f32) -> (u64, u64) {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/io")) {
        Ok(t) => t,
        Err(_) => return (0, 0),
    };
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
                .filter_map(|s| s.parse::<u64>().ok())
                .sum();
        }
    }
    0
}

fn collect_cpu_percents() -> Vec<f32> {
    // Read per-CPU utilisation from procfs.
    // Samples are keyed by CPU number (parsed from the "cpuN" label), NOT by
    // line position: after a park/unpark the set of online CPUs changes, and
    // diffing consecutive samples by index would attribute one CPU's jiffies
    // to another for the first sample after every topology change.
    use std::sync::Mutex as StdMutex;

    static PREV: StdMutex<Option<HashMap<u32, [u64; 10]>>> = StdMutex::new(None);

    let total_cpus = utils::get_cpu_count() as usize;
    let new_stats = read_percpu_stats();
    let mut result = vec![0.0f32; total_cpus];

    let mut prev_guard = PREV.lock().unwrap();
    if let Some(prev_map) = prev_guard.as_ref() {
        for (cpu_num, new) in &new_stats {
            let Some(prev) = prev_map.get(cpu_num) else {
                continue; // CPU just came online — no baseline yet
            };
            // Fields: user nice system idle iowait irq softirq steal guest guest_nice
            let prev_total: u64 = prev.iter().sum();
            let new_total: u64 = new.iter().sum();
            let total_delta = new_total.saturating_sub(prev_total) as f32;
            let idle_delta = new[3].saturating_sub(prev[3]) as f32;
            let pct = if total_delta > 0.0 {
                ((total_delta - idle_delta) / total_delta * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            if (*cpu_num as usize) < total_cpus {
                result[*cpu_num as usize] = pct;
            }
        }
    }
    *prev_guard = Some(new_stats.into_iter().collect());
    result
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
    // Read /proc/<pid>/io_prio or use ioprio_get syscall via nix
    // For display, we use the raw ioprio value decoded
    use nix::libc;
    let prio = unsafe {
        libc::syscall(
            libc::SYS_ioprio_get,
            1, /* IOPRIO_WHO_PROCESS */
            pid as libc::c_int,
        )
    };
    if prio < 0 {
        return String::new();
    }
    let class = (prio as u32 >> 13) & 0x7;
    let level = prio as u32 & 0x1fff;
    format!("{class}/{level}")
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
            let mut s = Sensor::new(label, "MB/s");
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
