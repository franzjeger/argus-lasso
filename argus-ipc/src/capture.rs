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
    read_control_in(&directory())
}

fn read_control_in(dir: &Path) -> Control {
    read_regular_capped(&dir.join("control.json"), 4096)
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
/// Start a recording if none is running, or stop the running one: what the
/// CLI and the shortcut do, not knowing which it is.
pub fn toggle(duration_seconds: u32) -> io::Result<Control> {
    update(&directory(), duration_seconds, |old| !old.is_active())
}

/// Start or stop recording. A button labelled from a scan up to a second
/// old must not toggle: "Stop" just after a recording ran out would start a
/// new one. Already in that state, nothing is written.
pub fn set_active(active: bool, duration_seconds: u32) -> io::Result<Control> {
    update(&directory(), duration_seconds, |_| active)
}

fn update(
    dir: &Path,
    duration_seconds: u32,
    active: impl FnOnce(&Control) -> bool,
) -> io::Result<Control> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("control.lock"))?;
    lock.lock()?;
    let old = read_control_in(dir);
    let active = active(&old);
    if active == old.is_active() {
        return Ok(Control { active, ..old });
    }
    let _ = fs::remove_file(dir.join("latest-error.txt"));
    let now = now_ms();
    let c = Control {
        session: format!("{now}-{}", std::process::id()),
        active,
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
/// What a recording measures, as its summary states it.
pub const METRIC: &str = "CPU intervals between successful vkQueuePresentKHR entry timestamps; not GPU time or displayed/FG FPS";

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
/// The counters a recording's summary is judged by, while the recording is
/// still being written. They live in the game's memory, so a game that exited
/// without tearing down its device left rows but no way to tell an intact
/// recording from a damaged one: every such recording was listed Incomplete.
/// The layer writes this next to the rows when a recording starts and again
/// whenever a counter changes; recovery builds the summary from it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub build: String,
    pub failed_presents: u64,
    pub dropped_samples: u64,
    pub io_failed: bool,
}

/// Write `checkpoint` for the recording whose files share `stem` (a path
/// without extension), replacing the previous one in one step.
pub fn write_checkpoint(stem: &Path, checkpoint: &Checkpoint) -> io::Result<()> {
    let path = stem.with_extension("checkpoint.json");
    let temp = stem.with_extension("checkpoint.json.tmp");
    let bytes = serde_json::to_vec(checkpoint).map_err(io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    io::Write::write_all(&mut file, &bytes)?;
    fs::rename(temp, path)
}

fn read_checkpoint(dir: &Path, stem: &str) -> Option<Checkpoint> {
    let bytes = read_regular_capped(&dir.join(format!("{stem}.checkpoint.json")), 4 * 1024)
        .ok()
        .flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// The file name of a program path, Unix or Windows: Wine and Proton
/// games are recorded by their Windows path (`Z:\\games\\Game.exe`).
pub fn program_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
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

/// Recording sessions listed at most, the newest first.
const LISTED_SESSIONS: usize = 30;
/// Summary files read per scan at most, the newest first.
const MAX_SUMMARIES_READ: usize = 500;
/// Largest recording CSV read to recover it (two million rows fit).
const MAX_RECOVERED_CSV: u64 = 96 * 1024 * 1024;

/// Delete the recording `summary` lists: its summary, CSV (finished or
/// partial) and metadata. Only a `.summary.json` directly in the recordings
/// directory is accepted, so nothing else can be removed through this.
pub fn delete_recording(summary: &Path) -> io::Result<()> {
    delete_recording_in(&directory(), summary)
}

fn delete_recording_in(dir: &Path, summary: &Path) -> io::Result<()> {
    let stem = summary
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".summary.json"))
        .filter(|stem| !stem.is_empty() && summary.parent() == Some(dir))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a recording", summary.display()),
            )
        })?;
    // The summary goes last: until it does, a deletion that failed half way
    // is still listed and can be tried again.
    for suffix in [
        ".csv",
        ".csv.partial",
        ".metadata.json",
        ".checkpoint.json",
        ".summary.json",
    ] {
        match fs::remove_file(dir.join(format!("{stem}{suffix}"))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Bytes the files in the recordings directory take.
pub fn disk_use() -> u64 {
    disk_use_in(&directory())
}

fn disk_use_in(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

pub fn load_summaries() -> Vec<(PathBuf, Summary)> {
    let dir = directory();
    recover_orphans(&dir, |pid| Path::new(&format!("/proc/{pid}")).exists());
    load_summaries_in(&dir)
}

/// The recordings of the newest sessions. A session has one per swapchain,
/// and a game that recreates its swapchain on every resize step made one
/// session fill a list cut at 30 files, pushing every earlier one out.
fn load_summaries_in(dir: &Path) -> Vec<(PathBuf, Summary)> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".summary.json"))
        })
        .collect();
    // Names begin with the session, which begins with its start time.
    paths.sort();
    paths.reverse();
    paths.truncate(MAX_SUMMARIES_READ);
    let mut sessions: Vec<String> = Vec::new();
    paths
        .into_iter()
        .filter_map(|p| read_summary_capped(&p).map(|s| (p, s)))
        .filter(|(_, summary)| summary.frames > 0)
        .filter(|(_, summary)| {
            if sessions.contains(&summary.session) {
                return true;
            }
            if sessions.len() >= LISTED_SESSIONS {
                return false;
            }
            sessions.push(summary.session.clone());
            true
        })
        .collect()
}

