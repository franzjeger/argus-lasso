//! System-load-gated ProBalance: reduce eligible process priorities only after
//! sustained overall pressure; restore under lower pressure or low process use.
//! All utilization percentages are shares of total available CPU capacity.

use std::collections::HashMap;

use crate::config::ProBalanceConfig;
use crate::utils;

/// Pure exempt-list check: case-insensitive substring match against any pattern.
/// Empty patterns are ignored so a stray blank entry in config doesn't exempt
/// every process.
fn is_exempt_match(name: &str, patterns: &[String]) -> bool {
    let lower = name.to_lowercase();
    patterns
        .iter()
        .filter(|p| !p.is_empty())
        .any(|p| lower.contains(&p.to_lowercase()))
}

// ── Per-process state ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ProcState {
    Normal,
    Throttled,
}

/// Which mechanism actually throttled this process — restore must use the
/// same one even if the configured method changes mid-throttle.
#[derive(Debug, Clone, PartialEq)]
enum Applied {
    /// With each thread's value from before, when it could be read.
    Nice(Option<utils::ThreadNices>),
    Cgroup {
        unit: String,
    },
}

#[derive(Debug, Clone)]
struct ProcEntry {
    /// Start time of the process this entry describes, so a new process
    /// that reuses the PID never inherits it.
    start_ticks: u64,
    state: ProcState,
    consecutive_high: f32, // seconds spent above threshold
    consecutive_low: f32,  // seconds spent below restore threshold
    original_nice: Option<i32>,
    throttle_nice: Option<i32>,
    applied: Option<Applied>,
}

impl ProcEntry {
    fn new(original_nice: i32) -> Self {
        Self {
            start_ticks: 0,
            state: ProcState::Normal,
            consecutive_high: 0.0,
            consecutive_low: 0.0,
            original_nice: Some(original_nice),
            throttle_nice: None,
            applied: None,
        }
    }
}

/// Refcounted per-unit throttle: N hog PIDs in one app scope share one
/// CPUWeight change, restored when the last one calms down or dies.
#[derive(Debug, Clone)]
struct UnitThrottle {
    count: usize,
    original: crate::cgroup::CpuPolicy,
}

// ── Throttle detail for UI display ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ThrottleInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub original_nice: i32,
    pub throttle_nice: i32,
    /// Seconds spent below restore threshold so far (progress toward restore).
    pub consecutive_low: f32,
    /// Target seconds below restore threshold before restoring.
    pub restore_hysteresis: f32,
    /// systemd unit throttled via CPUWeight, when the cgroup method applied.
    pub unit: Option<String>,
}

// ── A snapshot of one process (what ProBalance needs) ────────────────────────

#[derive(Debug, Clone)]
pub struct ProcSnapshot {
    pub pid: u32,
    /// With `pid`, the process's identity: a PID can be reused.
    pub start_ticks: u64,
    pub name: String,
    pub cpu_percent: f32,
    pub nice: i32,
}

// ── State-machine kernel (pure, syscall-free) ────────────────────────────────
//
// `tick()` splits cleanly into three steps:
//   1. decide()           — advance counters, choose an action
//   2. utils::set_nice()  — the only impure call
//   3. finalize_*()       — commit the action's effect on the entry
//
// The split exists so the state machine can be unit-tested without touching
// any process. Tests drive decide() directly and simulate syscall outcomes
// by passing `ok = true` / `false` into the finalize helpers.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    None,
    Throttle { new_nice: i32 },
    Restore { original_nice: i32 },
}

/// Pure state-machine step. Mutates `entry` counters and returns the action
/// to apply. Does NOT change `entry.state` — that's the finalize_*'s job.
fn decide(
    entry: &mut ProcEntry,
    proc: &ProcSnapshot,
    tick_seconds: f32,
    cfg: &ProBalanceConfig,
    system_cpu: Option<f32>,
) -> Decision {
    let system_cpu = system_cpu.filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
    match entry.state {
        ProcState::Normal => {
            if system_cpu.is_some_and(|v| v > cfg.system_cpu_threshold_percent)
                && proc.cpu_percent >= cfg.process_min_cpu_percent
            {
                entry.consecutive_high += tick_seconds;
                if entry.consecutive_high >= cfg.consecutive_seconds {
                    // Never below the process's own value: a hog already at
                    // nice 19 would be "throttled" to the floor of 15, that
                    // is, given more CPU.
                    let new_nice = (proc.nice + cfg.nice_adjustment)
                        .min(cfg.nice_floor)
                        .max(proc.nice);
                    if new_nice == proc.nice && cfg.method == "nice" {
                        // Nothing a nice change can do; cgroup methods still
                        // can.
                        return Decision::None;
                    }
                    // Capture the pre-throttle nice so we can restore it later,
                    // even if the syscall fails and we retry on a future tick.
                    entry.original_nice = Some(proc.nice);
                    return Decision::Throttle { new_nice };
                }
            } else {
                entry.consecutive_high = 0.0;
            }
        }
        ProcState::Throttled => {
            if system_cpu.is_none_or(|v| v < cfg.system_restore_threshold_percent)
                || proc.cpu_percent < cfg.process_min_cpu_percent
            {
                entry.consecutive_low += tick_seconds;
                if entry.consecutive_low >= cfg.restore_hysteresis_seconds {
                    let orig = entry.original_nice.unwrap_or(0);
                    return Decision::Restore {
                        original_nice: orig,
                    };
                }
            } else {
                entry.consecutive_low = 0.0;
            }
        }
    }
    Decision::None
}

/// Apply the result of a Throttle decision after the set_nice syscall.
/// On failure the entry stays Normal with consecutive_high intact, so the
/// next tick can retry without re-accumulating the trigger window.
fn finalize_throttle(entry: &mut ProcEntry, new_nice: i32, syscall_ok: bool) {
    if syscall_ok {
        entry.state = ProcState::Throttled;
        entry.throttle_nice = Some(new_nice);
        entry.consecutive_high = 0.0;
        entry.consecutive_low = 0.0;
    }
}

