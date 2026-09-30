//! CPU core parking: take non-preferred CPUs offline via privileged helper.
//!
//! Mirrors Python cpu_park.py:
//!   - detect_topology(): AMD X3D (L3 cache asymmetry), Intel Hybrid (max freq), or UNIFORM
//!   - set_parked_cpus() / unpark_all() via the polkit-authorised park helper
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

/// The polkit rule that lets the installing user run the helpers without a
/// password; see `polkit_rules`.
pub const RULES_PATH: &str = match option_env!("ARGUS_POLKIT_RULES_PATH") {
    Some(path) => path,
    None => "/etc/polkit-1/rules.d/50-argus-lasso.rules",
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
const HELPER_VERSION: &str = "argus-lasso-helper v7";

/// The three privileged operations, and the file each one lives in.
const OP_PARK: &str = "cpu-park";
const OP_POWER: &str = "power-profile";
const OP_RENICE: &str = "renice";

fn helper_path(op: &str) -> String {
    format!("{HELPER_DIR}/{op}")
}

const PARK_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v7 — CPU parking. Managed by argus-lasso; do not edit.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
case "${1-}" in
    set-offline)
        # Make exactly the listed CPUs offline and every other CPU online, in
        # one authorisation: online first, so fewer CPUs are never online
        # than either the old or the new state has.
        shift
        want=" "
        for c in "$@"; do
            [[ "$c" =~ ^[1-9][0-9]*$ ]] || exit 2
            want+="$c "
        done
        rc=0
        for pass in 1 0; do
            for f in /sys/devices/system/cpu/cpu[0-9]*/online; do
                c=${f#/sys/devices/system/cpu/cpu}; c=${c%/online}
                if [[ "$want" == *" $c "* ]]; then v=0; else v=1; fi
                [ "$v" = "$pass" ] || continue
                [ "$(cat "$f" 2>/dev/null)" = "$v" ] && continue
                echo "$v" > "$f" 2>/dev/null || { echo "cpu$c: could not set online=$v" >&2; rc=1; }
            done
        done
        exit $rc
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
        echo "usage: cpu-park set-offline [<cpu>...] | unpark-all" >&2; exit 2 ;;
esac
"#;

const POWER_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v7 — CPU governor and energy preference.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
# Write $2 to cpufreq/$1 on every online CPU. Fails, naming the CPUs and the
# kernel's reason, if it refuses any of them (a governor it lacks, an EPP the
# governor does not allow) or if no CPU has the file. Parked CPUs have no
# policy to set.
set_all() {
    local name=$1 value=$2 dir f err refused="" reason="" ok=0
    for dir in $(printf '%s\n' /sys/devices/system/cpu/cpu[0-9]* | sort -V); do
        f=$dir/cpufreq/$name
        [ -e "$f" ] || continue
        if [ "$(cat "$dir/online" 2>/dev/null || echo 1)" = 0 ]; then
            continue
        fi
        if err=$( { echo "$value" > "$f"; } 2>&1 ); then
            ok=$((ok + 1))
        else
            refused+=" ${dir##*/}"
            reason=${err##*: }
        fi
    done
    if [ -n "$refused" ]; then
        [ "$ok" = 0 ] && refused=" every CPU"
        echo "the kernel refused $name=$value on$refused ($reason)" >&2
        exit 1
    fi
    if [ "$ok" = 0 ]; then
        echo "no CPU has $name" >&2
        exit 1
    fi
}
case "${1-}" in
    governor)
        [[ "${2-}" =~ ^[a-z_-]+$ ]] || exit 2
        set_all scaling_governor "$2"
        ;;
    epp)
        [[ "${2-}" =~ ^[a-z_-]+$ ]] || exit 2
        set_all energy_performance_preference "$2"
        ;;
    *)
        echo "usage: power-profile governor <name> | epp <name>" >&2; exit 2 ;;
esac
"#;

