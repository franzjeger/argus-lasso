//! File-backed requests from the CLI and from a second launch, consumed by
//! the single running instance's monitor thread. Each empty file is a
//! complete request; create_new publishes it atomically.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

const QUEUE_DIR: &str = "overlay-toggle-requests";
/// A second launch asks the running instance to show its window.
const SHOW_WINDOW_DIR: &str = "show-window-requests";

pub fn request(config_dir: &Path) -> io::Result<()> {
    request_in(&config_dir.join(QUEUE_DIR))
}

pub fn request_show_window(config_dir: &Path) -> io::Result<()> {
    request_in(&config_dir.join(SHOW_WINDOW_DIR))
}

fn request_in(queue: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(queue)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(queue.join(uuid::Uuid::new_v4().to_string()))?;
    Ok(())
}

fn consume(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => {
            log::warn!("Could not consume request {}: {e}", path.display());
            false
        }
    }
}

/// Only successfully removed requests count. Failed removals must not toggle
/// repeatedly. Requests arriving during a scan may wait until the next tick.
pub fn drain(config_dir: &Path) -> usize {
    // Consume requests left by the previous CLI version as well.
    usize::from(consume(&config_dir.join("toggle_overlay"))) + drain_in(&config_dir.join(QUEUE_DIR))
}

/// Number of show-window requests taken from the queue.
pub fn drain_show_window(config_dir: &Path) -> usize {
    drain_in(&config_dir.join(SHOW_WINDOW_DIR))
}

fn drain_in(queue: &Path) -> usize {
    let mut count = 0;
    match fs::read_dir(queue) {
        Ok(entries) => {
            // Bound per-tick work so a burst cannot starve monitoring.
            for entry in entries.take(256) {
                match entry {
                    Ok(entry) => {
                        if entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
                            && consume(&entry.path())
                        {
                            count += 1;
                        }
                    }
                    Err(e) => log::warn!("Could not read request in {}: {e}", queue.display()),
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Could not read request queue {}: {e}", queue.display()),
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("argus-toggle-{}", uuid::Uuid::new_v4())))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A second launch's request to show the window must not toggle the
    /// overlay, nor the other way round.
    #[test]
    fn show_window_and_overlay_requests_are_separate() {
        let dir = TempDir::new();
        request_show_window(&dir.0).unwrap();
        request(&dir.0).unwrap();
        assert_eq!(drain_show_window(&dir.0), 1);
        assert_eq!(drain_show_window(&dir.0), 0);
        assert_eq!(drain(&dir.0), 1);
    }

    #[test]
    fn concurrent_requests_are_consumed_once_each() {
        let dir = TempDir::new();
        std::thread::scope(|scope| {
            for _ in 0..32 {
                scope.spawn(|| request(&dir.0).unwrap());
            }
        });
        assert_eq!(drain(&dir.0), 32);
        assert_eq!(drain(&dir.0), 0);
    }

    #[test]
    fn failed_removal_does_not_toggle_and_legacy_requests_work() {
        let dir = TempDir::new();
        let legacy = dir.0.join("toggle_overlay");
        fs::create_dir_all(&legacy).unwrap();
        assert_eq!(drain(&dir.0), 0);
        assert_eq!(drain(&dir.0), 0);
        fs::remove_dir(&legacy).unwrap();
        fs::write(&legacy, "").unwrap();
        assert_eq!(drain(&dir.0), 1);
        assert_eq!(drain(&dir.0), 0);
    }

    #[test]
    fn burst_is_drained_over_multiple_ticks() {
        let dir = TempDir::new();
        for _ in 0..259 {
            request(&dir.0).unwrap();
        }
        assert_eq!(drain(&dir.0), 256);
        assert_eq!(drain(&dir.0), 3);
        assert_eq!(drain(&dir.0), 0);
    }
}
