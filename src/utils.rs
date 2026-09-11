//! Low-level Linux helpers: CPU affinity, nice, ionice, cpulist parsing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;

// ── cpulist parsing / formatting ──────────────────────────────────────────────

/// Parse "0-7,16-23" → {0,1,2,3,4,5,6,7,16,17,18,19,20,21,22,23}
pub fn cpulist_to_set(cpulist: &str) -> Result<HashSet<u32>, String> {
    let mut result = HashSet::new();
    let trimmed = cpulist.trim();
    if trimmed.is_empty() {
        return Ok(result);
    }
    for part in trimmed.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: u32 = lo
                .trim()
                .parse()
                .map_err(|e| format!("bad cpulist range '{part}': {e}"))?;
            let hi: u32 = hi
                .trim()
                .parse()
                .map_err(|e| format!("bad cpulist range '{part}': {e}"))?;
            if lo > hi {
                // A reversed range ("7-2") would silently expand to nothing and
                // be accepted as an empty set — reject it explicitly.
                return Err(format!("reversed cpulist range '{part}' (lo > hi)"));
            }
            for c in lo..=hi {
                result.insert(c);
            }
        } else {
            let n: u32 = part
                .parse()
                .map_err(|e| format!("bad cpulist item '{part}': {e}"))?;
            result.insert(n);
        }
    }
    Ok(result)
}

/// Convert {0,1,2,3,5} → "0-3,5"
pub fn cpuset_to_cpulist(cpus: &HashSet<u32>) -> String {
    if cpus.is_empty() {
        return String::new();
    }
    let mut sorted: Vec<u32> = cpus.iter().copied().collect();
    sorted.sort_unstable();

    let mut ranges: Vec<String> = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &c in &sorted[1..] {
        if c == end + 1 {
            end = c;
        } else {
            ranges.push(if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            });
            start = c;
            end = c;
        }
    }
    ranges.push(if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    });
    ranges.join(",")
}

#[allow(dead_code)]
pub fn validate_cpulist(cpulist: &str) -> bool {
    let max_cpu = get_cpu_count().saturating_sub(1);
    match cpulist_to_set(cpulist) {
        Ok(set) if !set.is_empty() => set.iter().all(|&c| c <= max_cpu),
        _ => false,
    }
}

// ── Thread enumeration ────────────────────────────────────────────────────────

/// Return all thread IDs (TIDs) for a process by reading /proc/<pid>/task/.
/// Falls back to [pid] on error.
pub fn get_tids(pid: u32) -> Vec<u32> {
    let task_dir = format!("/proc/{pid}/task");
    match fs::read_dir(&task_dir) {
        Ok(entries) => entries
            .filter_map(|e| {
                e.ok()
                    .and_then(|e| e.file_name().to_str().map(|s| s.to_owned()))
                    .and_then(|s| s.parse::<u32>().ok())
            })
            .collect(),
        Err(_) => vec![pid],
    }
}

// ── sched_setaffinity ────────────────────────────────────────────────────────

/// Apply CPU affinity to a process AND all its threads via sched_setaffinity(2).
/// Returns true if at least one thread was set successfully.
pub fn set_affinity(pid: u32, cpulist: &str) -> bool {
    let cpuset = match cpulist_to_set(cpulist) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            log::warn!("set_affinity: empty cpulist for pid {pid}");
            return false;
        }
        Err(e) => {
            log::warn!("set_affinity: bad cpulist {cpulist:?} for pid {pid}: {e}");
            return false;
        }
    };

    // Build nix CpuSet
    use nix::sched::{sched_setaffinity, CpuSet};
    use nix::unistd::Pid;

    let mut cpu_set = CpuSet::new();
    for cpu in &cpuset {
        if let Err(e) = cpu_set.set(*cpu as usize) {
            log::warn!("CpuSet::set cpu={cpu}: {e}");
        }
    }

    let tids = get_tids(pid);
    let mut any_ok = false;
    for tid in tids {
        match sched_setaffinity(Pid::from_raw(tid as i32), &cpu_set) {
            Ok(_) => {
                any_ok = true;
            }
            Err(e) => {
                log::debug!("sched_setaffinity tid={tid}: {e}");
            }
        }
    }
    if any_ok {
        log::debug!("affinity pid={pid} cpulist={cpulist}: applied");
    }
    any_ok
}

