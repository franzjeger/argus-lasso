//! CPU core parking: take non-preferred CPUs offline via privileged helper.
//!
//! Mirrors Python cpu_park.py:
//!   - detect_topology(): AMD X3D (L3 cache asymmetry), Intel Hybrid (max freq), or UNIFORM
//!   - park_cpus() / unpark_all() via sudo /usr/local/bin/argus-lasso-sysfs
//!   - get_smt_siblings_of(): reads /sys/.../topology/core_id
//!   - Topology cache: preserved across calls so Gaming Mode doesn't lose it once CPUs are parked

use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;
use std::sync::Mutex;

use crate::utils::{cpuset_to_cpulist, get_offline_cpus, read_cpulist_file};

// ── Constants ─────────────────────────────────────────────────────────────────

// ── Privileged helpers ────────────────────────────────────────────────────────
//
// One executable per privileged operation, each with its own polkit action.
// pkexec keys authorisation on the *executable path*, not on arguments, so a
// single helper taking a subcommand can only ever have one policy covering
// all of them — which is what the previous sudoers rule granted: passwordless
// root for every subcommand, including reniceing any PID on the system.

pub const HELPER_DIR: &str = match option_env!("ARGUS_HELPER_DIR") {
    Some(dir) => dir,
    None => "/usr/local/lib/argus-lasso",
};

pub const POLICY_PATH: &str = match option_env!("ARGUS_POLICY_PATH") {
    Some(path) => path,
    None => "/usr/share/polkit-1/actions/io.github.franzjeger.argus-lasso.policy",
};

/// Predecessor install: a single helper plus a blanket NOPASSWD sudoers rule.
/// Removed when the polkit helpers are installed.
pub const LEGACY_HELPER: &str = "/usr/local/bin/argus-lasso-sysfs";
pub const LEGACY_SUDOERS: &str = "/etc/sudoers.d/argus-lasso";

/// Bumped whenever a helper script changes, so the app can tell an outdated
/// install from a missing one. Substring-matched in the installed files.
const HELPER_VERSION: &str = "argus-lasso-helper v3";

/// The three privileged operations, and the file each one lives in.
const OP_PARK: &str = "cpu-park";
const OP_POWER: &str = "power-profile";
const OP_RENICE: &str = "renice";

fn helper_path(op: &str) -> String {
    format!("{HELPER_DIR}/{op}")
}

const PARK_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v3 — CPU parking. Managed by argus-lasso; do not edit.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
case "${1-}" in
    online)
        [[ "${2-}" =~ ^[0-9]+$ ]] || exit 2
        [[ "${3-}" =~ ^[01]$   ]] || exit 2
        echo "$3" > "/sys/devices/system/cpu/cpu$2/online"
        ;;
    unpark-all)
        offline=$(cat /sys/devices/system/cpu/offline 2>/dev/null || true)
        [ -z "$offline" ] && exit 0
        for part in $(echo "$offline" | tr ',' ' '); do
            if [[ "$part" == *-* ]]; then
                lo=${part%-*}; hi=${part#*-}
                for ((c=lo; c<=hi; c++)); do
                    echo 1 > "/sys/devices/system/cpu/cpu${c}/online" 2>/dev/null || true
                done
            else
                echo 1 > "/sys/devices/system/cpu/cpu${part}/online" 2>/dev/null || true
            fi
        done
        ;;
    --check)
        exit 0 ;;
    *)
        echo "usage: cpu-park online <cpu> <0|1> | unpark-all" >&2; exit 2 ;;
esac
"#;

const POWER_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v3 — CPU governor and energy preference.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
case "${1-}" in
    governor)
        [[ "${2-}" =~ ^[a-z_-]+$ ]] || exit 2
        for f in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            echo "$2" > "$f" 2>/dev/null || true
        done
        ;;
    epp)
        [[ "${2-}" =~ ^[a-z_-]+$ ]] || exit 2
        for f in /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference; do
            echo "$2" > "$f" 2>/dev/null || true
        done
        ;;
    *)
        echo "usage: power-profile governor <name> | epp <name>" >&2; exit 2 ;;
esac
"#;

/// Negative nice needs CAP_SYS_NICE, which is why this runs privileged — but
/// it must never reach a process the caller does not own. pkexec exports
/// PKEXEC_UID; without it we are not being invoked through polkit and refuse
/// rather than guess who is asking.
const RENICE_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v3 — renice, restricted to the caller's own processes.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
[[ "${1-}" =~ ^-?[0-9]+$ ]] || exit 2
[[ "${2-}" =~ ^[0-9]+$   ]] || exit 2
nice_val=$1
pid=$2
if [ -z "${PKEXEC_UID-}" ]; then
    echo "refusing: not invoked through pkexec" >&2
    exit 2