/// A game that exits or crashes mid-recording never writes a summary; its
/// rows stay in a `.csv.partial`, unlisted and never cleaned up. Once the
/// process is gone, give each such file a summary built from the rows it
/// wrote, complete only if the layer's [`Checkpoint`] shows nothing was lost.
fn recover_orphans(dir: &Path, alive: impl Fn(u32) -> bool) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".csv.partial")) else {
            continue;
        };
        let summary_path = dir.join(format!("{stem}.summary.json"));
        if summary_path.exists() {
            continue; // finished as incomplete; the rows are kept for it
        }
        let Some((session, pid, swapchain)) = parse_recording_name(stem) else {
            continue;
        };
        if alive(pid) {
            continue; // possibly still recording
        }
        let checkpoint = dir.join(format!("{stem}.checkpoint.json"));
        let Some(summary) = recovered_summary(dir, stem, session, pid, swapchain) else {
            // Nothing recorded: nothing to keep.
            let _ = fs::remove_file(dir.join(format!("{stem}.csv.partial")));
            let _ = fs::remove_file(dir.join(format!("{stem}.metadata.json")));
            let _ = fs::remove_file(&checkpoint);
            continue;
        };
        let temp = dir.join(format!("{stem}.summary.json.tmp"));
        let written = serde_json::to_vec_pretty(&summary)
            .map_err(io::Error::other)
            .and_then(|bytes| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&temp)?;
                io::Write::write_all(&mut file, &bytes)
            })
            .and_then(|()| fs::rename(&temp, &summary_path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
            continue;
        }
        // After the summary, which is what lists the recording: complete rows
        // lose the .partial like those finished on device teardown, and the
        // graph reads either name.
        if summary.complete {
            let _ = fs::rename(
                dir.join(format!("{stem}.csv.partial")),
                dir.join(format!("{stem}.csv")),
            );
        }
        let _ = fs::remove_file(&checkpoint);
    }
}

/// `{session}-{pid}-{swapchain:x}-{sequence}`, the session itself being
/// `{start ms}-{pid}`.
fn parse_recording_name(stem: &str) -> Option<(String, u32, u64)> {
    let mut parts = stem.rsplitn(4, '-');
    let _sequence: u32 = parts.next()?.parse().ok()?;
    let swapchain = u64::from_str_radix(parts.next()?, 16).ok()?;
    let pid: u32 = parts.next()?.parse().ok()?;
    let session = parts.next()?.to_string();
    Some((session, pid, swapchain))
}