/// Negative nice needs CAP_SYS_NICE, which is why this runs privileged — but
/// it must never reach a process the caller does not own. pkexec exports
/// PKEXEC_UID; without it we are not being invoked through polkit and refuse
/// rather than guess who is asking.
///
/// The caller also passes the pid's start_ticks (/proc/<pid>/stat field 22,
/// read at the moment it decided to renice this pid) so the script can
/// re-check *identity*, not just ownership, immediately before acting: a pid
/// can be reused — even by a process the same uid owns — in the time between
/// the caller observing it and this script running. Two processes can never
/// share a start time, so comparing it catches reuse an ownership check
/// alone would miss. This narrows the race to the few lines between the
/// re-check and the renice call; Linux has no pidfd-based setpriority to
/// close it entirely.
const RENICE_SCRIPT: &str = r#"#!/bin/bash
# argus-lasso-helper v7 — renice, restricted to the caller's own processes.
set -euo pipefail
export PATH="/usr/sbin:/usr/bin:/sbin:/bin"
[[ "${1-}" =~ ^-?[0-9]+$ ]] || exit 2
[[ "${2-}" =~ ^[0-9]+$   ]] || exit 2
[[ "${3-}" =~ ^[0-9]+$   ]] || exit 2
nice_val=$1
pid=$2
want_start=$3
if [ -z "${PKEXEC_UID-}" ]; then
    echo "refusing: not invoked through pkexec" >&2
    exit 2
fi
owner=$(stat -c %u "/proc/$pid" 2>/dev/null) || { echo "no such process: $pid" >&2; exit 1; }
if [ "$owner" != "$PKEXEC_UID" ]; then
    echo "refusing: PID $pid belongs to uid $owner, not $PKEXEC_UID" >&2
    exit 1