/// Apply the result of a Restore decision. State always returns to Normal —
/// even if set_nice failed, we shouldn't keep marking the process as throttled
/// when our own bookkeeping is the only thing remembering it.
fn finalize_restore(entry: &mut ProcEntry, original_nice: i32) {
    entry.state = ProcState::Normal;
    entry.consecutive_high = 0.0;
    entry.consecutive_low = 0.0;
    entry.original_nice = Some(original_nice);
    entry.throttle_nice = None;
}

// ── ProBalance ────────────────────────────────────────────────────────────────

pub struct ProBalance {
    cfg: ProBalanceConfig,
    states: HashMap<u32, ProcEntry>,
    /// unit name → refcounted cgroup throttle (see docs/design-cgroup-probalance.md)
    unit_refs: HashMap<String, UnitThrottle>,
    /// Units where set-property failed — don't retry every tick
    cgroup_failed_units: std::collections::HashSet<String>,
    /// PIDs logged as un-throttleable under method="cgroup" (log once)
    cgroup_failed_pids: std::collections::HashSet<u32>,
    /// Where the unit throttles are recorded (see `with_journal`).
    journal: Option<std::path::PathBuf>,
    log_callback: Option<Box<dyn Fn(String) + Send>>,
}

/// Where the daemon records unit throttles: the runtime directory, which
/// lives exactly as long as systemd's `--runtime` properties do.
pub fn journal_path() -> Option<std::path::PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(std::path::PathBuf::from(runtime).join("argus-lasso/cgroup-throttles.json"))
}

impl ProBalance {
    pub fn new(mut cfg: ProBalanceConfig) -> Self {
        cfg.normalize();
        Self {
            cfg,
            states: HashMap::new(),
            unit_refs: HashMap::new(),
            cgroup_failed_units: std::collections::HashSet::new(),
            cgroup_failed_pids: std::collections::HashSet::new(),
            journal: None,
            log_callback: None,
        }
    }

    /// Record unit throttles in `path` as they are made, and take over the
    /// ones a previous run left there. A throttle outlives a crash or kill
    /// (systemd keeps the property until logout), and a later run would
    /// otherwise read the throttled weight as the unit's own and never put
    /// the real one back. Taken-over throttles are restored on the first
    /// tick, like any restore that failed.
    pub fn with_journal(mut self, path: Option<std::path::PathBuf>) -> Self {
        if let Some(path) = &path {
            let left: HashMap<String, crate::cgroup::CpuPolicy> =
                crate::cgroup::read_json_capped(path).unwrap_or_default();
            for (unit, original) in left {
                self.unit_refs
                    .insert(unit, UnitThrottle { count: 0, original });
            }
        }
        self.journal = path;
        self
    }

    fn save_journal(&self) {
        let Some(path) = &self.journal else {
            return;
        };
        let originals: HashMap<&String, &crate::cgroup::CpuPolicy> = self
            .unit_refs
            .iter()
            .map(|(unit, t)| (unit, &t.original))
            .collect();
        if let Err(e) = crate::cgroup::write_json_private(path, &originals) {
            log::warn!("cannot record cgroup throttles in {}: {e}", path.display());
        }
    }

    pub fn update_config(&mut self, mut cfg: ProBalanceConfig) {
        cfg.normalize();
        // Never mix consecutive windows collected under different thresholds.
        if cfg != self.cfg {
            for entry in self.states.values_mut() {
                entry.consecutive_high = 0.0;
                entry.consecutive_low = 0.0;
            }
        }
        // Disabling ProBalance must not strand processes at their penalty
        // nice — restore everything we throttled before dropping the state.
        if !cfg.enabled && self.cfg.enabled {
            self.restore_all("ProBalance disabled");
        }
        // Method/weight changes get a fresh chance on previously failed targets.
        self.cgroup_failed_units.clear();
        self.cgroup_failed_pids.clear();
        self.cfg = cfg;
    }

    /// Restore all throttled processes before the app exits.
    pub fn shutdown(&mut self) {
        self.restore_all("shutdown");
    }

    /// Restore every currently throttled process (via whichever mechanism
    /// throttled it) and forget all tracked state.
    fn restore_all(&mut self, reason: &str) {
        let mut pending_logs: Vec<String> = Vec::new();
        let entries: Vec<(u32, ProcEntry)> = self.states.drain().collect();
        for (pid, entry) in entries {
            if entry.state == ProcState::Throttled {
                let now = utils::get_nice(pid);
                self.undo_applied(pid, &entry, now, reason, &mut pending_logs);
            }
        }
        // Successfully restored units were removed by release_unit; entries
        // that failed to restore stay (count 0) for the per-tick retry.
        for msg in pending_logs {
            self.log(msg);
        }
    }

    /// Undo one throttle using the mechanism that applied it. `None` applied
    /// means a legacy/nice entry.
    /// `current_nice` is the process's nice now. A nice throttle is only put
    /// back while it is still the value ProBalance set: a manual change, a
    /// rule or another tool that changed it since owns it now.
    fn undo_applied(
        &mut self,
        pid: u32,
        entry: &ProcEntry,
        current_nice: Option<i32>,
        reason: &str,
        logs: &mut Vec<String>,
    ) {
        let original_nice = entry.original_nice.unwrap_or(0);
        match &entry.applied {
            Some(Applied::Cgroup { unit }) => {
                let unit = unit.clone();
                self.release_unit(&unit, reason, logs);
            }
            // `None`: a legacy entry, which was a nice throttle.
            Some(Applied::Nice(_)) | None => {
                let before = match &entry.applied {
                    Some(Applied::Nice(before)) => before.clone(),
                    _ => None,
                };
                if let Some(now) = current_nice.filter(|now| Some(*now) != entry.throttle_nice) {
                    logs.push(format!(
                        "[ProBalance] RESTORE ({reason}) PID {pid} left at nice {now}: changed since the throttle"
                    ));
                } else if before
                    .map_or_else(|| utils::set_nice(pid, original_nice), |b| b.restore(pid))
                    .is_ok()
                {
                    logs.push(format!(
                        "[ProBalance] RESTORE ({reason}) PID {pid} nice→{original_nice}"
                    ));
                }
            }
        }
    }