/// Read current affinity of the main thread as a cpulist string.
pub fn get_affinity_str(pid: u32) -> String {
    use nix::sched::sched_getaffinity;
    use nix::unistd::Pid;
    match sched_getaffinity(Pid::from_raw(pid as i32)) {
        Ok(cpu_set) => {
            let mut cpus = HashSet::new();
            for i in 0..CpuSet::count() {
                if cpu_set.is_set(i).unwrap_or(false) {
                    cpus.insert(i as u32);
                }
            }
            cpuset_to_cpulist(&cpus)
        }
        Err(_) => String::new(),
    }
}

use nix::sched::CpuSet;

// ── nice ──────────────────────────────────────────────────────────────────────

/// Set nice priority via `setpriority` syscall.
/// Negative values require root/CAP_SYS_NICE. Returns true on success.
pub fn set_nice(pid: u32, nice: i32) -> bool {
    use nix::libc;
    // clear errno before call, because setpriority can legitimately return -1
    // (if the previous nice was -1), so we must check if errno changed.
    // However, we only care about success (res == 0) for the most part,
    // but wait, setpriority returns 0 on success according to POSIX,
    // actually it returns the new nice value on Linux sometimes?
    // POSIX says: Upon successful completion, setpriority() shall return 0.
    // Let's use it as standard.
    unsafe {
        *libc::__errno_location() = 0;
    }
    let res = unsafe {
        libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice as libc::c_int)
    };
    if res == 0 {
        log::debug!("setpriority pid={pid} nice={nice}: OK");
        true
    } else {
        let err = std::io::Error::last_os_error();
        log::warn!("setpriority pid={pid} nice={nice} failed: {err}");
        false
    }
}

// ── ionice ───────────────────────────────────────────────────────────────────

/// Set I/O priority via `ioprio_set` syscall.
/// class: 1=realtime, 2=best-effort, 3=idle. level: 0-7 (RT and BE only).
pub fn set_ionice(pid: u32, class: i32, level: Option<i32>) -> bool {
    use nix::libc;
    let class_val = (class as u32) & 0x7;
    let data_val = (level.unwrap_or(0) as u32) & 0x1fff;
    let prio = (class_val << 13) | data_val;

    let res = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            1, // IOPRIO_WHO_PROCESS
            pid as libc::c_int,
            prio as libc::c_int,
        )
    };
    if res == 0 {
        log::debug!("ioprio_set pid={pid} class={class} level={level:?}: OK");
        true
    } else {
        let err = std::io::Error::last_os_error();
        log::warn!("ioprio_set pid={pid} class={class} failed: {err}");
        false
    }
}

// ── Dirty-check reads (avoid redundant syscalls) ─────────────────────────────

/// Read the current nice value for a process from /proc/<pid>/stat.
/// Returns None if the process has exited or cannot be read.
pub fn get_nice(pid: u32) -> Option<i32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt
    //         majflt cmajflt utime stime cutime cstime priority nice ...
    // Skip "pid (comm) " by finding the last ')' (comm may contain spaces/parens).
    let after_comm = stat.rfind(')')?.checked_add(1)?;
    let rest = &stat[after_comm..];
    // After closing ')', fields are space-separated starting with state.
    // Index 16 (0-based) is nice (state=0, ppid=1, …, priority=15, nice=16).
    rest.split_whitespace().nth(16).and_then(|s| s.parse().ok())
}

/// Read the current ionice class and level for a process via ioprio_get syscall.
/// Returns None if the syscall fails (e.g., process gone, unsupported).
pub fn get_ionice_raw(pid: u32) -> Option<(i32, i32)> {
    use nix::libc;
    let prio = unsafe {
        libc::syscall(
            libc::SYS_ioprio_get,
            1, /* IOPRIO_WHO_PROCESS */
            pid as libc::c_int,
        )
    };
    if prio < 0 {
        return None;
    }
    let class = ((prio as u32 >> 13) & 0x7) as i32;
    let level = (prio as u32 & 0x1fff) as i32;
    Some((class, level))
}

// ── CPU topology helpers ──────────────────────────────────────────────────────