fi
owner=$(stat -c %u "/proc/$pid" 2>/dev/null) || { echo "no such process: $pid" >&2; exit 1; }
if [ "$owner" != "$PKEXEC_UID" ]; then
    echo "refusing: PID $pid belongs to uid $owner, not $PKEXEC_UID" >&2
    exit 1
fi
renice -n "$nice_val" -p "$pid" >/dev/null
"#;

/// Three separate actions so an administrator can tighten one without losing
/// the others. All default to `allow_active=yes` — no prompt for the user at
/// the physical seat — which matches what the sudoers rule did, except the
/// grant is now per operation and renice is confined to the caller's own
/// processes by the helper itself.
fn policy_xml() -> String {
    let action = |id: &str, desc: &str, msg: &str, path: String| {
        format!(
            r#"  <action id="io.github.franzjeger.argus-lasso.{id}">
    <description>{desc}</description>
    <message>{msg}</message>
    <defaults>
      <allow_any>no</allow_any>
      <allow_inactive>no</allow_inactive>
      <allow_active>yes</allow_active>
    </defaults>
    <annotate key="org.freedesktop.policykit.exec.path">{path}</annotate>
    <annotate key="org.freedesktop.policykit.exec.allow_gui">true</annotate>
  </action>
"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/PolicyKit/1.0/policyconfig.dtd">
<policyconfig>
  <vendor>Argus-Lasso</vendor>
  <vendor_url>https://github.com/franzjeger/argus-lasso</vendor_url>
{}{}{}</policyconfig>
"#,
        action(
            OP_PARK,
            "Take CPU cores offline or bring them back online",
            "Authentication is required to park CPU cores",
            helper_path(OP_PARK)
        ),
        action(
            OP_POWER,
            "Set the CPU scaling governor and energy performance preference",
            "Authentication is required to change the CPU power profile",
            helper_path(OP_POWER)
        ),
        action(
            OP_RENICE,
            "Raise the scheduling priority of one of your own processes",
            "Authentication is required to change process priority",
            helper_path(OP_RENICE)
        ),
    )
}

// ── Topology ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TopologyKind {
    AmdX3D,
    IntelHybrid,
    Uniform,
}

#[derive(Debug, Clone)]
pub struct CpuTopology {
    pub kind: TopologyKind,
    pub preferred: HashSet<u32>,
    pub non_preferred: HashSet<u32>,
    pub description: String,
    /// Short human label for preferred cores, e.g. "P-cores (5.5 GHz)" or "V-Cache CCD (96 MB L3)"
    pub preferred_label: String,
    /// Short human label for non-preferred cores, e.g. "E-cores (4.9 GHz)" or "Standard CCD (32 MB L3)"
    pub non_preferred_label: String,
}

impl CpuTopology {
    pub fn uniform(all_cpus: HashSet<u32>) -> Self {
        Self {
            kind: TopologyKind::Uniform,
            preferred: all_cpus,
            non_preferred: HashSet::new(),
            description: "Uniform topology (no asymmetry detected). All CPUs equal.".into(),
            preferred_label: String::new(),
            non_preferred_label: String::new(),
        }
    }

    pub fn has_asymmetry(&self) -> bool {
        !self.non_preferred.is_empty()
    }

    /// Button label for preferred cores, e.g. "P-cores 0-7 (5.5 GHz)"
    pub fn preferred_button_label(&self) -> String {
        format!(
            "{} ({})",
            self.preferred_label,
            cpuset_to_cpulist(&self.preferred)
        )
    }

    /// Button label for non-preferred cores, e.g. "E-cores 8-23 (4.9 GHz)"
    pub fn non_preferred_button_label(&self) -> String {
        format!(
            "{} ({})",
            self.non_preferred_label,
            cpuset_to_cpulist(&self.non_preferred)
        )
    }

    /// Short kind label for display
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            TopologyKind::AmdX3D => "AMD X3D",
            TopologyKind::IntelHybrid => "Intel Hybrid",
            TopologyKind::Uniform => "Symmetric",
        }
    }
}

// ── Topology cache ────────────────────────────────────────────────────────────
// Once we detect an asymmetric topology, we preserve it even after Gaming Mode
// parks one CCD (making sysfs entries for those CPUs unreadable).

static TOPO_CACHE: Mutex<Option<CpuTopology>> = Mutex::new(None);

// ── Detection ─────────────────────────────────────────────────────────────────

