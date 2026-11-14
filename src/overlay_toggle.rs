//! File-backed requests from the CLI and from a second launch, consumed by
//! the single running instance's monitor thread. A toggle or show-window
//! request is an empty file published atomically by create_new; a control
//! request carries one line and is published by rename.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

const QUEUE_DIR: &str = "overlay-toggle-requests";
/// A second launch asks the running instance to show its window.
const SHOW_WINDOW_DIR: &str = "show-window-requests";

/// What `argus-lasso gaming|probalance|profile` asks the running instance.
const CONTROL_DIR: &str = "control-requests";
/// Far more than any request needs; bounds what a drain will read.
const CONTROL_MAX_BYTES: u64 = 1024;

/// A command for the running instance, from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Gaming(bool),
    ProBalance(bool),
    /// Replace the active rules with this saved profile.
    Profile(String),
    /// Cap games' frame rate; 0 lifts the cap.
    FpsLimit(u32),
}

impl Control {
    fn encode(&self) -> String {
        let on_off = |on: &bool| if *on { "on" } else { "off" };
        match self {
            Control::Gaming(on) => format!("gaming {}", on_off(on)),
            Control::ProBalance(on) => format!("probalance {}", on_off(on)),
            Control::Profile(name) => format!("profile {name}"),
            Control::FpsLimit(fps) => format!("fps-limit {fps}"),
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let (verb, rest) = line.split_once(' ')?;
        let on_off = |word: &str| match word {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        match verb {
            "gaming" => on_off(rest).map(Control::Gaming),
            "probalance" => on_off(rest).map(Control::ProBalance),
            "fps-limit" => rest.parse().ok().map(Control::FpsLimit),
            // A profile name is free text and may hold spaces, but never a
            // line break: one request is one line.
            "profile" if !rest.is_empty() && !rest.contains(['\n', '\r']) => {
                Some(Control::Profile(rest.to_owned()))
            }
            _ => None,
        }
    }
}

pub fn request_control(config_dir: &Path, control: &Control) -> io::Result<()> {
    let queue = config_dir.join(CONTROL_DIR);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&queue)?;
    // Written under a name drains skip, then renamed: a drain never sees a
    // half-written request.
    let id = uuid::Uuid::new_v4();
    let staged = queue.join(format!(".{id}.tmp"));
    let publish = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(control.encode().as_bytes())?;
        fs::rename(&staged, queue.join(id.to_string()))
    };
    publish().inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })
}

/// Requests in the order they were made. Each is removed before it is
/// returned, so one that cannot be removed is not acted on twice.
pub fn drain_control(config_dir: &Path) -> Vec<Control> {
    let queue = config_dir.join(CONTROL_DIR);
    let entries = match fs::read_dir(&queue) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::warn!("Could not read request queue {}: {e}", queue.display());
            return Vec::new();
        }
    };
    let mut pending: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .take(256)
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    pending.sort();
    pending
        .into_iter()
        .filter_map(|(_, path)| {
            let mut line = String::new();
            let read = fs::File::open(&path)
                .and_then(|file| file.take(CONTROL_MAX_BYTES).read_to_string(&mut line));
            if !consume(&path) {
                return None;
            }
            let control = read.ok().and_then(|_| Control::parse(&line));
            if control.is_none() {
                log::warn!("Ignored a malformed control request: {line:?}");
            }
            control
        })
        .collect()
}

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
    fn control_requests_arrive_once_and_in_order() {
        let dir = TempDir::new();
        let sent = [
            Control::Gaming(true),
            Control::Profile("Work rules".into()),
            Control::ProBalance(false),
            Control::Gaming(false),
        ];
        for control in &sent {
            request_control(&dir.0, control).unwrap();
            // The queue is ordered by modification time.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert_eq!(drain_control(&dir.0), sent);
        assert!(drain_control(&dir.0).is_empty());
        // Nothing staged is left behind, and the other queues saw nothing.
        assert_eq!(fs::read_dir(dir.0.join(CONTROL_DIR)).unwrap().count(), 0);
        assert_eq!(drain(&dir.0), 0);
    }

    #[test]
    fn malformed_and_unfinished_control_requests_are_not_acted_on() {
        let dir = TempDir::new();
        let queue = dir.0.join(CONTROL_DIR);
        fs::create_dir_all(&queue).unwrap();
        for body in [
            "",
            "gaming",
            "gaming maybe",
            "profile ",
            "profile a\nb",
            "reboot now",
        ] {
            fs::write(queue.join(uuid::Uuid::new_v4().to_string()), body).unwrap();
        }
        // A request still being written keeps its staging name.
        let staged = queue.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&staged, "gaming on").unwrap();
        assert!(drain_control(&dir.0).is_empty());
        assert!(staged.exists());
        assert_eq!(fs::read_dir(&queue).unwrap().count(), 1);
    }

    #[test]
    fn control_requests_survive_their_own_encoding() {
        for control in [
            Control::Gaming(true),
            Control::Gaming(false),
            Control::ProBalance(true),
            Control::ProBalance(false),
            Control::Profile("with  two spaces".into()),
            Control::FpsLimit(0),
            Control::FpsLimit(144),
        ] {
            assert_eq!(Control::parse(&control.encode()), Some(control));
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