    /// Drop one reference on a unit throttle; restore the unit when the last
    /// reference goes away. On restore failure the entry is KEPT (count 0) so
    /// the recorded original weight isn't lost — a later throttle would
    /// otherwise read the throttled weight and record it as "original",
    /// polluting every future restore. Zero-count entries are retried each
    /// tick.
    fn release_unit(&mut self, unit: &str, reason: &str, logs: &mut Vec<String>) {
        let Some(t) = self.unit_refs.get_mut(unit) else {
            return;
        };
        t.count = t.count.saturating_sub(1);
        if t.count == 0 {
            let original = t.original.clone();
            match crate::cgroup::restore_unit(unit, &original) {
                crate::cgroup::Restore::Done => {
                    self.unit_refs.remove(unit);
                    self.save_journal();
                    logs.push(format!(
                        "[ProBalance] RESTORE ({reason}) unit {unit} {original}"
                    ));
                }
                // The app closed and systemd removed its scope.
                crate::cgroup::Restore::Gone => {
                    self.unit_refs.remove(unit);
                    self.save_journal();
                }
                crate::cgroup::Restore::Failed => logs.push(format!(
                    "[ProBalance] RESTORE ({reason}) unit {unit} FAILED — will retry"
                )),
            }
        }
    }

    /// Retry unit restores that failed earlier (zero-reference entries).
    fn retry_pending_unit_restores(&mut self, logs: &mut Vec<String>) {
        let pending: Vec<String> = self
            .unit_refs
            .iter()
            .filter(|(_, t)| t.count == 0)
            .map(|(u, _)| u.clone())
            .collect();
        for unit in pending {
            let Some(original) = self.unit_refs.get(&unit).map(|t| t.original.clone()) else {
                continue;
            };
            match crate::cgroup::restore_unit(&unit, &original) {
                crate::cgroup::Restore::Done => {
                    self.unit_refs.remove(&unit);
                    self.save_journal();
                    logs.push(format!(
                        "[ProBalance] RESTORE (retry) unit {unit} {original}"
                    ));
                }
                crate::cgroup::Restore::Gone => {
                    self.unit_refs.remove(&unit);
                    self.save_journal();
                }
                crate::cgroup::Restore::Failed => {}
            }
        }
    }

    /// Try to throttle one process using the configured method. Returns the
    /// mechanism that succeeded, or None (state machine stays Normal and
    /// retries next tick).
    fn apply_throttle(
        &mut self,
        proc: &ProcSnapshot,
        new_nice: i32,
        logs: &mut Vec<String>,
    ) -> Option<Applied> {
        let method = self.cfg.method.as_str();
        let cgroup_wanted = matches!(method, "cgroup" | "auto");

        if cgroup_wanted {
            match crate::cgroup::unit_for_pid(proc.pid) {
                Some(unit) if !self.cgroup_failed_units.contains(&unit) => {
                    if let Some(t) = self.unit_refs.get_mut(&unit) {
                        // Unit already throttled by another hog PID — share it.
                        t.count += 1;
                        logs.push(format!(
                            "[ProBalance] THROTTLE {}({}) cpu={:.1}% joins throttled unit {unit}",
                            proc.name, proc.pid, proc.cpu_percent
                        ));
                        return Some(Applied::Cgroup { unit });
                    }
                    let current = crate::cgroup::read_unit_cpu_policy(
                        &unit,
                        proc.pid,
                        self.cfg.cgroup_quota_percent > 0,
                    );
                    if let Some(current) = current {
                        let (set, original) = crate::cgroup::plan_throttle(
                            &current,
                            self.cfg.cgroup_throttle_weight,
                            self.cfg.cgroup_quota_percent,
                        );
                        // Recorded before it is made, so a crash in between
                        // still leaves the original to put back.
                        self.unit_refs
                            .insert(unit.clone(), UnitThrottle { count: 1, original });
                        self.save_journal();
                        let outcome = crate::cgroup::throttle_unit(&unit, &set);
                        if outcome == crate::cgroup::Outcome::Done {
                            let what = if set == crate::cgroup::CpuPolicy::default() {
                                "already limited further".to_string()
                            } else {
                                set.to_string()
                            };
                            logs.push(format!(
                                "[ProBalance] THROTTLE {}({}) cpu={:.1}% unit {unit} {what}",
                                proc.name, proc.pid, proc.cpu_percent
                            ));
                            return Some(Applied::Cgroup { unit });
                        }
                        if outcome == crate::cgroup::Outcome::Unknown {
                            // The change may still land. Keep the original
                            // with no holder, so the pending-restore retry
                            // puts it back; forgetting it let a later
                            // throttle read the throttled weight as original.
                            if let Some(t) = self.unit_refs.get_mut(&unit) {
                                t.count = 0;
                            }
                        } else {
                            self.unit_refs.remove(&unit);
                        }
                        self.save_journal();
                    }
                    self.cgroup_failed_units.insert(unit.clone());
                    logs.push(format!(
                        "[ProBalance] cgroup throttle FAILED for unit {unit}{}",
                        if method == "auto" {
                            " — falling back to nice"
                        } else {
                            ""
                        }
                    ));
                    if method == "cgroup" {
                        return None;
                    }
                }
                Some(_) if method == "cgroup" => return None, // known-failed unit
                Some(_) => {}                                 // auto: fall through to nice
                None if method == "cgroup" => {
                    if self.cgroup_failed_pids.insert(proc.pid) {
                        logs.push(format!(
                            "[ProBalance] {}({}) has no throttleable user unit (method=cgroup) — skipping",
                            proc.name, proc.pid
                        ));
                    }
                    return None;
                }
                None => {} // auto: fall through to nice
            }
        }

        // Nice path (method="nice", or auto-fallback)
        if new_nice <= proc.nice {
            return None; // already at least this low in priority
        }
        let before = utils::ThreadNices::read(proc.pid);
        if utils::set_nice(proc.pid, new_nice).is_ok() {
            logs.push(format!(
                "[ProBalance] THROTTLE {}({}) cpu={:.1}% nice {}→{}",
                proc.name, proc.pid, proc.cpu_percent, proc.nice, new_nice
            ));
            Some(Applied::Nice(before))
        } else {
            None
        }
    }

