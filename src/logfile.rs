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
