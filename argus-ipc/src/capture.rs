//! Recording control shared through the private HOME mount visible to Proton.
//! This is user-owned data, never a privileged command channel.
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::PathBuf,
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
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share/argus-lasso/benchmarks")
        })
}
pub fn read_control() -> Control {
    fs::read(directory().join("control.json"))
        .ok()
        .filter(|b| b.len() < 4096)
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        .filter_map(|p| {
            File::open(&p)
                .ok()
                .and_then(|f| serde_json::from_reader(f).ok())
                .map(|s| (p, s))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
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