/// Auto-detect CPU topology. Tries AMD X3D first, then Intel Hybrid.
/// Caches asymmetric results so topology survives CPU parking.
pub fn detect_topology() -> CpuTopology {
    if let Some(topo) = detect_amd_x3d() {
        if topo.has_asymmetry() {
            *TOPO_CACHE.lock().unwrap() = Some(topo.clone());
            return topo;
        }
    }
    if let Some(topo) = detect_intel_hybrid() {
        if topo.has_asymmetry() {
            *TOPO_CACHE.lock().unwrap() = Some(topo.clone());
            return topo;
        }
    }
    // If live detection is UNIFORM but we have a cached asymmetric result
    // (e.g. Gaming Mode already parked one CCD), return the cache.
    if let Some(cached) = TOPO_CACHE.lock().unwrap().clone() {
        if cached.has_asymmetry() {
            return cached;
        }
    }
    let all = present_cpus();
    CpuTopology::uniform(all)
}

fn present_cpus() -> HashSet<u32> {
    read_cpulist_file("/sys/devices/system/cpu/present").unwrap_or_else(|| {
        let n = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        (0..n).collect()
    })
}

/// Detect AMD X3D: preferred CCD has larger L3 (3D V-Cache).
fn detect_amd_x3d() -> Option<CpuTopology> {
    let present = present_cpus();
    let offline = get_offline_cpus();

    // Read L3 cache sizes for all present CPUs
    let mut l3: HashMap<u32, u64> = HashMap::new();
    for cpu in &present {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/cache/index3/size");
        if let Ok(raw) = fs::read_to_string(&path) {
            let raw = raw.trim();
            // One unparsable size file must skip that CPU, not abort the whole
            // detection (a `?` here degraded real X3D machines to Uniform).
            let kb: Option<u64> = if let Some(s) = raw.strip_suffix('K') {
                s.parse().ok()
            } else if let Some(s) = raw.strip_suffix('M') {
                s.parse::<u64>().ok().map(|mb| mb * 1024)
            } else {
                raw.parse().ok()
            };
            let Some(kb) = kb else { continue };
            l3.insert(*cpu, kb);
        }
        // offline CPUs have no sysfs entry — silently skip
    }

    if l3.is_empty() {
        return None;
    }

    let sizes: HashSet<u64> = l3.values().copied().collect();
    if sizes.len() <= 1 {
        // All readable CPUs have the same L3.
        // Only interpret offline CPUs as "the other CCD is parked" when a
        // previous detection actually saw an X3D topology — otherwise any
        // machine with a manually offlined core would be misdetected as X3D.
        let cached_is_x3d = TOPO_CACHE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|t| t.kind == TopologyKind::AmdX3D);
        if !offline.is_empty() && cached_is_x3d {
            let online_kb = *sizes.iter().next().unwrap();
            let online_set: HashSet<u32> = l3.keys().copied().collect();
            return Some(CpuTopology {
                kind: TopologyKind::AmdX3D,
                preferred: online_set.clone(),
                non_preferred: offline.clone(),
                description: format!(
                    "AMD X3D detected (other CCD currently parked). \
                     Preferred (V-Cache, {}MB L3): CPUs {}. Non-preferred (parked): CPUs {}.",
                    online_kb / 1024,
                    cpuset_to_cpulist(&online_set),
                    cpuset_to_cpulist(&offline),
                ),
                preferred_label: format!("V-Cache CCD ({} MB L3)", online_kb / 1024),
                non_preferred_label: "Standard CCD (parked)".into(),
            });
        }
        return None; // genuine uniform L3
    }

    let max_kb = *sizes.iter().max().unwrap();
    let min_kb = *sizes.iter().min().unwrap();
    let preferred: HashSet<u32> = l3
        .iter()
        .filter(|(_, &s)| s == max_kb)
        .map(|(&c, _)| c)
        .collect();
    let non_preferred: HashSet<u32> = l3
        .iter()
        .filter(|(_, &s)| s == min_kb)
        .map(|(&c, _)| c)
        .collect();

    Some(CpuTopology {
        kind: TopologyKind::AmdX3D,
        preferred: preferred.clone(),
        non_preferred: non_preferred.clone(),
        description: format!(
            "AMD X3D detected. Preferred (V-Cache, {}MB L3): CPUs {}. Non-preferred ({}MB L3): CPUs {}.",
            max_kb / 1024,
            cpuset_to_cpulist(&preferred),
            min_kb / 1024,
            cpuset_to_cpulist(&non_preferred),
        ),
        preferred_label: format!("V-Cache CCD ({} MB L3)", max_kb / 1024),
        non_preferred_label: format!("Standard CCD ({} MB L3)", min_kb / 1024),
    })
}

