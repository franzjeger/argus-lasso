//! Portable file-dialog helpers.
//!
//! Tries kdialog (KDE) → zenity (GNOME/GTK) → qarma (Qt/Wayland) → None.
//! Blocking: call from a worker thread, never the GUI thread.

use std::path::PathBuf;
use std::process::Command;

// ── Backend detection ─────────────────────────────────────────────────────────

fn which(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

enum Backend {
    Kdialog,
    Zenity,
    Qarma,
}

fn backend() -> Result<Backend, String> {
    if which("kdialog") {
        return Ok(Backend::Kdialog);
    }
    if which("zenity") {
        return Ok(Backend::Zenity);
    }
    if which("qarma") {
        return Ok(Backend::Qarma);
    }
    Err("No file dialog available. Install kdialog, zenity or qarma.".into())
}

fn run(args: &[&str]) -> Result<Option<String>, String> {
    let out = Command::new(args[0])
        .args(&args[1..])
        .output()
        .map_err(|e| format!("Could not open file dialog: {e}"))?;
    if out.status.success() {
        let path = String::from_utf8(out.stdout).map_err(|_| "File path is not valid UTF-8")?;
        let path = path.trim_end_matches(['\r', '\n']).to_string();
        Ok((!path.is_empty()).then_some(path))
    } else if out.status.code() == Some(1) && out.stderr.is_empty() {
        Ok(None)
    } else {
        Err(format!(
            "File dialog failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Open a file picker. `filter` is a glob string, e.g. `"*.json"`.
/// Returns the selected path, or None if cancelled; errors explain missing or failed backends.
pub fn open(filter: &str) -> Result<Option<PathBuf>, String> {
    let s = match backend()? {
        Backend::Kdialog => run(&["kdialog", "--getopenfilename", ".", filter]),
        Backend::Zenity => run(&[
            "zenity",
            "--file-selection",
            "--title=Open file",
            &format!("--file-filter={filter}"),
        ]),
        Backend::Qarma => run(&[
            "qarma",
            "--file-selection",
            &format!("--file-filter={filter}"),
        ]),
    };
    s.map(|p| p.map(PathBuf::from))
}

/// Save-as picker. `default_name` is the pre-filled filename, `filter` is a glob.
/// Returns the selected path, or None if cancelled; errors explain missing or failed backends.
pub fn save(default_name: &str, filter: &str) -> Result<Option<PathBuf>, String> {
    let s = match backend()? {
        Backend::Kdialog => run(&["kdialog", "--getsavefilename", default_name, filter]),
        // No --confirm-overwrite: zenity ≥ 3.91 (GTK4) removed the flag and
        // exits non-zero when it's passed, silently breaking saving.
        Backend::Zenity => run(&[
            "zenity",
            "--file-selection",
            "--save",
            &format!("--filename={default_name}"),
        ]),
        Backend::Qarma => run(&[
            "qarma",
            "--file-selection",
            "--save",
            &format!("--filename={default_name}"),
        ]),
    };
    s.map(|p| p.map(PathBuf::from))
}