/// Return the set of currently online CPU numbers from /sys/devices/system/cpu/online.
pub fn get_online_cpus() -> HashSet<u32> {
    read_cpulist_file("/sys/devices/system/cpu/online")
        .unwrap_or_else(|| (0..get_cpu_count()).collect())
}

/// Return the set of offline CPU numbers from /sys/devices/system/cpu/offline.
pub fn get_offline_cpus() -> HashSet<u32> {
    read_cpulist_file("/sys/devices/system/cpu/offline").unwrap_or_default()
}

/// Return total logical CPU count including parked CPUs.
/// Uses /sys/devices/system/cpu/present so parked CPUs are counted.
pub fn get_cpu_count() -> u32 {
    if let Some(cpus) = read_cpulist_file("/sys/devices/system/cpu/present") {
        if let Some(&max) = cpus.iter().max() {
            return max + 1;
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

pub fn read_cpulist_file(path: &str) -> Option<HashSet<u32>> {
    let text = fs::read_to_string(path).ok()?;
    cpulist_to_set(text.trim()).ok()
}

/// Returns a map: primary CPU (lowest-numbered logical CPU per physical core)
/// → its HT sibling CPUs. Read from /sys topology/core_id.
/// Used for grouped affinity display in the process table.
pub fn build_core_pairs() -> HashMap<u32, Vec<u32>> {
    let n = get_cpu_count();
    let mut core_to_logical: HashMap<u32, Vec<u32>> = HashMap::new();
    for cpu in 0..n {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/core_id");
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(core_id) = raw.trim().parse::<u32>() {
                core_to_logical.entry(core_id).or_default().push(cpu);
            }
        }
    }
    let mut pairs: HashMap<u32, Vec<u32>> = HashMap::new();
    for mut logical in core_to_logical.into_values() {
        logical.sort_unstable();
        if logical.len() >= 2 {
            pairs.insert(logical[0], logical[1..].to_vec());
        }
    }
    pairs
}

// ── Wine/Proton name resolution ───────────────────────────────────────────────

/// Return the best human-readable process name.
///
/// Wine/Proton processes have comm='Main' (or other generic names) but
/// cmdline[0] is a Windows path like Z:\...\PathOfExileSteam.exe.
/// Also handles comm truncated at 15 chars.
pub fn resolve_name(comm: &str, cmdline: &[String]) -> String {
    if let Some(arg0) = cmdline.first() {
        // Windows path: contains backslash and ends with .exe
        if arg0.contains('\\') && arg0.to_lowercase().ends_with(".exe") {
            let basename = arg0.replace('\\', "/");
            let basename = basename.trim_end_matches('/');
            if let Some(name) = basename.rsplit('/').next() {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        // comm is capped at 15 chars by the kernel; try cmdline[0] basename
        if comm.len() == 15 {
            if let Some(basename) = std::path::Path::new(arg0).file_name() {
                let s = basename.to_string_lossy();
                if s.len() > 15 {
                    return s.into_owned();
                }
            }
        }
    }
    comm.to_string()
}

// ── Per-process detail readout (for the details window) ──────────────────────

/// Snapshot of one process's procfs details. Cheap to read (a handful of
/// small files for a single PID); refreshed on the display cadence.
#[derive(Debug, Clone, Default)]
pub struct ProcDetails {
    /// Kernel state, e.g. "S (sleeping)"
    pub state: String,
    /// (tid, thread name), capped at 128 entries
    pub threads: Vec<(u32, String)>,
    pub thread_count: usize,
    /// Open file descriptors; None if /proc/<pid>/fd is unreadable
    pub fd_count: Option<usize>,
    pub cwd: String,
    pub exe: String,
}

/// Read details for one PID. Returns None if the process is gone.
pub fn read_proc_details(pid: u32) -> Option<ProcDetails> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut d = ProcDetails::default();
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("State:") {
            d.state = v.trim().to_string();
        }
    }

    let task_dir = format!("/proc/{pid}/task");
    if let Ok(entries) = std::fs::read_dir(&task_dir) {
        for entry in entries.flatten() {
            let tid_str = entry.file_name();
            let Ok(tid) = tid_str.to_string_lossy().parse::<u32>() else {
                continue;
            };
            d.thread_count += 1;
            if d.threads.len() < 128 {
                let comm = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                d.threads.push((tid, comm));
            }
        }
    }
    d.threads.sort_unstable_by_key(|(tid, _)| *tid);

    d.fd_count = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()
        .map(|it| it.count());
    d.cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    d.exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Some(d)
}

// ── Process tree ───────────────────────────────────────────────────────────────

/// Collect `root_pid` and every descendant (children, grandchildren, …) from a
/// snapshot's pid→ppid edges. Returns the root first, then descendants in
/// breadth-first order, so killing in reverse order terminates leaves before
/// their parents (avoids reparenting orphans to init mid-sweep).
pub fn process_tree(root_pid: u32, snapshot: &[(u32, u32)]) -> Vec<u32> {
    use std::collections::HashMap;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut known = HashSet::new();
    for &(pid, ppid) in snapshot {
        children.entry(ppid).or_default().push(pid);
        known.insert(pid);
    }
    if !known.contains(&root_pid) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root_pid);
    while let Some(pid) = queue.pop_front() {
        out.push(pid);
        if let Some(kids) = children.get(&pid) {
            for &k in kids {
                queue.push_back(k);
            }
        }
    }
    out
}

// ── Listening ports ────────────────────────────────────────────────────────────

/// Parse a `/proc/net/tcp{,6}` table into a map of socket inode → local port.
fn parse_tcp_file(path: &str, out: &mut HashMap<u64, u16>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines().skip(1) {
        let mut f = line.split_whitespace();
        let _ = f.next(); // sl
        let Some(local) = f.next() else { continue };
        let _rem = f.next();
        let _st = f.next();
        let _flags = f.next();
        let _uid = f.next();
        let Some(inode) = f.next() else { continue };
        let Ok(inode) = inode.parse::<u64>() else {
            continue;
        };
        if inode == 0 {
            continue;
        }
        out.insert(inode, hex_port(local));
    }
}

/// "0100007F:0016" → 22 (the ":0016" part is the port in hex, big-endian).
fn hex_port(field: &str) -> u16 {
    let Some(hex) = field.rsplit_once(':').map(|(_, p)| p) else {
        return 0;
    };
    u16::from_str_radix(hex, 16).unwrap_or(0)
}

/// The socket inodes a PID currently holds (from `/proc/PID/fd`).
fn socket_inodes_for_pid(pid: u32) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    if let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) {
        for entry in entries.flatten() {
            if let Ok(target) = fs::read_link(entry.path()) {
                if let Some(inner) = target.to_string_lossy().strip_prefix("socket:[") {
                    if let Some(num) = inner.strip_suffix(']') {
                        if let Ok(inode) = num.parse::<u64>() {
                            inodes.insert(inode);
                        }
                    }
                }
            }
        }
    }
    inodes
}