/// Detect Intel Hybrid: P-cores vs E-cores.
/// Primary: kernel sysfs cpu_core/cpu_atom (reliable, available since Linux 5.18+).
/// Fallback: frequency-based classification using the midpoint between max and min freq.
fn detect_intel_hybrid() -> Option<CpuTopology> {
    // ── Primary: kernel cpu_core / cpu_atom classification ────────────────
    let p_cores = read_cpulist_file("/sys/devices/cpu_core/cpus");
    let e_cores = read_cpulist_file("/sys/devices/cpu_atom/cpus");
    if let (Some(p), Some(e)) = (p_cores, e_cores) {
        if !p.is_empty() && !e.is_empty() {
            // Read max freq for labels (best-effort)
            let p_max = p
                .iter()
                .filter_map(|&c| {
                    fs::read_to_string(format!(
                        "/sys/devices/system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"
                    ))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                })
                .max()
                .unwrap_or(0);
            let e_max = e
                .iter()
                .filter_map(|&c| {
                    fs::read_to_string(format!(
                        "/sys/devices/system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"
                    ))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                })
                .max()
                .unwrap_or(0);

            return Some(CpuTopology {
                kind: TopologyKind::IntelHybrid,
                preferred: p.clone(),
                non_preferred: e.clone(),
                description: format!(
                    "Intel Hybrid detected. P-cores ({:.1} GHz max): CPUs {}. E-cores ({:.1} GHz max): CPUs {}.",
                    p_max as f64 / 1_000_000.0,
                    cpuset_to_cpulist(&p),
                    e_max as f64 / 1_000_000.0,
                    cpuset_to_cpulist(&e),
                ),
                preferred_label: format!("P-cores ({:.1} GHz)", p_max as f64 / 1_000_000.0),
                non_preferred_label: format!("E-cores ({:.1} GHz)", e_max as f64 / 1_000_000.0),
            });
        }
    }

    // ── Fallback: frequency-based detection ──────────────────────────────
    let present = present_cpus();
    let mut max_freq: HashMap<u32, u64> = HashMap::new();

    for cpu in &present {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq");
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(f) = raw.trim().parse::<u64>() {
                max_freq.insert(*cpu, f);
            }
        }
    }

    if max_freq.is_empty() {
        return None;
    }

    let freqs: HashSet<u64> = max_freq.values().copied().collect();
    if freqs.len() <= 1 {
        return None; // uniform max freq
    }

    let max_f = *freqs.iter().max().unwrap();
    let min_f = *freqs.iter().min().unwrap();
    // Use midpoint between highest and lowest freq as threshold —
    // much more robust than 80% of max for close P/E freq gaps.
    let threshold = (max_f + min_f) / 2;
    let preferred: HashSet<u32> = max_freq
        .iter()
        .filter(|(_, &f)| f >= threshold)
        .map(|(&c, _)| c)
        .collect();
    let non_preferred: HashSet<u32> = max_freq
        .iter()
        .filter(|(_, &f)| f < threshold)
        .map(|(&c, _)| c)
        .collect();

    Some(CpuTopology {
        kind: TopologyKind::IntelHybrid,
        preferred: preferred.clone(),
        non_preferred: non_preferred.clone(),
        description: format!(
            "Intel Hybrid detected. P-cores ({:.1} GHz max): CPUs {}. E-cores ({:.1} GHz max): CPUs {}.",
            max_f as f64 / 1_000_000.0,
            cpuset_to_cpulist(&preferred),
            min_f as f64 / 1_000_000.0,
            cpuset_to_cpulist(&non_preferred),
        ),
        preferred_label: format!("P-cores ({:.1} GHz)", max_f as f64 / 1_000_000.0),
        non_preferred_label: format!("E-cores ({:.1} GHz)", min_f as f64 / 1_000_000.0),
    })
}

// ── SMT sibling detection ─────────────────────────────────────────────────────

/// Return the SMT sibling threads within a set of CPUs.
/// For each physical core with 2+ logical CPUs, all but the lowest-numbered are siblings.
pub fn get_smt_siblings_of(cpus: &HashSet<u32>) -> HashSet<u32> {
    let mut core_to_logical: HashMap<u32, Vec<u32>> = HashMap::new();
    for &cpu in cpus {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/core_id");
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(core_id) = raw.trim().parse::<u32>() {
                core_to_logical.entry(core_id).or_default().push(cpu);
            }
        }
    }
    let mut siblings = HashSet::new();
    for mut logical_cpus in core_to_logical.into_values() {
        if logical_cpus.len() >= 2 {
            logical_cpus.sort_unstable();
            let primary = logical_cpus[0];
            for &c in &logical_cpus[1..] {
                if c != primary {
                    siblings.insert(c);
                }
            }
        }
    }
    siblings
}

