//! Recording control shared through the private HOME mount visible to Proton.
//! This is user-owned data, never a privileged command channel.
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Control {
    pub session: String,
    pub active: bool,
    pub started_unix_ms: u64,
    pub duration_seconds: u32,
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn directory() -> PathBuf {
    std::env::var_os("ARGUS_CAPTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // An empty HOME would make this relative to the working directory.
            std::env::home_dir()
                .unwrap_or_else(|| PathBuf::from("/nonexistent"))
                .join(".local/share/argus-lasso/benchmarks")
        })
}

/// Read at most `limit` bytes of `path`, which must be a regular file.
///
/// Everything in this directory is written by the other side of the
/// boundary — the game or the app — so it is untrusted in shape: a FIFO in
/// place of a file would block a plain open forever, waiting for a writer,
/// and a huge file must not be loaded whole. `Ok(None)` if the file is larger
/// than `limit`.
pub fn read_regular_capped(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    open_regular(path)?.take(limit + 1).read_to_end(&mut buf)?;
    Ok((buf.len() as u64 <= limit).then_some(buf))
}

/// Open `path` for reading only if it is a regular file, without blocking.
pub fn open_regular(path: &Path) -> io::Result<File> {
    // O_NONBLOCK makes opening a FIFO return at once instead of waiting for a
    // writer; it has no effect on reading a regular file.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

pub fn read_control() -> Control {
    read_regular_capped(&directory().join("control.json"), 4096)
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
impl Control {
    pub fn is_active(&self) -> bool {
        self.active
            && !self.session.is_empty()
            && self.session.len() <= 64
            && self
                .session
                .bytes()
                .all(|c| c.is_ascii_digit() || c == b'-')
            && now_ms().saturating_sub(self.started_unix_ms)
                < u64::from(self.duration_seconds.clamp(5, 600)) * 1000
    }
}
pub fn toggle(duration_seconds: u32) -> io::Result<Control> {
    let dir = directory();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("control.lock"))?;
    lock.lock()?;
    let old = read_control();
    let _ = fs::remove_file(dir.join("latest-error.txt"));
    let now = now_ms();
    let c = Control {
        session: format!("{now}-{}", std::process::id()),
        active: !old.is_active(),
        started_unix_ms: now,
        duration_seconds: duration_seconds.clamp(5, 600),
    };
    let temp = dir.join("control.json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer(&mut file, &c)?;
    file.sync_all()?;
    fs::rename(temp, dir.join("control.json"))?;
    Ok(c)
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub schema: u32,
    pub metric: String,
    pub build: String,
    pub session: String,
    pub pid: u32,
    pub executable: String,
    pub swapchain: u64,
    pub frames: usize,
    pub duration_seconds: f64,
    pub average_fps: Option<f64>,
    pub low_1_fps: Option<f64>,
    pub p99_frametime_ms: Option<f64>,
    pub dropped_samples: u64,
    pub failed_presents: u64,
    pub complete: bool,
}
pub fn statistics(intervals_ns: &mut [u64]) -> (Option<f64>, Option<f64>, Option<f64>) {
    if intervals_ns.is_empty() {
        return (None, None, None);
    }
    let n = intervals_ns.len();
    let total: f64 = intervals_ns.iter().map(|n| *n as f64).sum();
    if total <= 0.0 {
        return (None, None, None);
    }
    intervals_ns.sort_unstable();
    let count = n.div_ceil(100);
    let slow: f64 = intervals_ns[n - count..].iter().map(|n| *n as f64).sum();
    (
        Some(n as f64 * 1e9 / total),
        Some(count as f64 * 1e9 / slow),
        Some(intervals_ns[(99 * n).div_ceil(100) - 1] as f64 / 1e6),
    )
}
/// Summary files are written by the in-process recorder running inside the
/// game (argus-layer), the less-trusted side of this boundary — a compromised
/// or simply buggy game binary can drop an arbitrarily large file at this
/// same-uid-writable path. A real Summary is a handful of scalars and short
/// strings, nowhere near this size; capping the *read* (not just checking the
/// size after) means a huge file costs at most one bounded allocation to
/// reject, not an attempt to load the whole thing into memory.
const MAX_SUMMARY_BYTES: u64 = 64 * 1024;

fn read_summary_capped(path: &Path) -> Option<Summary> {
    let buf = read_regular_capped(path, MAX_SUMMARY_BYTES).ok()??;
    serde_json::from_slice(&buf).ok()
}

pub fn load_summaries() -> Vec<(PathBuf, Summary)> {
    let mut paths: Vec<_> = fs::read_dir(directory())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".summary.json"))
        })
        .collect();
    paths.sort();
    paths.reverse();
    paths.truncate(30);
    paths
        .into_iter()
        .filter_map(|p| read_summary_capped(&p).map(|s| (p, s)))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", std::process::id()))
    }

    /// A real Summary is a handful of scalars and short strings; this proves
    /// the cap doesn't get in the way of an ordinary, legitimately-sized one.
    #[test]
    fn read_summary_capped_accepts_a_normal_summary() {
        let path = temp_path("argus-ipc-summary-ok");
        let summary = Summary {
            schema: 1,
            metric: "fps".into(),
            build: "1.0.0".into(),
            session: "12345".into(),
            pid: 42,
            executable: "/usr/bin/game".into(),
            swapchain: 7,
            frames: 1000,
            duration_seconds: 60.0,
            average_fps: Some(120.0),
            low_1_fps: Some(90.0),
            p99_frametime_ms: Some(11.0),
            dropped_samples: 0,
            failed_presents: 0,
            complete: true,
        };
        std::fs::write(&path, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert_eq!(read_summary_capped(&path), Some(summary));
        std::fs::remove_file(&path).ok();
    }

    /// The whole point of the cap: a file from the less-trusted side of this
    /// boundary (the in-process recorder running inside the game) far larger
    /// than any real Summary must be rejected without ever being fully
    /// buffered or parsed as one giant JSON value.
    #[test]
    fn read_summary_capped_rejects_an_oversized_file() {
        let path = temp_path("argus-ipc-summary-huge");
        // Well past MAX_SUMMARY_BYTES, and not valid JSON once truncated to
        // it either way.
        let huge = format!("{{\"metric\":\"{}", "x".repeat(200_000));
        std::fs::write(&path, &huge).unwrap();
        assert_eq!(read_summary_capped(&path), None);
        std::fs::remove_file(&path).ok();
    }

    /// A FIFO in place of a recording file used to block the reader until
    /// something wrote to it — forever, stalling every later scan.
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let path = temp_path("argus-ipc-fifo");
        let _ = std::fs::remove_file(&path);
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path and a plain mode.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader_path = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_regular_capped(&reader_path, 16).is_err());
        });
        let refused = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading a FIFO blocked");
        assert!(refused);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn capped_reads_accept_the_limit_and_refuse_beyond_it() {
        let path = temp_path("argus-ipc-capped");
        std::fs::write(&path, b"12345").unwrap();
        assert_eq!(
            read_regular_capped(&path, 5).unwrap(),
            Some(b"12345".to_vec())
        );
        assert_eq!(read_regular_capped(&path, 4).unwrap(), None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_summary_capped_returns_none_for_a_missing_file() {
        let path = temp_path("argus-ipc-summary-missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_summary_capped(&path), None);
    }

    #[test]
    fn explicit_low_definition_and_empty_capture() {
        assert_eq!(statistics(&mut []), (None, None, None));
        let mut ms = vec![2_000_000; 200];
        ms[0] = 20_000_000;
        ms[1] = 10_000_000;
        let (avg, low, p99) = statistics(&mut ms);
        assert!((avg.unwrap() - 200e3 / 426.0).abs() < 0.001);
        assert!((low.unwrap() - 1000.0 / 15.0).abs() < 0.001);
        assert_eq!(p99, Some(2.0));
    }
}