/// Build an inode → local-port map from the kernel's TCP tables.
fn socket_port_map() -> HashMap<u64, u16> {
    let mut tables: HashMap<u64, u16> = HashMap::new();
    parse_tcp_file("/proc/net/tcp", &mut tables);
    parse_tcp_file("/proc/net/tcp6", &mut tables);
    tables
}

/// The PIDs (from `snapshot`) that hold a socket bound to `port`.
pub fn pids_for_port(port: u16, snapshot: &[u32]) -> HashSet<u32> {
    let tables = socket_port_map();
    let mut out = HashSet::new();
    for &pid in snapshot {
        if socket_inodes_for_pid(pid)
            .iter()
            .any(|inode| tables.get(inode) == Some(&port))
        {
            out.insert(pid);
        }
    }
    out
}

// ── Snapshot export ────────────────────────────────────────────────────────────

/// Serialize a process snapshot to CSV (header + one row per process).
pub fn export_csv(procs: &[crate::monitor::ProcInfo]) -> String {
    let mut out = String::from("pid,ppid,name,cpu_percent,gpu_percent,mem_rss_mb,nice,affinity,ionice,disk_read_bps,disk_write_bps,cmdline\n");
    for p in procs {
        out.push_str(&format!(
            "{},{},{},{:.1},{:.1},{},{},{},{},{},{},{}\n",
            p.pid,
            p.ppid,
            csv_field(&p.name),
            p.cpu_percent,
            p.gpu_percent,
            p.mem_rss / 1024 / 1024,
            p.nice,
            csv_field(&p.affinity),
            csv_field(&p.ionice),
            p.disk_read_bps,
            p.disk_write_bps,
            csv_field(p.cmdline.as_str())
        ));
    }
    out
}