// ── Helper check ─────────────────────────────────────────────────────────────

pub fn is_helper_installed() -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: String| {
        fs::metadata(&p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    executable(helper_path(OP_PARK))
        && executable(helper_path(OP_POWER))
        && executable(helper_path(OP_RENICE))
        && std::path::Path::new(POLICY_PATH).exists()
}

pub fn is_helper_current() -> bool {
    is_helper_installed()
        && fs::read_to_string(helper_path(OP_PARK))
            .map(|s| s.contains(HELPER_VERSION))
            .unwrap_or(false)
}

/// True while the superseded single-helper-plus-sudoers install is still on
/// disk. Surfaced so the user is told the blanket NOPASSWD rule is gone
/// rather than left wondering why a file they remember disappeared.
pub fn legacy_install_present() -> bool {
    std::path::Path::new(LEGACY_SUDOERS).exists() || std::path::Path::new(LEGACY_HELPER).exists()
}

/// Whether polkit will currently authorise us, asked without prompting.
///
/// `pkcheck` without `--allow-user-interaction` answers from policy alone, so
/// this never raises a dialog as a side effect of drawing the tab. If pkcheck
/// is missing we fall back to "the files are installed", which is the best
/// that can be said without asking.
pub fn is_helper_authorized() -> bool {
    if !is_helper_installed() {
        return false;
    }
    let pid = std::process::id().to_string();
    match Command::new("pkcheck")
        .args([
            "--action-id",
            "io.github.franzjeger.argus-lasso.cpu-park",
            "--process",
            &pid,
        ])
        .output()
    {
        Ok(o) => o.status.success(),
        Err(_) => true,
    }
}

// ── Park / Unpark ─────────────────────────────────────────────────────────────

/// Run one privileged operation through pkexec.
fn run_helper(op: &str, args: &[&str]) -> (bool, String) {
    if !is_helper_installed() {
        return (false, "Helper not installed. Run install first.".into());
    }
    let mut cmd = Command::new("pkexec");
    cmd.arg(helper_path(op));
    for a in args {
        cmd.arg(a);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => (true, String::new()),
        // 126/127 are pkexec's own codes for "dismissed" and "not authorised",
        // distinct from anything the helper scripts return.
        Ok(o) if matches!(o.status.code(), Some(126) | Some(127)) => (
            false,
            "Not authorised by polkit (dialog dismissed, or no polkit agent running).".into(),
        ),
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (
                false,
                if msg.is_empty() {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                } else {
                    msg
                },
            )
        }
        Err(e) => (false, e.to_string()),
    }
}

/// Take CPUs offline. Returns true if all succeeded.
pub fn park_cpus(cpus: &HashSet<u32>, log_cb: impl Fn(String)) -> bool {
    if cpus.is_empty() {
        return true;
    }
    let mut ok = true;
    let mut sorted: Vec<u32> = cpus.iter().copied().collect();
    sorted.sort_unstable();
    for cpu in sorted {
        if cpu == 0 {
            log_cb("[Park] Skipping CPU 0 (bootstrap processor, cannot offline)".to_string());
            continue;
        }
        let (success, msg) = run_helper(OP_PARK, &["online", &cpu.to_string(), "0"]);
        if success {
            log_cb(format!("[Park] CPU {cpu} → offline"));
        } else {
            log::warn!("park cpu{cpu} failed: {msg}");
            log_cb(format!("[Park] CPU {cpu} FAILED: {msg}"));
            ok = false;
        }
    }
    ok
}

/// Bring all offline CPUs back online.
pub fn unpark_all(log_cb: impl Fn(String)) -> bool {
    let offline = get_offline_cpus();
    if offline.is_empty() {
        log_cb("[Park] No offline CPUs to restore.".into());
        return true;
    }
    let (success, msg) = run_helper(OP_PARK, &["unpark-all"]);
    if success {
        log_cb(format!("[Park] CPUs {:?} restored online.", {
            let mut v: Vec<u32> = offline.iter().copied().collect();
            v.sort_unstable();
            v
        }));
        true
    } else {
        log::warn!("unpark-all failed: {msg}");
        log_cb(format!("[Park] Unpark all FAILED: {msg}"));
        false
    }
}

/// Raise a process's scheduling priority. The helper refuses any PID the
/// calling user does not own, so this can only ever affect our own processes.
pub fn set_process_nice_via_helper(pid: u32, nice: i32) -> bool {
    let (ok, msg) = run_helper(OP_RENICE, &[&nice.to_string(), &pid.to_string()]);
    if !ok {
        log::warn!("renice pid={pid} nice={nice} failed: {msg}");
    }
    ok
}