fn recovered_summary(
    dir: &Path,
    stem: &str,
    session: String,
    pid: u32,
    swapchain: u64,
) -> Option<Summary> {
    let csv =
        read_regular_capped(&dir.join(format!("{stem}.csv.partial")), MAX_RECOVERED_CSV).ok()??;
    // Only whole rows: a last row cut off by the exit may hold a prefix of
    // its number, which would still parse.
    let whole = csv
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(&csv[..0], |end| &csv[..=end]);
    let mut intervals: Vec<u64> = String::from_utf8_lossy(whole)
        .lines()
        .skip(1)
        .filter_map(|line| line.split(',').nth(1)?.parse().ok())
        .filter(|ns| *ns > 0)
        .collect();
    if intervals.is_empty() {
        return None;
    }
    let executable = read_regular_capped(&dir.join(format!("{stem}.metadata.json")), 64 * 1024)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|meta| meta["program"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let frames = intervals.len();
    let duration_seconds = intervals.iter().map(|ns| *ns as f64 / 1e9).sum();
    let (average_fps, low_1_fps, p99_frametime_ms) = statistics(&mut intervals);
    // Rows are written out every 100 ms, so an exit loses at most the last
    // of them, never a gap. With the layer's counters clean, the recording is
    // as intact as one finished on device teardown. A layer too old to write
    // a checkpoint leaves no way to tell, and its recording stays incomplete.
    let checkpoint = read_checkpoint(dir, stem);
    let complete = checkpoint
        .as_ref()
        .is_some_and(|c| !c.io_failed && c.failed_presents == 0 && c.dropped_samples == 0);
    let checkpoint = checkpoint.unwrap_or_default();
    Some(Summary {
        schema: 1,
        metric: METRIC.into(),
        build: checkpoint.build,
        session,
        pid,
        executable,
        swapchain,
        frames,
        duration_seconds,
        average_fps,
        low_1_fps,
        p99_frametime_ms,
        dropped_samples: checkpoint.dropped_samples,
        failed_presents: checkpoint.failed_presents,
        complete,
    })
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

    /// The recordings page's "Stop" is labelled from a scan up to a second
    /// old; toggling then started a new recording if the last one had just
    /// run out.
    #[test]
    fn stopping_a_recording_that_ran_out_starts_nothing() {
        let dir = temp_path("argus-ipc-control");
        let _ = std::fs::remove_dir_all(&dir);
        let started = update(&dir, 5, |_| true).unwrap();
        assert!(started.is_active());
        // It runs out.
        let mut expired = started.clone();
        expired.started_unix_ms -= 10_000;
        std::fs::write(
            dir.join("control.json"),
            serde_json::to_vec(&expired).unwrap(),
        )
        .unwrap();

        let stopped = update(&dir, 5, |_| false).unwrap();
        assert!(!stopped.is_active());
        assert!(!read_control_in(&dir).is_active());
        assert_eq!(
            read_control_in(&dir).session,
            started.session,
            "nothing written"
        );

        let toggled = update(&dir, 5, |old| !old.is_active()).unwrap();
        assert!(toggled.is_active(), "the CLI toggle still starts one");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Recordings were never deleted: CSVs of up to ~60 MB piled up.
    #[test]
    fn a_recording_is_deleted_with_all_its_files_and_nothing_else() {
        let dir = temp_path("argus-ipc-delete");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let files = |stem: &str| {
            [
                ".summary.json",
                ".csv",
                ".csv.partial",
                ".metadata.json",
                ".checkpoint.json",
            ]
            .map(|s| dir.join(format!("{stem}{s}")))
        };
        for path in files("1-2-a-1").iter().chain(&files("1-2-b-2")) {
            fs::write(path, b"x").unwrap();
        }
        fs::write(dir.join("control.json"), b"{}").unwrap();
        assert_eq!(disk_use_in(&dir), 12);

        delete_recording_in(&dir, &dir.join("1-2-a-1.summary.json")).unwrap();
        assert!(files("1-2-a-1").iter().all(|p| !p.exists()));
        assert!(files("1-2-b-2").iter().all(|p| p.exists()));
        assert_eq!(disk_use_in(&dir), 7);

        for refused in [
            dir.join("control.json"),
            dir.join(".summary.json"),
            temp_path("elsewhere").join("1-2-b-2.summary.json"),
        ] {
            assert!(delete_recording_in(&dir, &refused).is_err(), "{refused:?}");
        }
        assert!(dir.join("control.json").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn program_file_names_of_both_kinds_of_path() {
        assert_eq!(program_file_name("/usr/bin/vkcube"), "vkcube");
        assert_eq!(program_file_name("Z:\\games\\Game\\game.exe"), "game.exe");
        assert_eq!(program_file_name("game.exe"), "game.exe");
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

    fn scratch(name: &str) -> PathBuf {
        let dir = temp_path(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recording_names_give_session_pid_and_swapchain() {
        assert_eq!(
            parse_recording_name("1790000000000-4242-777-1a2b-3"),
            Some(("1790000000000-4242".into(), 777, 0x1a2b))
        );
        assert_eq!(parse_recording_name("control"), None);
    }

    /// A game that crashed mid-recording left only a .csv.partial, never
    /// listed and never cleaned up.
    #[test]
    fn a_recording_left_by_a_game_that_died_is_recovered() {
        let dir = scratch("argus-ipc-orphans");
        let dead = "1790000000000-1-900-ab-1";
        let running = "1790000000000-1-901-cd-2";
        let empty = "1790000000000-1-902-ef-3";
        let rows = "present_begin_ns,interval_ns,vulkan_result\n0,10000000,0\n10,20000000,0\n30,5";
        for stem in [dead, running] {
            fs::write(dir.join(format!("{stem}.csv.partial")), rows).unwrap();
        }
        fs::write(
            dir.join(format!("{dead}.metadata.json")),
            r#"{"program":"/usr/bin/vkcube"}"#,
        )
        .unwrap();
        fs::write(
            dir.join(format!("{empty}.csv.partial")),
            "present_begin_ns,interval_ns\n",
        )
        .unwrap();

        recover_orphans(&dir, |pid| pid == 901);

        let summary = read_summary_capped(&dir.join(format!("{dead}.summary.json"))).unwrap();
        assert_eq!(summary.frames, 2, "the torn last row is left out");
        assert!(!summary.complete);
        assert_eq!(summary.executable, "/usr/bin/vkcube");
        assert_eq!(summary.pid, 900);
        assert!((summary.duration_seconds - 0.03).abs() < 1e-9);
        assert!(
            !dir.join(format!("{running}.summary.json")).exists(),
            "still running"
        );
        assert!(
            !dir.join(format!("{empty}.csv.partial")).exists(),
            "nothing to keep"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// A game that exits without tearing down its device leaves rows that are
    /// whole but for the last 100 ms. The layer's checkpoint says whether
    /// anything else was lost, and only a clean one makes them complete.
    #[test]
    fn a_recording_left_by_an_exit_is_judged_by_its_checkpoint() {
        let dir = scratch("argus-ipc-checkpoints");
        let clean = "1790000000000-1-900-ab-1";
        let failed = "1790000000000-1-900-cd-2";
        let rows = "present_begin_ns,interval_ns,vulkan_result\n0,10000000,0\n10,20000000,0\n";
        for stem in [clean, failed] {
            fs::write(dir.join(format!("{stem}.csv.partial")), rows).unwrap();
        }
        let checkpoint = |stem: &str, failed_presents| {
            write_checkpoint(
                &dir.join(stem),
                &Checkpoint {
                    build: "abc".into(),
                    failed_presents,
                    ..Default::default()
                },
            )
            .unwrap();
        };
        checkpoint(clean, 0);
        checkpoint(failed, 2);

        recover_orphans(&dir, |_| false);

        let summary = read_summary_capped(&dir.join(format!("{clean}.summary.json"))).unwrap();
        assert!(summary.complete);
        assert_eq!(summary.build, "abc");
        assert!(
            dir.join(format!("{clean}.csv")).exists(),
            "named as finished"
        );
        assert!(!dir.join(format!("{clean}.csv.partial")).exists());

        let summary = read_summary_capped(&dir.join(format!("{failed}.summary.json"))).unwrap();
        assert!(!summary.complete);
        assert_eq!(summary.failed_presents, 2);
        assert!(dir.join(format!("{failed}.csv.partial")).exists());

        for stem in [clean, failed] {
            assert!(
                !dir.join(format!("{stem}.checkpoint.json")).exists(),
                "the summary replaces the checkpoint"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// One session with a summary per swapchain used to push every earlier
    /// session out of a list cut at 30 files.
    #[test]
    fn the_list_keeps_whole_sessions_and_skips_empty_recordings() {
        let dir = scratch("argus-ipc-sessions");
        let write = |name: &str, session: &str, frames: usize| {
            let summary = Summary {
                session: session.into(),
                frames,
                ..Default::default()
            };
            fs::write(
                dir.join(format!("{name}.summary.json")),
                serde_json::to_vec(&summary).unwrap(),
            )
            .unwrap();
        };
        write("1700000000000-1-5-a-1", "1700000000000-1", 100);
        for i in 0..40 {
            write(
                &format!("1800000000000-2-6-{i:x}-{i}"),
                "1800000000000-2",
                100,
            );
        }
        write("1800000000000-2-6-ff-99", "1800000000000-2", 0);

        let listed = load_summaries_in(&dir);
        assert_eq!(listed.len(), 41);
        assert!(listed.iter().any(|(_, s)| s.session == "1700000000000-1"));
        assert!(listed.iter().all(|(_, s)| s.frames > 0));
        fs::remove_dir_all(&dir).ok();
    }
}
