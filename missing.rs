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