// ── Helper installation ───────────────────────────────────────────────────────

/// True if polkit's pkexec is available. Without it the helpers cannot be
/// authorised at all, so installing them would leave dead files on disk.
pub fn is_pkexec_available() -> bool {
    Command::new("which")
        .arg("pkexec")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Allowlist a path for unquoted-adjacent interpolation into a shell command
/// that `pkexec` runs as root. Every caller still single-quotes the value on
/// top of this — belt and suspenders, since either one alone has a history
/// of missed edge cases in shell-command construction.
fn validate_shell_safe_path(s: &str) -> Result<&str, String> {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    {
        Ok(s)
    } else {
        Err("Staging path contains unsafe characters.".into())
    }
}

/// Stage the helper scripts and the polkit policy in the user's own config
/// directory, and build the root command that installs them.
///
/// Staging in a private directory rather than a fixed /tmp path matters: a
/// world-writable staging path could be swapped between our write and root's
/// copy, which is a straight local-root escalation.
fn stage_install() -> Result<(String, std::path::PathBuf), String> {
    let stage = crate::config::config_dir().join("helper-stage");
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).map_err(|e| format!("Failed to create staging dir: {e}"))?;

    // `dir` is interpolated into a string that pkexec runs via `sh -c`, so it
    // is as good as attacker input: it comes from $HOME (config_dir()), which
    // isn't necessarily trustworthy just because we're running as our own
    // uid. An allowlist (rather than blocking a few known-bad characters) and
    // single-quoting every use below are both required — either alone has a
    // history of missed edge cases in shell-command construction.
    let dir = match stage.to_str() {
        Some(s) => validate_shell_safe_path(s)?.to_string(),
        None => return Err("Staging path is not valid UTF-8.".into()),
    };

    for (name, body) in [
        (OP_PARK, PARK_SCRIPT),
        (OP_POWER, POWER_SCRIPT),
        (OP_RENICE, RENICE_SCRIPT),
    ] {
        fs::write(stage.join(name), body).map_err(|e| format!("Failed to stage {name}: {e}"))?;
    }
    fs::write(stage.join("policy.xml"), policy_xml())
        .map_err(|e| format!("Failed to stage the polkit policy: {e}"))?;

    // Removing the predecessor is part of the install, not a separate step:
    // leaving the old NOPASSWD sudoers rule in place would keep the very hole
    // this replaces open.
    //
    // Every path is single-quoted, including the compile-time HELPER_DIR/
    // POLICY_PATH constants: those are trusted, but quoting them too is free
    // and keeps this command safe even if a packager ever bakes in a path
    // containing a space.
    let cmd = format!(
        "set -e && \
         install -d -m 755 -o root -g root '{HELPER_DIR}' && \
         install -m 755 -o root -g root '{dir}/{OP_PARK}' '{HELPER_DIR}/{OP_PARK}' && \
         install -m 755 -o root -g root '{dir}/{OP_POWER}' '{HELPER_DIR}/{OP_POWER}' && \
         install -m 755 -o root -g root '{dir}/{OP_RENICE}' '{HELPER_DIR}/{OP_RENICE}' && \
         install -D -m 644 -o root -g root '{dir}/policy.xml' '{POLICY_PATH}' && \
         rm -f '{LEGACY_SUDOERS}' '{LEGACY_HELPER}' && \
         echo INSTALL_OK"
    );
    Ok((cmd, stage))
}

fn install_outcome(o: &std::process::Output) -> (bool, String) {
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    let combined = format!("{out}{err}");
    if combined.contains("INSTALL_OK") {
        (
            true,
            "Helpers and polkit policy installed; the old sudoers rule was removed.".into(),
        )
    } else {
        let tail: String = {
            let t: Vec<char> = combined.trim().chars().collect();
            t[t.len().saturating_sub(300)..].iter().collect()
        };
        (
            false,
            format!("Install failed (rc={:?}): {tail}", o.status.code()),
        )
    }
}