    pub fn set_log_callback<F: Fn(String) + Send + 'static>(&mut self, cb: F) {
        self.log_callback = Some(Box::new(cb));
    }

    fn log(&self, msg: String) {
        log::info!("{msg}");
        if let Some(cb) = &self.log_callback {
            cb(msg);
        }
    }

    fn is_exempt(&self, name: &str) -> bool {
        is_exempt_match(name, &self.cfg.exempt_patterns)
    }

    /// Called every ~1s with the current process snapshot.
    /// tick_seconds is the elapsed time since the last tick.
    pub fn tick(
        &mut self,
        snapshot: &[ProcSnapshot],
        tick_seconds: f32,
        system_cpu: Option<f32>,
        protected_pids: &std::collections::HashSet<u32>,
    ) {
        // A long scheduler pause is not evidence of consecutive high samples.
        let system_cpu = if tick_seconds.is_finite() && (0.0..=2.5).contains(&tick_seconds) {
            system_cpu
        } else {
            None
        };
        let tick_seconds = if tick_seconds.is_finite() {
            tick_seconds.clamp(0.0, 2.5)
        } else {
            0.0
        };
        // Failed unit restores are retried even while disabled — a unit left
        // at the throttle weight must not depend on ProBalance staying on.
        if !self.unit_refs.is_empty() {
            let mut retry_logs = Vec::new();
            self.retry_pending_unit_restores(&mut retry_logs);
            for msg in retry_logs {
                self.log(msg);
            }
        }
        if !self.cfg.enabled {
            return;
        }

        // Collect log messages separately to avoid holding &mut self while calling self.log()
        let mut pending_logs: Vec<String> = Vec::new();

        // Clean up dead PIDs. A cgroup-throttled unit outlives its hog PID, so
        // release the unit reference instead of just dropping the entry. A PID
        // now held by a different process (another start time) is dead too:
        // restoring the old process's nice onto the new one would change a
        // process ProBalance never touched.
        let alive: HashMap<u32, u64> = snapshot.iter().map(|p| (p.pid, p.start_ticks)).collect();
        let dead: Vec<u32> = self
            .states
            .iter()
            .filter(|(pid, entry)| alive.get(pid) != Some(&entry.start_ticks))
            .map(|(&pid, _)| pid)
            .collect();
        for pid in dead {
            if let Some(entry) = self.states.remove(&pid) {
                if entry.state == ProcState::Throttled {
                    if let Some(Applied::Cgroup { unit }) = &entry.applied {
                        let unit = unit.clone();
                        self.release_unit(&unit, "process exited", &mut pending_logs);
                    }
                    // Nice: nothing to restore — the process is gone.
                }
            }
        }
        self.cgroup_failed_pids.retain(|p| alive.contains_key(p));

        // Unit-level CPUWeight must never penalize a protected neighbor.
        let mut protected = protected_pids.clone();
        protected.extend(
            snapshot
                .iter()
                .filter(|p| self.is_exempt(&p.name))
                .map(|p| p.pid),
        );
        if self.cfg.method != "nice" || !self.unit_refs.is_empty() {
            let units: std::collections::HashSet<_> = protected
                .iter()
                .filter_map(|p| crate::cgroup::unit_for_pid(*p))
                .collect();
            for proc in snapshot {
                if crate::cgroup::unit_for_pid(proc.pid).is_some_and(|unit| units.contains(&unit)) {
                    protected.insert(proc.pid);
                }
            }
        }
        for proc in snapshot {
            if protected.contains(&proc.pid) {
                // A process exempted *after* being throttled must be restored,
                // not silently abandoned in its throttled state.
                if let Some(entry) = self.states.remove(&proc.pid) {
                    if entry.state == ProcState::Throttled {
                        self.undo_applied(
                            proc.pid,
                            &entry,
                            Some(proc.nice),
                            "exempted",
                            &mut pending_logs,
                        );
                    }
                }
                continue;
            }

            // Pure step: advance counters and decide what to do. No syscalls.
            // (Scoped so the entry borrow ends before the impure application —
            // apply_throttle/undo_applied need &mut self.)
            let decision = {
                let entry = self.states.entry(proc.pid).or_insert_with(|| ProcEntry {
                    start_ticks: proc.start_ticks,
                    ..ProcEntry::new(proc.nice)
                });
                decide(entry, proc, tick_seconds, &self.cfg, system_cpu)
            };

            // Apply side effects at the boundary, then finalize entry state.
            match decision {
                Decision::None => {}
                Decision::Throttle { new_nice } => {
                    let applied = self.apply_throttle(proc, new_nice, &mut pending_logs);
                    if let Some(entry) = self.states.get_mut(&proc.pid) {
                        finalize_throttle(entry, new_nice, applied.is_some());
                        entry.applied = applied;
                    }
                }
                Decision::Restore { original_nice } => {
                    if let Some(entry) = self.states.get(&proc.pid).cloned() {
                        self.undo_applied(
                            proc.pid,
                            &entry,
                            Some(proc.nice),
                            "calmed down",
                            &mut pending_logs,
                        );
                    }
                    if let Some(entry) = self.states.get_mut(&proc.pid) {
                        finalize_restore(entry, original_nice);
                        entry.applied = None;
                    }
                }
            }
        }

        // Flush log messages now that the mutable borrow of self.states is released
        for msg in pending_logs {
            self.log(msg);
        }
    }

    /// The nice value each process throttled through nice had before, for a
    /// rule that takes the value over: what it should put back later is this,
    /// not the throttle.
    pub fn held_nices(&self) -> HashMap<u32, utils::ThreadNices> {
        self.states
            .iter()
            .filter(|(_, e)| e.state == ProcState::Throttled)
            .filter_map(|(&pid, e)| match &e.applied {
                Some(Applied::Nice(Some(before))) => Some((pid, before.clone())),
                Some(Applied::Nice(None)) | None => {
                    Some((pid, utils::ThreadNices::uniform(e.original_nice?)))
                }
                Some(Applied::Cgroup { .. }) => None,
            })
            .collect()
    }

    /// Return the set of currently throttled PIDs (for UI display).
    pub fn throttled_pids(&self) -> std::collections::HashSet<u32> {
        self.states
            .iter()
            .filter(|(_, e)| e.state == ProcState::Throttled)
            .map(|(&pid, _)| pid)
            .collect()
    }

    /// Test-only: count how many PIDs are currently being tracked. Exposed via
    /// `pub(crate)` so we can verify exempt processes are skipped in unit tests
    /// without exercising the syscall path.
    #[cfg(test)]
    pub(crate) fn tracked_pid_count(&self) -> usize {
        self.states.len()
    }

    /// Return detailed info for all currently throttled processes.
    pub fn throttle_infos(&self, snapshot: &[ProcSnapshot]) -> Vec<ThrottleInfo> {
        self.states
            .iter()
            .filter(|(_, e)| e.state == ProcState::Throttled)
            .map(|(&pid, e)| {
                let (name, cpu_percent) = snapshot
                    .iter()
                    .find(|p| p.pid == pid)
                    .map(|p| (p.name.as_str(), p.cpu_percent))
                    .unwrap_or(("unknown", 0.0));
                ThrottleInfo {
                    pid,
                    name: name.to_string(),
                    cpu_percent,
                    original_nice: e.original_nice.unwrap_or(0),
                    throttle_nice: e.throttle_nice.unwrap_or(0),
                    consecutive_low: e.consecutive_low,
                    restore_hysteresis: self.cfg.restore_hysteresis_seconds,
                    unit: match &e.applied {
                        Some(Applied::Cgroup { unit }) => Some(unit.clone()),
                        _ => None,
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decide(
        entry: &mut ProcEntry,
        proc: &ProcSnapshot,
        seconds: f32,
        cfg: &ProBalanceConfig,
    ) -> Decision {
        super::decide(entry, proc, seconds, cfg, Some(proc.cpu_percent))
    }
    use crate::config::ProBalanceConfig;

    fn pat(s: &str) -> Vec<String> {
        vec![s.into()]
    }

    #[test]
    fn exempt_match_is_case_insensitive() {
        assert!(is_exempt_match("KWin", &pat("kwin")));
        assert!(is_exempt_match("kwin_wayland", &pat("KWIN")));
    }

    #[test]
    fn exempt_match_is_substring_not_anchored() {
        // "systemd" should match "systemd-journald" too — this is intentional.
        assert!(is_exempt_match("systemd-journald", &pat("systemd")));
        assert!(is_exempt_match("dbus-systemd-helper", &pat("systemd")));
    }

    #[test]
    fn exempt_match_no_pattern_means_not_exempt() {
        assert!(!is_exempt_match("anything", &[]));
    }

    #[test]
    fn exempt_match_blank_pattern_is_ignored() {
        // Empty patterns must not exempt every process — guards against a stray
        // blank entry in the exempt list config nuking ProBalance silently.
        assert!(!is_exempt_match("steam", &pat("")));
        // But a real pattern alongside the blank still works.
        let mixed = ["".to_string(), "steam".to_string()];
        assert!(is_exempt_match("steam", &mixed));
    }

    #[test]
    fn exempt_match_unrelated_name() {
        assert!(!is_exempt_match("firefox", &pat("kwin")));
    }

    #[test]
    fn tick_disabled_is_noop() {
        let cfg = ProBalanceConfig {
            enabled: false,
            ..Default::default()
        };
        let mut pb = ProBalance::new(cfg);
        let snap = vec![ProcSnapshot {
            pid: 99999,
            name: "anything".into(),
            cpu_percent: 99.0,
            nice: 0,
            start_ticks: 0,
        }];
        pb.tick(&snap, 1.0, Some(10.0), &Default::default());
        // Disabled → state map stays empty, no syscalls attempted.
        assert_eq!(pb.tracked_pid_count(), 0);
    }

    #[test]
    fn tick_skips_exempt_processes_without_tracking() {
        let cfg = ProBalanceConfig {
            exempt_patterns: vec!["kwin".into()],
            ..Default::default()
        };
        let mut pb = ProBalance::new(cfg);
        let snap = vec![ProcSnapshot {
            pid: 4242,
            name: "kwin_wayland".into(),
            cpu_percent: 99.0,
            nice: 0,
            start_ticks: 0,
        }];
        pb.tick(&snap, 10.0, Some(10.0), &Default::default());
        // Exempt processes are skipped entirely — no entry created in state map.
        assert_eq!(pb.tracked_pid_count(), 0);
    }

    #[test]
    fn tick_tracks_non_exempt_below_threshold_without_throttling() {
        // Below threshold → entry exists but stays Normal, no syscalls fire.
        let mut pb = ProBalance::new(ProBalanceConfig::default());
        let snap = vec![ProcSnapshot {
            pid: 4242,
            name: "myapp".into(),
            cpu_percent: 10.0,
            nice: 0,
            start_ticks: 0,
        }];
        pb.tick(&snap, 1.0, Some(10.0), &Default::default());
        assert_eq!(pb.tracked_pid_count(), 1);
        assert!(pb.throttled_pids().is_empty());
    }

    /// A PID reused by a new process between two ticks must not inherit the
    /// old process's throttled entry — its restore would renice a process
    /// ProBalance never touched.
    #[test]
    fn a_reused_pid_does_not_inherit_the_old_process_entry() {
        let mut pb = ProBalance::new(ProBalanceConfig::default());
        let mut old = ProcEntry::new(0);
        old.start_ticks = 100;
        old.state = ProcState::Throttled;
        old.throttle_nice = Some(10);
        pb.states.insert(4242, old);

        let reused = ProcSnapshot {
            pid: 4242,
            start_ticks: 200,
            name: "newcomer".into(),
            cpu_percent: 1.0,
            nice: 7,
        };
        pb.tick(&[reused], 1.0, Some(10.0), &Default::default());

        assert!(pb.throttled_pids().is_empty());
        let entry = &pb.states[&4242];
        assert_eq!(entry.start_ticks, 200);
        assert_eq!(entry.original_nice, Some(7));
    }

    /// A hog already niced to 19 was "throttled" to the floor of 15, that
    /// is, given more CPU.
    #[test]
    fn a_throttle_never_lowers_nice() {
        let mut cfg = cfg_for_state_tests();
        cfg.nice_adjustment = 10;
        cfg.nice_floor = 15;
        cfg.method = "nice".into();
        let mut e = ProcEntry::new(19);
        for _ in 0..5 {
            assert_eq!(
                decide(&mut e, &snap(1, 90.0, 19), 1.0, &cfg),
                Decision::None
            );
        }
        cfg.method = "auto".into();
        let mut e = ProcEntry::new(19);
        let decisions: Vec<Decision> = (0..5)
            .map(|_| decide(&mut e, &snap(1, 90.0, 19), 1.0, &cfg))
            .collect();
        assert!(
            decisions.contains(&Decision::Throttle { new_nice: 19 }),
            "cgroup can still act"
        );
        assert!(!decisions.contains(&Decision::Throttle { new_nice: 15 }));
    }

    /// A manual nice change on a throttled process was undone when it was
    /// exempted (the manual change marks it protected) or calmed down.
    #[test]
    fn a_restore_leaves_a_nice_changed_since_alone() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = crate::fast_proc::read_stat(pid, &mut [0; 1024])
            .unwrap()
            .starttime;
        utils::set_nice(pid, 15).unwrap();

        let mut pb = ProBalance::new(ProBalanceConfig::default());
        // An original above the current value, so that putting it back is
        // allowed without privilege and the test means the same everywhere.
        let mut entry = ProcEntry::new(19);
        entry.original_nice = Some(19);
        entry.start_ticks = start;
        entry.state = ProcState::Throttled;
        entry.throttle_nice = Some(10);
        entry.applied = Some(Applied::Nice(None));
        pb.states.insert(pid, entry);
        let now = ProcSnapshot {
            pid,
            start_ticks: start,
            name: "sleep".into(),
            cpu_percent: 0.0,
            nice: 15,
        };
        pb.tick(&[now], 1.0, Some(10.0), &[pid].into());
        let after = utils::get_nice(pid);
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(after, Some(15));
        assert!(pb.throttled_pids().is_empty());
    }

    #[test]
    fn tick_evicts_dead_pids() {
        let mut pb = ProBalance::new(ProBalanceConfig::default());
        let snap1 = vec![ProcSnapshot {
            pid: 4242,
            name: "myapp".into(),
            cpu_percent: 10.0,
            nice: 0,
            start_ticks: 0,
        }];
        pb.tick(&snap1, 1.0, Some(10.0), &Default::default());
        assert_eq!(pb.tracked_pid_count(), 1);

        // PID disappears from snapshot → state entry should be cleaned up.
        pb.tick(&[], 1.0, Some(10.0), &Default::default());
        assert_eq!(pb.tracked_pid_count(), 0);
    }

    // ── State-machine kernel tests (drive decide() directly) ────────────────
    //
    // These exercise the full Normal↔Throttled lifecycle with no syscalls by
    // calling decide() and feeding the result to finalize_*() directly. They
    // verify the pure decisions, the threshold/hysteresis windows, the
    // nice_floor cap, and the syscall-failure retry behavior.

    fn cfg_for_state_tests() -> ProBalanceConfig {
        ProBalanceConfig {
            enabled: true,
            system_cpu_threshold_percent: 80.0,
            consecutive_seconds: 3.0,
            nice_adjustment: 5,
            nice_floor: 19,
            system_restore_threshold_percent: 30.0,
            restore_hysteresis_seconds: 4.0,
            exempt_patterns: vec![],
            ..Default::default()
        }
    }

    fn snap(pid: u32, cpu: f32, nice: i32) -> ProcSnapshot {
        ProcSnapshot {
            pid,
            name: "test".into(),
            cpu_percent: cpu,
            nice,
            start_ticks: 0,
        }
    }

    #[test]
    fn decide_below_threshold_resets_consecutive_counter() {
        let cfg = cfg_for_state_tests();
        let mut e = ProcEntry::new(0);
        e.consecutive_high = 2.0;
        let d = decide(&mut e, &snap(1, 10.0, 0), 1.0, &cfg);
        assert_eq!(d, Decision::None);
        assert_eq!(e.consecutive_high, 0.0);
        // Doesn't go negative
        let d = decide(&mut e, &snap(1, 10.0, 0), 5.0, &cfg);
        assert_eq!(d, Decision::None);
        assert_eq!(e.consecutive_high, 0.0);
    }

    #[test]
    fn decide_above_threshold_below_window_no_action() {
        let cfg = cfg_for_state_tests(); // window = 3.0s
        let mut e = ProcEntry::new(0);
        // 1s above threshold — not yet enough.
        let d = decide(&mut e, &snap(1, 95.0, 0), 1.0, &cfg);
        assert_eq!(d, Decision::None);
        assert!((e.consecutive_high - 1.0).abs() < 1e-6);
        // Total 2s — still not enough.
        let d = decide(&mut e, &snap(1, 95.0, 0), 1.0, &cfg);
        assert_eq!(d, Decision::None);
        assert_eq!(e.state, ProcState::Normal);
    }

    #[test]
    fn decide_throttles_after_consecutive_seconds() {
        let cfg = cfg_for_state_tests();
        let mut e = ProcEntry::new(0);
        let _ = decide(&mut e, &snap(1, 95.0, 0), 1.5, &cfg);
        let d = decide(&mut e, &snap(1, 95.0, 0), 1.5, &cfg); // total 3.0s
        match d {
            Decision::Throttle { new_nice } => {
                // proc.nice (0) + adjustment (5) = 5, capped at nice_floor (19)
                assert_eq!(new_nice, 5);
            }
            other => panic!("expected Throttle, got {other:?}"),
        }
        // decide() captures original_nice but does NOT flip the state — that's finalize_*'s job.
        assert_eq!(e.state, ProcState::Normal);
        assert_eq!(e.original_nice, Some(0));
    }

    #[test]
    fn decide_caps_new_nice_at_nice_floor() {
        let cfg = cfg_for_state_tests(); // floor=19, adjustment=5
        let mut e = ProcEntry::new(18);
        let _ = decide(&mut e, &snap(1, 95.0, 18), 3.0, &cfg);
        // 18 + 5 = 23, but capped at floor=19
        match decide(&mut e, &snap(1, 95.0, 18), 0.01, &cfg) {
            Decision::Throttle { new_nice } => assert_eq!(new_nice, 19),
            // The first decide() already triggered. Re-derive directly:
            _ => {
                // In rare cases the first call was the trigger — that's fine,
                // re-create to verify the cap once.
                let mut e2 = ProcEntry::new(18);
                if let Decision::Throttle { new_nice } =
                    decide(&mut e2, &snap(1, 95.0, 18), 3.5, &cfg)
                {
                    assert_eq!(new_nice, 19);
                } else {
                    panic!("expected Throttle on second attempt");
                }
            }
        }
    }

    #[test]
    fn finalize_throttle_success_flips_state_and_resets_counters() {
        let mut e = ProcEntry::new(0);
        e.consecutive_high = 3.5;
        e.consecutive_low = 1.0;
        finalize_throttle(&mut e, 10, true);
        assert_eq!(e.state, ProcState::Throttled);
        assert_eq!(e.throttle_nice, Some(10));
        assert_eq!(e.consecutive_high, 0.0);
        assert_eq!(e.consecutive_low, 0.0);
    }

    #[test]
    fn finalize_throttle_failure_keeps_state_for_retry() {
        // If set_nice fails (e.g., process exited), we stay Normal with
        // consecutive_high intact — next tick re-decides cleanly.
        let mut e = ProcEntry::new(0);
        e.consecutive_high = 3.0;
        finalize_throttle(&mut e, 10, false);
        assert_eq!(e.state, ProcState::Normal);
        assert_eq!(e.throttle_nice, None);
        assert_eq!(e.consecutive_high, 3.0); // not reset
    }

    #[test]
    fn decide_restored_resets_low_counter_on_high_cpu() {
        let cfg = cfg_for_state_tests();
        let mut e = ProcEntry::new(0);
        e.state = ProcState::Throttled;
        e.consecutive_low = 3.0;
        // CPU above restore_threshold (30%) → counter resets, no decision.
        let d = decide(&mut e, &snap(1, 50.0, 0), 1.0, &cfg);
        assert_eq!(d, Decision::None);
        assert_eq!(e.consecutive_low, 0.0);
        assert_eq!(e.state, ProcState::Throttled);
    }

    #[test]
    fn decide_restores_after_hysteresis_window() {
        let cfg = cfg_for_state_tests(); // restore_hyst = 4.0s
        let mut e = ProcEntry::new(0);
        e.state = ProcState::Throttled;
        e.original_nice = Some(2);
        e.throttle_nice = Some(7);

        // 2s low — not enough.
        let d = decide(&mut e, &snap(1, 5.0, 7), 2.0, &cfg);
        assert_eq!(d, Decision::None);
        // 4.5s total — past hysteresis.
        let d = decide(&mut e, &snap(1, 5.0, 7), 2.5, &cfg);
        match d {
            Decision::Restore { original_nice } => assert_eq!(original_nice, 2),
            other => panic!("expected Restore, got {other:?}"),
        }
        // Still Throttled until finalize runs.
        assert_eq!(e.state, ProcState::Throttled);
    }

    #[test]
    fn finalize_restore_resets_state_regardless_of_syscall() {
        // Even when set_nice fails, we return to Normal — our bookkeeping
        // shouldn't outlive the kernel state we couldn't write.
        let mut e = ProcEntry::new(0);
        e.state = ProcState::Throttled;
        e.throttle_nice = Some(15);
        e.consecutive_low = 4.0;
        finalize_restore(&mut e, 0);
        assert_eq!(e.state, ProcState::Normal);
        assert_eq!(e.throttle_nice, None);
        assert_eq!(e.consecutive_low, 0.0);
        assert_eq!(e.original_nice, Some(0));
    }

    #[test]
    fn full_lifecycle_normal_throttled_normal() {
        // End-to-end: drive a process from idle → high → low → idle through
        // decide() + simulated syscall successes.
        let cfg = cfg_for_state_tests();
        let mut e = ProcEntry::new(0);

        // Stay below threshold first — counter remains 0.
        let _ = decide(&mut e, &snap(1, 5.0, 0), 1.0, &cfg);
        assert_eq!(e.state, ProcState::Normal);

        // CPU spikes for 4s → throttle decision on 3rd second.
        let _ = decide(&mut e, &snap(1, 95.0, 0), 1.0, &cfg);
        let _ = decide(&mut e, &snap(1, 95.0, 0), 1.0, &cfg);
        let d = decide(&mut e, &snap(1, 95.0, 0), 1.0, &cfg);
        let new_nice = match d {
            Decision::Throttle { new_nice } => new_nice,
            other => panic!("expected Throttle, got {other:?}"),
        };
        finalize_throttle(&mut e, new_nice, true);
        assert_eq!(e.state, ProcState::Throttled);

        // CPU drops to 5% — restore after 4s hysteresis.
        let _ = decide(&mut e, &snap(1, 5.0, 5), 2.0, &cfg);
        let d = decide(&mut e, &snap(1, 5.0, 5), 2.0, &cfg);
        let orig = match d {
            Decision::Restore { original_nice } => original_nice,
            other => panic!("expected Restore, got {other:?}"),
        };
        assert_eq!(orig, 0);
        finalize_restore(&mut e, orig);
        assert_eq!(e.state, ProcState::Normal);
        assert_eq!(e.throttle_nice, None);
    }
}

#[cfg(test)]
mod system_pressure_tests {
    use super::*;

    fn process(cpu_percent: f32) -> ProcSnapshot {
        ProcSnapshot {
            pid: 4242,
            name: "background".into(),
            cpu_percent,
            nice: 0,
            start_ticks: 0,
        }
    }

    #[test]
    fn busy_process_on_idle_system_never_activates_policy() {
        let cfg = ProBalanceConfig::default();
        let mut entry = ProcEntry::new(0);
        for _ in 0..20 {
            assert_eq!(
                decide(&mut entry, &process(3.125), 1.0, &cfg, Some(3.125)),
                Decision::None
            );
        }
        assert_eq!(entry.consecutive_high, 0.0);
    }

    #[test]
    fn system_pressure_and_process_share_are_independent_and_sustained() {
        let cfg = ProBalanceConfig::default();
        let mut entry = ProcEntry::new(0);
        assert_eq!(
            decide(&mut entry, &process(5.0), 1.0, &cfg, Some(85.0)),
            Decision::None
        );
        for load in [90.0, 90.0, 80.0, 90.0, 90.0] {
            assert_eq!(
                decide(&mut entry, &process(5.0), 1.0, &cfg, Some(load)),
                Decision::None
            );
        }
        assert!(matches!(
            decide(&mut entry, &process(5.0), 1.0, &cfg, Some(90.0)),
            Decision::Throttle { .. }
        ));
        let mut tiny = ProcEntry::new(0);
        for _ in 0..5 {
            assert_eq!(
                decide(&mut tiny, &process(0.2), 1.0, &cfg, Some(100.0)),
                Decision::None
            );
        }
    }

    #[test]
    fn recovery_uses_lower_system_threshold_even_when_process_stays_busy() {
        let cfg = ProBalanceConfig::default();
        let mut entry = ProcEntry::new(0);
        finalize_throttle(&mut entry, 10, true);
        for _ in 0..10 {
            assert_eq!(
                decide(&mut entry, &process(5.0), 1.0, &cfg, Some(80.0)),
                Decision::None
            );
        }
        for _ in 0..4 {
            assert_eq!(
                decide(&mut entry, &process(5.0), 1.0, &cfg, Some(70.0)),
                Decision::None
            );
        }
        assert_eq!(
            decide(&mut entry, &process(5.0), 1.0, &cfg, Some(70.0)),
            Decision::Restore { original_nice: 0 }
        );
    }

    #[test]
    fn missing_samples_break_activation_and_release_existing_penalties() {
        let cfg = ProBalanceConfig::default();
        let mut entry = ProcEntry::new(0);
        decide(&mut entry, &process(5.0), 2.0, &cfg, Some(90.0));
        assert_eq!(
            decide(&mut entry, &process(5.0), 1.0, &cfg, None),
            Decision::None
        );
        assert_eq!(entry.consecutive_high, 0.0);
        finalize_throttle(&mut entry, 10, true);
        assert_eq!(
            decide(&mut entry, &process(5.0), 5.0, &cfg, None),
            Decision::Restore { original_nice: 0 }
        );
    }

    #[test]
    fn protected_pid_is_not_a_candidate_at_full_system_load() {
        let mut policy = ProBalance::new(ProBalanceConfig::default());
        for _ in 0..6 {
            policy.tick(
                &[process(50.0)],
                1.0,
                Some(100.0),
                &[4242].into_iter().collect(),
            );
        }
        assert!(policy.states.is_empty()); // no syscall and no latent candidate timer
    }

    #[test]
    fn suspension_gap_and_config_changes_do_not_reuse_activation_time() {
        let mut policy = ProBalance::new(ProBalanceConfig::default());
        policy.tick(&[process(5.0)], 1.0, Some(95.0), &Default::default());
        policy.tick(&[process(5.0)], 30.0, Some(95.0), &Default::default());
        assert_eq!(policy.states[&4242].consecutive_high, 0.0);
        policy.tick(&[process(5.0)], 1.0, Some(95.0), &Default::default());
        let mut cfg = policy.cfg.clone();
        cfg.system_cpu_threshold_percent = 90.0;
        policy.update_config(cfg);
        assert_eq!(policy.states[&4242].consecutive_high, 0.0);
    }

    /// A throttle outlives Argus being killed; the next run used to read the
    /// throttled weight as the unit's own. Now it finds the original in the
    /// journal and restores it like a failed restore, on the first tick.
    #[test]
    fn unit_throttles_survive_in_the_journal() {
        use crate::cgroup::{CpuPolicy, Weight};
        let path = std::env::temp_dir().join(format!("argus-journal-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let original = CpuPolicy {
            weight: Some(Weight::Idle),
            quota: None,
        };

        let mut crashed =
            ProBalance::new(ProBalanceConfig::default()).with_journal(Some(path.clone()));
        crashed.unit_refs.insert(
            "app-build.scope".into(),
            UnitThrottle {
                count: 2,
                original: original.clone(),
            },
        );
        crashed.save_journal();
        drop(crashed);

        let next = ProBalance::new(ProBalanceConfig::default()).with_journal(Some(path.clone()));
        let pending = &next.unit_refs["app-build.scope"];
        assert_eq!(pending.count, 0, "restored on the first tick");
        assert_eq!(pending.original, original);

        let mut next = next;
        next.unit_refs.clear();
        next.save_journal();
        assert!(!path.exists(), "nothing left to restore");
    }
}