fi
# /proc/<pid>/stat is "pid (comm) state ppid ... starttime ...": strip up to
# the last ") " (comm may itself contain spaces or parens) and starttime is
# the 20th field after that split (state=0, ppid=1, ..., starttime=19).
stat_line=$(cat "/proc/$pid/stat" 2>/dev/null) || { echo "no such process: $pid" >&2; exit 1; }
after_comm=${stat_line##*) }
read -r -a fields <<< "$after_comm"
actual_start=${fields[19]-}
if [ "$actual_start" != "$want_start" ]; then
    echo "refusing: PID $pid is not the process we were asked to renice (start time changed)" >&2
    exit 1
fi
renice -n "$nice_val" -p "$pid" >/dev/null
# Nice is per thread on Linux: `renice -p` moves only the thread whose TID is
# given. The main thread above decides success; the rest are best effort,
# since a thread may exit between the listing and its renice.
for task in "/proc/$pid/task/"*; do
    tid=${task##*/}
    [ "$tid" = "$pid" ] || renice -n "$nice_val" -p "$tid" >/dev/null 2>&1 || true
done
"#;

/// The polkit rule, with `@USER@` for the user who installs the helpers: that
/// user, in an active local session, runs them without a password.
fn polkit_rules() -> String {
    format!(
        r#"// Managed by argus-lasso ({HELPER_VERSION}); reinstalled with the helpers.
// The user who installed Argus-Lasso's CPU control helpers runs them without
// a password from an active local session. Anyone else is asked for an
// administrator's password (the actions' default).
polkit.addRule(function(action, subject) {{
    if (action.id.indexOf("io.github.franzjeger.argus-lasso.") === 0 &&
        subject.user === "@USER@" && subject.local && subject.active) {{
        return polkit.Result.YES;
    }}
}});
"#
    )
}

/// Root-side shell writing the polkit rule for the user pkexec was called
/// by (PKEXEC_UID, which pkexec sets and the caller cannot) from the
/// verified template in "$t".
fn rules_script() -> String {
    String::from(
        "user=$(id -nu \"${PKEXEC_UID:?not run through pkexec}\")\n\
         case \"$user\" in ''|*[!a-z0-9_.-]*) echo \"unusable user name: $user\" >&2; exit 1;; esac\n\
         sed \"s/@USER@/$user/\" \"$t/rules.in\" > \"$t/rules\"\n",
    )
}

/// Three separate actions so an administrator can tighten one without losing
/// the others. Each asks for an administrator's password by default;
/// `polkit_rules` lets the user who installed the helpers run them without
/// one, from an active local session. (`allow_active=yes` let every local
/// user do that.) Renice is confined to the caller's own processes by the
/// helper itself.
fn policy_xml() -> String {
    let action = |id: &str, desc: &str, msg: &str, path: String| {
        format!(
            r#"  <action id="io.github.franzjeger.argus-lasso.{id}">
    <description>{desc}</description>
    <message>{msg}</message>
    <defaults>
      <allow_any>no</allow_any>
      <allow_inactive>no</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
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

/// Make exactly `cpus` the parked (offline) CPUs, bringing every other CPU
/// back online, in one helper call. CPU 0 cannot be taken offline and is
/// skipped. Returns true if every CPU reached its state.
pub fn set_parked_cpus(cpus: &HashSet<u32>, log_cb: impl Fn(String)) -> bool {
    if cpus.contains(&0) {
        log_cb("[Park] Skipping CPU 0 (bootstrap processor, cannot offline)".to_string());
    }
    let mut sorted: Vec<u32> = cpus.iter().copied().filter(|&cpu| cpu != 0).collect();
    sorted.sort_unstable();
    let args: Vec<String> = std::iter::once("set-offline".to_string())
        .chain(sorted.iter().map(u32::to_string))
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (success, msg) = run_helper(OP_PARK, &args);
    if success {
        log_cb(format!(
            "[Park] Offline: {sorted:?}; every other CPU online."
        ));
    } else {
        log::warn!("set-offline {sorted:?} failed: {msg}");
        log_cb(format!("[Park] Parking {sorted:?} FAILED: {msg}"));
    }
    success
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
///
/// `start_ticks` is the pid's start time (ProcInfo::start_ticks /
/// /proc/<pid>/stat field 22) as the caller last observed it — passed
/// through so the helper can refuse if the pid has since been reused by a
/// different process.
pub fn set_process_nice_via_helper(pid: u32, start_ticks: u64, nice: i32) -> bool {
    let (ok, msg) = run_helper(
        OP_RENICE,
        &[
            &nice.to_string(),
            &pid.to_string(),
            &start_ticks.to_string(),
        ],
    );
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

/// Ends each file embedded in the root command; no helper may contain it.
const EMBED_END: &str = "ARGUS_LASSO_HELPER_FILE_END";

/// Printed by the root command when an embedded file does not match its
/// digest. Matched in `install_outcome`.
const EMBEDDED_FILES_DAMAGED: &str = "EMBEDDED_FILES_DAMAGED";

/// Root-side shell that writes each file into a private `mktemp -d`
/// directory from a quoted here-document in the command itself, and checks
/// the result against its SHA-256 digest. On success `$t` holds the files,
/// and only those are installed.
///
/// Root reads nothing the user can touch: the command is fixed once pkexec
/// runs it. It used to copy the files out of a staging directory in the
/// user's config, which any of the user's processes (a Wine game included)
/// could replace while the polkit prompt was open, with a FIFO that hung
/// root or a device whose open() has effects.
fn embed_and_verify_script(files: &[(&str, String)]) -> String {
    let mut script = String::from("t=$(mktemp -d)\ntrap 'rm -rf \"$t\"' EXIT\n");
    for (name, body) in files {
        script += &format!("cat > \"$t/{name}\" <<'{EMBED_END}'\n{body}{EMBED_END}\n");
    }
    script += "if ! (cd \"$t\" && printf '%s\\n'";
    for (name, body) in files {
        let digest = crate::updater::sha256_hex(body.as_bytes());
        script += &format!(" '{digest}  {name}'");
    }
    script += &format!(
        " | sha256sum -c >/dev/null 2>&1); then\n    echo {EMBEDDED_FILES_DAMAGED} >&2\n    exit 1\nfi\n"
    );
    script
}

/// The helper scripts and the polkit policy, each ending in a newline as a
/// here-document reproduces it.
fn helper_files() -> Vec<(&'static str, String)> {
    [
        (OP_PARK, PARK_SCRIPT.to_string()),
        (OP_POWER, POWER_SCRIPT.to_string()),
        (OP_RENICE, RENICE_SCRIPT.to_string()),
        ("policy.xml", policy_xml()),
        ("rules.in", polkit_rules()),
    ]
    .into_iter()
    .map(|(name, mut body)| {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        (name, body)
    })
    .collect()
}

/// The command pkexec runs as root to install `files`.
fn root_install_command(files: &[(&str, String)]) -> String {
    // Removing the predecessor is part of the install, not a separate step:
    // leaving the old NOPASSWD sudoers rule in place would keep the very hole
    // this replaces open.
    //
    // Every path is single-quoted, including the compile-time HELPER_DIR/
    // POLICY_PATH constants: those are trusted, but quoting them too is free
    // and keeps this command safe even if a packager ever bakes in a path
    // containing a space.
    format!(
        "set -e\n{verify}{rules}\
         install -d -m 755 -o root -g root '{HELPER_DIR}'\n\
         install -m 755 -o root -g root \"$t/{OP_PARK}\" '{HELPER_DIR}/{OP_PARK}'\n\
         install -m 755 -o root -g root \"$t/{OP_POWER}\" '{HELPER_DIR}/{OP_POWER}'\n\
         install -m 755 -o root -g root \"$t/{OP_RENICE}\" '{HELPER_DIR}/{OP_RENICE}'\n\
         install -D -m 644 -o root -g root \"$t/policy.xml\" '{POLICY_PATH}'\n\
         install -D -m 644 -o root -g root \"$t/rules\" '{RULES_PATH}'\n\
         rm -f '{LEGACY_SUDOERS}' '{LEGACY_HELPER}'\n\
         echo INSTALL_OK\n",
        verify = embed_and_verify_script(files),
        rules = rules_script(),
    )
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
    } else if combined.contains(EMBEDDED_FILES_DAMAGED) {
        (
            false,
            "Install aborted: the helper files did not arrive intact. Nothing was installed."
                .into(),
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
    // Left by versions that staged the files in the user's config.
    let _ = fs::remove_dir_all(crate::config::config_dir().join("helper-stage"));
    let cmd = root_install_command(&helper_files());
    let result = Command::new("pkexec")
        .args(["/bin/sh", "-c", &cmd])
        .output();
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
    // Whether to set EPP at all: platforms without it have no such file.
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
        // PID 1 is root's. Claim to be some other uid and it must be refused
        // — before the start_ticks re-check is even reached, so any
        // well-formed placeholder value works here.
        let code = run(&script, &["-5", "1", "0"], Some("4242"));
        assert_eq!(code, 1, "renice must refuse a PID owned by another uid");
        fs::remove_file(&script).ok();
    }

    /// Without PKEXEC_UID we cannot know who is asking, so the script must
    /// refuse rather than fall back to trusting the caller.
    #[test]
    fn renice_refuses_when_not_invoked_through_pkexec() {
        let script = stage_script("renice-nopk", RENICE_SCRIPT);
        let code = run(&script, &["-5", "1", "0"], None);
        assert_eq!(code, 2, "renice must refuse outside pkexec");
        fs::remove_file(&script).ok();
    }

    /// The v5 helper ignored every refused write and exited 0, so Settings
    /// reported a governor or EPP change the kernel never made.
    #[test]
    fn power_helper_reports_writes_the_kernel_refuses() {
        // As root the write might succeed; this is about refusal.
        // SAFETY: getuid(2) cannot fail and takes no arguments.
        if unsafe { nix::libc::getuid() } == 0 {
            return;
        }
        let script = stage_script("power-refused", POWER_SCRIPT);
        let out = Command::new("bash")
            .arg(&script)
            .args(["governor", "performance"])
            .output()
            .unwrap();
        fs::remove_file(&script).ok();
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.trim() == "the kernel refused scaling_governor=performance on every CPU (Permission denied)"
                || stderr.trim() == "no CPU has scaling_governor",
            "{stderr}"
        );
    }

    #[test]
    fn renice_rejects_malformed_arguments() {
        let script = stage_script("renice-args", RENICE_SCRIPT);
        for args in [
            vec!["notanumber", "1", "0"],
            vec!["-5", "notapid", "0"],
            vec!["-5", "1", "notanumber"],
            vec!["-5", "1"],
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

    /// The whole point of adding start_ticks: even with the right owner, a
    /// pid that has since been reused by a different process instance must
    /// be refused, not just any pid the caller happens to still own.
    #[test]
    fn renice_refuses_when_start_ticks_does_not_match() {
        use std::os::unix::fs::MetadataExt;
        let script = stage_script("renice-start-mismatch", RENICE_SCRIPT);
        let my_pid = std::process::id();
        let my_uid = fs::metadata("/proc/self").unwrap().uid();
        // Real start_ticks are never 0 for anything started after boot, so
        // this can never accidentally match our real one.
        let code = run(
            &script,
            &["0", &my_pid.to_string(), "0"],
            Some(&my_uid.to_string()),
        );
        assert_eq!(
            code, 1,
            "renice must refuse when start_ticks does not match the live process"
        );
        fs::remove_file(&script).ok();
    }

    /// Positive-path check for the same mechanism: our own real pid, uid and
    /// start_ticks must still be accepted. Also cross-checks the script's
    /// hand-rolled /proc/<pid>/stat field parsing against fast_proc's.
    #[test]
    fn renice_succeeds_when_start_ticks_matches() {
        // This test actually renices the test binary's own process (unlike
        // its sibling above, which is refused before reaching renice) — see
        // PROCESS_NICE_TEST_LOCK's doc comment for why that needs
        // serializing against other tests doing the same (rules.rs's
        // rules_sharing_a_nice_target_change_it_once).
        let _guard = crate::utils::PROCESS_NICE_TEST_LOCK.lock().unwrap();
        use std::os::unix::fs::MetadataExt;
        let script = stage_script("renice-start-match", RENICE_SCRIPT);
        let my_pid = std::process::id();
        let my_uid = fs::metadata("/proc/self").unwrap().uid();
        let real_start = crate::fast_proc::read_stat(my_pid, &mut [0u8; 1024])
            .expect("read our own /proc/self/stat")
            .starttime;
        // Renice to whatever our nice value already is: a genuine no-op
        // regardless of what any other test has done to it, so this never
        // needs to raise (requires CAP_SYS_NICE) or lower (governed by
        // RLIMIT_NICE, and not guaranteed even back to a value this same
        // process held a moment ago) our own priority — see
        // PROCESS_NICE_TEST_LOCK's doc comment and rules.rs's staleness
        // regression test for why a hardcoded target bit us here before.
        let current_nice = crate::utils::get_nice(my_pid).unwrap_or(0);
        let code = run(
            &script,
            &[
                &current_nice.to_string(),
                &my_pid.to_string(),
                &real_start.to_string(),
            ],
            Some(&my_uid.to_string()),
        );
        assert_eq!(code, 0, "renice must succeed when start_ticks matches");
        fs::remove_file(&script).ok();
    }

    #[test]
    fn park_helper_rejects_out_of_range_arguments() {
        let script = stage_script("park", PARK_SCRIPT);
        for args in [
            vec!["set-offline", "abc"],
            vec!["set-offline", "3", "-1"],
            // CPU 0 cannot be parked, and "03" is not a CPU number.
            vec!["set-offline", "0"],
            vec!["set-offline", "03"],
            vec!["online", "3", "0"],
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

    /// The installed script, with only its sysfs root rewritten to a fake
    /// tree: set-offline must leave exactly the listed CPUs offline, bring
    /// the rest back online, and never touch CPU 0 (which has no switch).
    #[test]
    fn set_offline_parks_exactly_the_listed_cpus() {
        let root = std::env::temp_dir().join(format!("argus-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cpu0")).unwrap();
        for (cpu, online) in [(1, "1"), (2, "1"), (3, "1"), (4, "0"), (5, "1")] {
            fs::create_dir_all(root.join(format!("cpu{cpu}"))).unwrap();
            fs::write(root.join(format!("cpu{cpu}/online")), online).unwrap();
        }
        let body = PARK_SCRIPT.replace("/sys/devices/system/cpu/", &format!("{}/", root.display()));
        assert_ne!(body, PARK_SCRIPT);
        let script = stage_script("park-set-offline", &body);

        assert_eq!(run(&script, &["set-offline", "2", "3"], None), 0);

        let online = |cpu: u32| {
            fs::read_to_string(root.join(format!("cpu{cpu}/online")))
                .unwrap()
                .trim()
                .to_string()
        };
        assert_eq!(
            [online(1), online(2), online(3), online(4), online(5)],
            ["1", "0", "0", "1", "1"]
        );
        fs::remove_file(&script).ok();
        fs::remove_dir_all(&root).ok();
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

    /// Root writes the helpers from its own command: exactly the bytes the
    /// app carries, whatever is in the user's directories.
    #[test]
    fn the_root_command_carries_the_helpers_themselves() {
        let files = helper_files();
        for (name, body) in &files {
            assert!(
                !body.contains(EMBED_END),
                "{name} contains the here-document end"
            );
        }
        let names: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
        let script = format!(
            "set -e\n{}cd \"$t\" && cat {}\n",
            embed_and_verify_script(&files),
            names.join(" ")
        );
        let out = Command::new("sh").arg("-c").arg(script).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        let expected: String = files.iter().map(|(_, body)| body.as_str()).collect();
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    }

    /// The rule names the user pkexec was called by, and only that user:
    /// allow_active=yes let every local user run the helpers without a
    /// password.
    #[test]
    fn the_polkit_rule_is_for_the_installing_user() {
        let run = |uid: Option<String>| {
            let script = format!(
                "set -e\n{}{}cat \"$t/rules\"\n",
                embed_and_verify_script(&helper_files()),
                rules_script()
            );
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(script).env_remove("PKEXEC_UID");
            if let Some(uid) = uid {
                cmd.env("PKEXEC_UID", uid);
            }
            cmd.output().unwrap()
        };
        use std::os::unix::fs::MetadataExt;
        let uid = fs::metadata("/proc/self").unwrap().uid().to_string();
        let me = String::from_utf8(
            Command::new("id")
                .args(["-nu", &uid])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let out = run(Some(uid));
        assert!(out.status.success(), "{out:?}");
        let rules = String::from_utf8_lossy(&out.stdout);
        assert!(
            rules.contains(&format!("subject.user === \"{}\"", me.trim())),
            "{rules}"
        );
        assert!(!rules.contains("@USER@"));
        assert!(policy_xml().contains("<allow_active>auth_admin_keep</allow_active>"));
        assert!(!policy_xml().contains("<allow_active>yes</allow_active>"));

        assert!(!run(None).status.success(), "not through pkexec: no rule");
    }

    /// A file that does not match its digest stops the install.
    #[test]
    fn a_damaged_embedded_file_stops_the_install() {
        let files = vec![("a", "first\n".to_string())];
        let mut script = embed_and_verify_script(&files);
        script = script.replacen("first", "other", 1);
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("set -e\n{script}echo INSTALLED\n"))
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(EMBEDDED_FILES_DAMAGED));
        assert!(out.stdout.is_empty());
    }

    /// Verifying copies is pointless if root then installs from the staging
    /// directory itself: every install must read the verified copy in "$t".
    #[test]
    fn root_installs_only_the_verified_copies() {
        let cmd = root_install_command(&helper_files());
        let installs: Vec<_> = cmd.lines().filter(|l| l.starts_with("install ")).collect();
        assert_eq!(installs.len(), 6, "{cmd}");
        for line in installs.iter().skip(1) {
            assert!(line.contains("\"$t/"), "installs from outside $t: {line}");
        }
        let verify = cmd.find("sha256sum -c").expect("no digest check");
        assert!(
            verify < cmd.find("install ").unwrap(),
            "digest check must come first"
        );
        let syntax = Command::new("sh")
            .args(["-n", "-c", &cmd])
            .output()
            .unwrap();
        assert!(syntax.status.success(), "not valid sh: {syntax:?}\n{cmd}");
    }
}
