//! Persistent log file with size-based rotation.
//!
//! The in-app log is a 2000-line ring buffer; anything older is lost on
//! overflow or exit. Once the app turns it on (`enable`), this module
//! mirrors every line to `$XDG_DATA_HOME/argus-lasso/argus-lasso.log`
//! (fallback `~/.local/share/...`), rotating to `.log.1` at 1 MiB so disk use
//! stays bounded at ~2 MiB. Until then, as in tests and the UI tour, lines
//! stay in the in-app log only.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 1024 * 1024;

struct LogFile {
    file: File,
    written: u64,
}

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG: Mutex<Option<LogFile>> = Mutex::new(None);

// eframe's external-event-loop API reports renderer failures through log
// records rather than a Result. Retain that error for the window runner.
static WINDOW_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn take_window_error() -> Option<String> {
    WINDOW_ERROR.lock().ok()?.take()
}

struct Logger(env_logger::Logger);

fn persist(metadata: &log::Metadata<'_>) -> bool {
    metadata.level() <= log::Level::Warn
        || (metadata.level() == log::Level::Info && metadata.target().starts_with("argus_lasso"))
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        persist(metadata) || self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if record.level() == log::Level::Error && record.target() == "eframe::native::run" {
            if let Ok(mut error) = WINDOW_ERROR.lock() {
                *error = Some(record.args().to_string());
            }
        }
        if persist(record.metadata()) {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            append(&format!(
                "[unix:{seconds}] [{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
        if self.0.enabled(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

/// Keep the existing RUST_LOG-controlled stderr output, while persisting
/// diagnostics even when a desktop launcher redirects stderr to /dev/null.
pub fn init_logger() {
    let logger = env_logger::Builder::from_default_env().build();
    let level = logger.filter().max(log::LevelFilter::Info);
    log::set_boxed_logger(Box::new(Logger(logger))).expect("logger initialized once");
    log::set_max_level(level);
}

/// Where the app keeps its log.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("argus-lasso").join("argus-lasso.log"))
}

/// Mirror log lines to `path` from now on. Only the app itself does this, at
/// startup: tests used to append their made-up lines ("[Rule:x] Set nice=5
/// on game(42)") to the user's real log.
pub fn enable(path: PathBuf) {
    let _ = PATH.set(path);
}

fn open(path: &Path) -> Option<LogFile> {
    fs::create_dir_all(path.parent()?).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(LogFile { file, written })
}

/// Append one line (already timestamped by the caller). Failures are silent —
/// logging must never take the app down or spam itself with errors.
pub fn append(line: &str) {
    let Some(path) = PATH.get() else { return };
    let Ok(mut guard) = LOG.lock() else { return };
    write_line(&mut guard, path, line);
}

/// Append `line` to the log at `path`, opening it into `slot` if needed and
/// rotating it to `.log.1` once it has reached `MAX_BYTES`.
fn write_line(slot: &mut Option<LogFile>, path: &Path, line: &str) {
    if slot.is_none() {
        *slot = open(path);
    }
    let Some(lf) = slot.as_mut() else { return };

    if lf.written >= MAX_BYTES {
        // Rotate: current → .1 (replacing any previous .1), then reopen.
        let _ = fs::rename(path, path.with_extension("log.1"));
        *slot = open(path);
    }
    let Some(lf) = slot.as_mut() else { return };
    if writeln!(lf.file, "{line}").is_ok() {
        lf.written += line.len() as u64 + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests log through the same AppState::append_log as the app, and used
    /// to fill the user's real log. Nothing in a test turns the file on.
    #[test]
    fn tests_keep_their_log_lines_in_memory() {
        let mut state = crate::monitor::AppState::default();
        state.append_log("[Rule:x] Set nice=5 on game(42)".into());
        assert!(PATH.get().is_none(), "a test enabled the real log file");
        assert!(state.log_lines.back().unwrap().ends_with("game(42)"));
    }

    #[test]
    fn persistent_diagnostics_exclude_dependency_info_chatter() {
        let metadata = |level, target| log::Metadata::builder().level(level).target(target).build();
        assert!(!persist(&metadata(log::Level::Info, "zbus::object_server")));
        assert!(persist(&metadata(log::Level::Warn, "winit")));
        assert!(persist(&metadata(log::Level::Error, "eframe::native::run")));
        assert!(persist(&metadata(log::Level::Info, "argus_lasso")));
    }

    #[test]
    fn renderer_errors_survive_disabled_stderr_logging() {
        use log::Log;
        let mut builder = env_logger::Builder::new();
        builder.filter_level(log::LevelFilter::Off);
        let logger = Logger(builder.build());
        logger.log(
            &log::Record::builder()
                .level(log::Level::Error)
                .target("eframe::native::run")
                .args(format_args!("Exiting because of error: test EGL failure"))
                .build(),
        );
        assert_eq!(
            take_window_error().as_deref(),
            Some("Exiting because of error: test EGL failure")
        );
        assert!(take_window_error().is_none());
        logger.log(
            &log::Record::builder()
                .level(log::Level::Error)
                .target("unrelated")
                .args(format_args!("not a window failure"))
                .build(),
        );
        assert!(take_window_error().is_none());
    }

    #[test]
    fn the_log_rotates_once_it_is_full() {
        let dir = std::env::temp_dir().join(format!("argus-logfile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("argus-lasso.log");
        let mut slot = None;
        let line = "x".repeat(64 * 1024);
        for _ in 0..16 {
            write_line(&mut slot, &path, &line);
        }
        assert!(!path.with_extension("log.1").exists());
        write_line(&mut slot, &path, "after rotation");
        let rotated = fs::metadata(path.with_extension("log.1")).unwrap().len();
        assert!(rotated >= MAX_BYTES);
        assert_eq!(fs::read_to_string(&path).unwrap(), "after rotation\n");
        fs::remove_dir_all(&dir).ok();
    }
}