/// Serialize a process snapshot to pretty JSON.
pub fn export_json(procs: &[crate::monitor::ProcInfo]) -> String {
    let items: Vec<serde_json::Value> = procs
        .iter()
        .map(|p| {
            serde_json::json!({
                "pid": p.pid,
                "ppid": p.ppid,
                "name": p.name,
                "cpu_percent": (p.cpu_percent * 10.0).round() / 10.0,
                "gpu_percent": (p.gpu_percent * 10.0).round() / 10.0,
                "mem_rss_mb": p.mem_rss / 1024 / 1024,
                "nice": p.nice,
                "affinity": p.affinity,
                "ionice": p.ionice,
                "disk_read_bps": p.disk_read_bps,
                "disk_write_bps": p.disk_write_bps,
                "cmdline": p.cmdline.as_str(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "processes": items }))
        .unwrap_or_else(|_| "[]".into())
}

/// Quote a CSV field if it contains a comma, quote, or newline.
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpulist_parses_ranges_and_items() {
        let s = cpulist_to_set("0-2,5").unwrap();
        assert_eq!(s, [0u32, 1, 2, 5].into_iter().collect());
    }

    #[test]
    fn cpulist_accepts_empty() {
        assert!(cpulist_to_set("").unwrap().is_empty());
        assert!(cpulist_to_set("  ").unwrap().is_empty());
    }

    #[test]
    fn cpulist_rejects_reversed_range() {
        // "7-2" used to silently parse to an empty set that was accepted.
        assert!(cpulist_to_set("7-2").is_err());
        assert!(cpulist_to_set("0-7,15-8").is_err());
    }

    #[test]
    fn cpulist_rejects_garbage() {
        assert!(cpulist_to_set("0-x").is_err());
        assert!(cpulist_to_set("abc").is_err());
    }

    #[test]
    fn process_tree_collects_descendants() {
        // 1 → 2,3 ; 2 → 4 ; 3 → 5,6
        let snap = [
            (1u32, 0u32),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 3),
            (6, 3),
            (99, 0),
        ];
        let tree = process_tree(1, &snap);
        assert_eq!(tree[0], 1, "root must come first");
        let set: std::collections::HashSet<u32> = tree.iter().copied().collect();
        assert_eq!(set, [1, 2, 3, 4, 5, 6].into_iter().collect());
        assert!(!set.contains(&99), "unrelated pid must be excluded");
    }

    #[test]
    fn process_tree_single_root() {
        let snap = [(42u32, 1u32)];
        assert_eq!(process_tree(42, &snap), vec![42]);
    }

    #[test]
    fn process_tree_unknown_root_is_empty() {
        let snap = [(2u32, 1u32)];
        assert!(process_tree(999, &snap).is_empty());
    }

    #[test]
    fn hex_port_reads_high_nibble() {
        assert_eq!(hex_port("0100007F:01BB"), 443); // 0x01BB
        assert_eq!(hex_port("00000000:0050"), 80);
        assert_eq!(hex_port("garbage"), 0);
    }

    #[test]
    fn csv_field_quotes_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn export_csv_has_header_and_rows() {
        let p = crate::monitor::ProcInfo {
            pid: 7,
            ppid: 1,
            name: "bash".into(),
            cpu_percent: 12.345,
            mem_rss: 2 * 1024 * 1024,
            ..Default::default()
        };
        let out = export_csv(&[p]);
        assert!(out.starts_with("pid,ppid,name,"));
        assert!(out.contains("7,1,bash,12.3,0.0,2,"));
        assert!(out.ends_with(",\n"));
    }

    #[test]
    fn export_json_is_valid() {
        let p = crate::monitor::ProcInfo {
            pid: 7,
            name: "bash".into(),
            ..Default::default()
        };
        let out = export_json(&[p]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["processes"][0]["pid"], 7);
        assert_eq!(v["processes"][0]["name"], "bash");
    }
}