/// Install the helpers and the polkit policy. Authentication is handled by
/// the desktop's polkit agent — no password passes through this process.
///
/// There is deliberately no root-password fallback any more. The helpers are
/// authorised by polkit, so on a system without it they would be installed
/// and then permanently unusable.
pub fn install_helper_via_pkexec() -> (bool, String) {
    if !is_pkexec_available() {
        return (
            false,
            "pkexec was not found. Argus-Lasso authorises its privileged \
             helpers through polkit, so install polkit first."
                .into(),
        );
    }
    let (cmd, stage) = match stage_install() {
        Ok(v) => v,
        Err(e) => return (false, e),
    };
    let result = Command::new("pkexec")
        .args(["/bin/sh", "-c", &cmd])
        .output();
    let _ = fs::remove_dir_all(&stage);
    match result {
        Ok(o) if matches!(o.status.code(), Some(126) | Some(127)) => {
            (false, "Authentication cancelled or failed.".into())
        }
        Ok(o) => install_outcome(&o),
        Err(e) => (false, format!("pkexec spawn failed: {e}")),
    }
}

// ── Power profiles (governor + EPP via helper) ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PowerProfile {
    Performance,
    Balanced,
    PowerSave,
}

impl PowerProfile {
    pub fn label(&self) -> &'static str {
        match self {
            PowerProfile::Performance => "Performance",
            PowerProfile::Balanced => "Balanced",
            PowerProfile::PowerSave => "Power Save",
        }
    }

    /// EPP value for this profile (only meaningful on EPP-capable drivers).
    fn epp(&self) -> &'static str {
        match self {
            PowerProfile::Performance => "performance",
            PowerProfile::Balanced => "balance_performance",
            PowerProfile::PowerSave => "power",
        }
    }

    /// Pick the governor for this profile based on what the platform offers.
    ///
    /// With an EPP-capable driver (intel_pstate / amd-pstate in active mode),
    /// "powersave" means "EPP-controlled" and is the right base for both
    /// Balanced and Power Save. WITHOUT EPP (acpi-cpufreq, amd-pstate
    /// passive/guided, cpufreq-dt), the static "powersave" governor pins every
    /// core to its minimum frequency — so Balanced must use a scaling governor
    /// (schedutil/ondemand/conservative) instead.
    fn pick_governor(&self, epp_supported: bool, available: &[String]) -> Option<String> {
        let first_of = |cands: &[&str]| {
            cands
                .iter()
                .find(|g| available.iter().any(|a| a == *g))
                .map(|g| g.to_string())
        };
        match self {
            PowerProfile::Performance => first_of(&["performance"]),
            PowerProfile::Balanced => {
                if epp_supported {
                    first_of(&["powersave"])
                } else {
                    first_of(&["schedutil", "ondemand", "conservative"])
                }
            }
            // Static "powersave" (min frequency) is acceptable semantics for
            // Power Save even without EPP.
            PowerProfile::PowerSave => first_of(&["powersave", "conservative", "schedutil"]),
        }
    }
}

