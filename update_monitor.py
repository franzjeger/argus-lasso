import re

with open("src/monitor.rs", "r") as f:
    text = f.read()

# Replace collect_snapshot body
old_collect = """fn collect_snapshot(
    prev_times: &mut HashMap<u32, u64>,
    prev_sys_total: u64,
    caches: &mut SnapshotCaches,
    detail: bool,
    io_elapsed: f32,
) -> (Vec<ProcInfo>, HashMap<u32, u64>, u64) {"""

new_collect = """fn collect_snapshot(
    prev_times: &mut HashMap<u32, u64>,
    prev_sys_total: u64,
    caches: &mut SnapshotCaches,
    detail: bool,
    io_elapsed: f32,
) -> (Vec<ProcInfo>, HashMap<u32, u64>, u64) {
    let mut new_times: HashMap<u32, u64> = HashMap::new();
    let mut snapshot: Vec<ProcInfo> = Vec::new();

    let sys_total = read_sys_cpu_total();
    let sys_delta = sys_total.saturating_sub(prev_sys_total) as f32;
    let n_cpus = utils::get_online_cpus().len().max(1) as f32;

    let mut stat_buf = [0u8; 1024];
    let mut cmd_buf = Vec::with_capacity(1024);

    for pid in crate::fast_proc::all_pids() {
        let stat = match crate::fast_proc::read_stat(pid, &mut stat_buf) {
            Some(s) => s,
            None => continue,
        };

        let ppid = stat.ppid;

        let meta = match caches.meta.get(&pid) {
            Some(m) if m.start_time == stat.starttime => m,
            _ => {
                let cmdline = crate::fast_proc::read_cmdline(pid, &mut cmd_buf);
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
            (delta_ticks / sys_delta * n_cpus * 100.0).min(n_cpus * 100.0)
        } else {
            0.0
        };

        let mem_rss = stat.rss_bytes;
        let nice = stat.nice;

        let (affinity, ionice, disk_read_bps, disk_write_bps) = if detail {
            let affinity = utils::get_affinity_str(pid);
            let ionice = read_ionice(pid);
            let (r, w) = read_proc_io(pid, &mut caches.io, io_elapsed);
            caches
                .display
                .insert(pid, (affinity.clone(), ionice.clone()));
            (affinity, ionice, r, w)
        } else {
            match caches.display.get(&pid) {
                Some((a, i)) => (a.clone(), i.clone(), 0, 0),
                None => (String::new(), String::new(), 0, 0),
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

    (snapshot, new_times, sys_total)
}"""

# Find start
start_idx = text.find(old_collect)
if start_idx == -1:
    print("Could not find start_idx")
    import sys
    sys.exit(1)

# Find the end of the function (look for the next top-level function)
end_idx = text.find("\nfn read_sys_cpu_total()", start_idx)
if end_idx == -1:
    print("Could not find end_idx")
    import sys
    sys.exit(1)

text = text[:start_idx] + new_collect + text[end_idx:]

with open("src/monitor.rs", "w") as f:
    f.write(text)

