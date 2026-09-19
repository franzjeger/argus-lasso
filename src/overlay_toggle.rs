//! File-backed requests for the CLI, consumed by the single monitor instance.
//! Each empty file is a complete request; create_new publishes it atomically.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

const QUEUE_DIR: &str = "overlay-toggle-requests";

pub fn request(config_dir: &Path) -> io::Result<()> {
    let queue = config_dir.join(QUEUE_DIR);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&queue)?;
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
            log::warn!("Could not consume overlay toggle {}: {e}", path.display());
            false
        }
    }
}

/// Only successfully removed requests count. Failed removals must not toggle
/// repeatedly. Requests arriving during a scan may wait until the next tick.
pub fn drain(config_dir: &Path) -> usize {
    // Consume requests left by the previous CLI version as well.
    let mut count = usize::from(consume(&config_dir.join("toggle_overlay")));
    match fs::read_dir(config_dir.join(QUEUE_DIR)) {
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
                    Err(e) => log::warn!("Could not read overlay toggle request: {e}"),
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Could not read overlay toggle queue: {e}"),
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