fn available_governors() -> Vec<String> {
    fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors")
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Apply a power profile via the privileged helper. Returns (ok, message).
pub fn apply_power_profile(profile: PowerProfile) -> (bool, String) {
    // The helper's cpu-epp glob writes are best-effort and always exit 0, so
    // detect EPP support from sysfs instead of trusting the helper.
    let epp_supported = current_epp().is_some();
    let available = available_governors();
    let Some(governor) = profile.pick_governor(epp_supported, &available) else {
        return (
            false,
            format!(
                "[Power] no suitable governor for {} (available: {})",
                profile.label(),
                available.join(" ")
            ),
        );
    };
    let (gov_ok, gov_msg) = run_helper(OP_POWER, &["governor", &governor]);
    if !gov_ok {
        return (false, format!("[Power] governor change failed: {gov_msg}"));
    }
    let epp_note = if epp_supported {
        let epp = profile.epp();
        let (epp_ok, epp_msg) = run_helper(OP_POWER, &["epp", epp]);
        if epp_ok {
            format!(", EPP={epp}")
        } else {
            format!(", EPP={epp} (failed: {epp_msg})")
        }
    } else {
        String::new()
    };
    (
        true,
        format!(
            "[Power] {} — governor={governor}{epp_note}",
            profile.label()
        ),
    )
}

/// Read the current scaling governor of cpu0 (representative for display).
pub fn current_governor() -> Option<String> {
    fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read the current EPP of cpu0, if the platform exposes it.
pub fn current_epp() -> Option<String> {
    fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Set the scaling governor on every CPU through the privileged helper.
/// Used as a fallback when a direct sysfs write is refused.
pub fn set_governor_via_helper(governor: &str) -> Result<(), String> {
    match run_helper(OP_POWER, &["governor", governor]) {
        (true, _) => Ok(()),
        (false, msg) => Err(msg),
    }
}

/// Set the energy performance preference on every CPU through the helper.
pub fn set_epp_via_helper(epp: &str) -> Result<(), String> {
    match run_helper(OP_POWER, &["epp", epp]) {
        (true, _) => Ok(()),
        (false, msg) => Err(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage_script(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("argus-{name}-{}", std::process::id()));
        fs::write(&path, body).unwrap();
        path
    }

    /// Run the script through `bash` rather than exec'ing the file.
    ///
    /// Tests run in parallel, and a sibling that spawns a process between our
    /// open-for-write and close leaves that child holding a writable
    /// descriptor to it — exec'ing the file then fails with ETXTBSY until the
    /// child exits. Handing the path to bash only ever opens it for reading,
    /// so the race cannot happen, and the file needs no exec bit.
    fn run(script: &std::path::Path, args: &[&str], pkexec_uid: Option<&str>) -> i32 {
        let mut cmd = Command::new("bash");
        cmd.arg(script).args(args).env_remove("PKEXEC_UID");
        if let Some(uid) = pkexec_uid {
            cmd.env("PKEXEC_UID", uid);
        }
        cmd.output().unwrap().status.code().unwrap_or(-1)
    }

    /// The whole point of splitting the helper: renice must not be reachable
    /// for a process the caller does not own. Previously one NOPASSWD sudoers
    /// rule let any local process renice PID 1.
    #[test]
    fn renice_refuses_a_process_the_caller_does_not_own() {
        let script = stage_script("renice", RENICE_SCRIPT);
        // PID 1 is root's. Claim to be some other uid and it must be refused.
        let code = run(&script, &["-5", "1"], Some("4242"));
        assert_eq!(code, 1, "renice must refuse a PID owned by another uid");
        fs::remove_file(&script).ok();
    }

    /// Without PKEXEC_UID we cannot know who is asking, so the script must
    /// refuse rather than fall back to trusting the caller.
    #[test]
    fn renice_refuses_when_not_invoked_through_pkexec() {
        let script = stage_script("renice-nopk", RENICE_SCRIPT);
        let code = run(&script, &["-5", "1"], None);
        assert_eq!(code, 2, "renice must refuse outside pkexec");
        fs::remove_file(&script).ok();
    }

    #[test]
    fn renice_rejects_malformed_arguments() {
        let script = stage_script("renice-args", RENICE_SCRIPT);
        for args in [
            vec!["notanumber", "1"],
            vec!["-5", "notapid"],
            vec!["-5"],
            vec![],
        ] {
            assert_eq!(
                run(&script, &args, Some("0")),
                2,
                "expected rejection for {args:?}"
            );
        }
        fs::remove_file(&script).ok();
    }

    #[test]
    fn park_helper_rejects_out_of_range_arguments() {
        let script = stage_script("park", PARK_SCRIPT);
        for args in [
            vec!["online", "abc", "0"],
            vec!["online", "3", "2"],
            vec!["bogus"],
        ] {
            assert_eq!(
                run(&script, &args, Some("0")),
                2,
                "expected rejection for {args:?}"
            );
        }
        fs::remove_file(&script).ok();
    }

    /// A policy that does not name every helper would leave one operation
    /// unauthorised, which surfaces only when a user tries it.
    #[test]
    fn policy_covers_every_helper() {
        let xml = policy_xml();
        for op in [OP_PARK, OP_POWER, OP_RENICE] {
            assert!(
                xml.contains(&format!("io.github.franzjeger.argus-lasso.{op}")),
                "no action for {op}"
            );
            assert!(xml.contains(&helper_path(op)), "no exec.path for {op}");
        }
        // allow_any=yes would hand the operation to remote sessions too.
        assert!(!xml.contains("<allow_any>yes"));
    }

    #[test]
    fn every_helper_carries_the_version_marker() {
        for body in [PARK_SCRIPT, POWER_SCRIPT, RENICE_SCRIPT] {
            assert!(body.contains(HELPER_VERSION), "missing version marker");
        }
    }

    /// stage_install() interpolates this value into a string that pkexec
    /// runs as root via `sh -c`. It comes from $HOME (config_dir()), which
    /// is not inherently trustworthy — each of these must be rejected, not
    /// just the literal single-quote the old blocklist-only check caught.
    #[test]
    fn staging_path_validation_rejects_shell_metacharacters() {
        for bad in [
            "",
            "/home/user;rm -rf /",
            "/home/user$(whoami)",
            "/home/user`whoami`",
            "/home/user|cat /etc/shadow",
            "/home/user&background",
            "/home/user name/with space",
            "/home/user'quote",
            "/home/user\"quote",
            "/home/user\nnewline",
        ] {
            assert!(
                validate_shell_safe_path(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn staging_path_validation_accepts_normal_paths() {
        let good = "/home/user/.config/argus-lasso/helper-stage";
        assert_eq!(validate_shell_safe_path(good), Ok(good));
    }
}
