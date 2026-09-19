use std::fs::File;
use std::io::{Read, Write};

#[derive(Debug, Default)]
pub struct FastStat {
    pub comm: String,
    pub ppid: u32,
    pub utime: u64,
    pub stime: u64,
    pub nice: i32,
    pub starttime: u64,
    pub rss_bytes: u64,
}

static PAGE_SIZE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn get_page_size() -> u64 {
    *PAGE_SIZE.get_or_init(|| unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) as u64 })
}

/// Format "/proc/<pid>/stat" into a stack buffer instead of heap-allocating a
/// String for it — read_stat runs once per live process on every daemon
/// tick, and every digit of the largest possible pid plus the fixed
/// surrounding text fits comfortably under 32 bytes.
fn proc_stat_path(pid: u32, buf: &mut [u8; 32]) -> Option<&str> {
    let total = buf.len();
    let mut cursor: &mut [u8] = buf;
    write!(cursor, "/proc/{pid}/stat").ok()?;
    let written = total - cursor.len();
    std::str::from_utf8(&buf[..written]).ok()
}

/// Parse /proc/[pid]/stat with zero allocation (except for comm when needed).
pub fn read_stat(pid: u32, buf: &mut [u8; 1024]) -> Option<FastStat> {
    let mut path_buf = [0u8; 32];
    let path = proc_stat_path(pid, &mut path_buf)?;
    let mut file = File::open(path).ok()?;
    let n = file.read(buf).ok()?;
    if n == 0 {
        return None;
    }
    let data = &buf[..n];

    let start_paren = data.iter().position(|&b| b == b'(')?;
    let end_paren = data.iter().rposition(|&b| b == b')')?;

    // `from_utf8_lossy` already returns an owned String when the input isn't
    // valid UTF-8 (the only case that would actually copy); `.into_owned()`
    // takes it as-is, unlike `.to_string()`, which would clone it again.
    let comm = String::from_utf8_lossy(&data[start_paren + 1..end_paren]).into_owned();

    // The rest of the fields start after ") "
    if end_paren + 2 >= data.len() {
        return None;
    }
    let rest = &data[end_paren + 2..];

    // Split by ASCII space
    let mut parts = rest.split(|&b| b == b' ');

    // Field 3: state -> parts[0]
    let _state = parts.next()?;
    // Field 4: ppid -> parts[1]
    let ppid_str = std::str::from_utf8(parts.next()?).ok()?;
    let ppid: u32 = ppid_str.parse().ok()?;

    // Skip to Field 14 (utime), which is parts[11]
    for _ in 0..9 {
        parts.next()?;
    }
    let utime_str = std::str::from_utf8(parts.next()?).ok()?;
    let utime: u64 = utime_str.parse().ok()?;

    // Field 15: stime -> parts[12]
    let stime_str = std::str::from_utf8(parts.next()?).ok()?;
    let stime: u64 = stime_str.parse().ok()?;

    // Skip to Field 19 (nice) -> parts[16] (skip 3)
    for _ in 0..3 {
        parts.next()?;
    }
    let nice_str = std::str::from_utf8(parts.next()?).ok()?;
    let nice: i32 = nice_str.parse().ok()?;

    // Skip to Field 22 (starttime) -> parts[19] (skip 2)
    for _ in 0..2 {
        parts.next()?;
    }
    let starttime_str = std::str::from_utf8(parts.next()?).ok()?;
    let starttime: u64 = starttime_str.parse().ok()?;

    // Skip to Field 24 (rss) -> parts[21] (skip 1)
    parts.next()?;
    let rss_str = std::str::from_utf8(parts.next()?).ok()?;
    let rss_pages: i64 = rss_str.parse().ok()?; // rss can be negative in procfs technically but usually u64

    let rss_bytes = (rss_pages.max(0) as u64) * get_page_size();

    Some(FastStat {
        comm,
        ppid,
        utime,
        stime,
        nice,
        starttime,
        rss_bytes,
    })
}

/// Returns an iterator over all PIDs in /proc.
pub fn all_pids() -> impl Iterator<Item = u32> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            s.parse::<u32>().ok()
        })
}

/// Reads the command line arguments of a process. Returns empty vector if unavailable.
pub fn read_cmdline(pid: u32, buf: &mut Vec<u8>) -> Vec<String> {
    let path = format!("/proc/{}/cmdline", pid);
    buf.clear();
    if let Ok(mut file) = File::open(&path) {
        let _ = file.read_to_end(buf);
    }
    if buf.is_empty() {
        return Vec::new();
    }

    // Command line arguments are null-separated.
    buf.split(|&b| b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::proc_stat_path;

    #[test]
    fn proc_stat_path_formats_without_heap_allocation() {
        let mut buf = [0u8; 32];
        assert_eq!(proc_stat_path(1, &mut buf), Some("/proc/1/stat"));
        assert_eq!(proc_stat_path(123_456, &mut buf), Some("/proc/123456/stat"));
    }

    #[test]
    fn proc_stat_path_handles_the_largest_possible_pid() {
        let mut buf = [0u8; 32];
        assert_eq!(
            proc_stat_path(u32::MAX, &mut buf),
            Some("/proc/4294967295/stat")
        );
    }
}
