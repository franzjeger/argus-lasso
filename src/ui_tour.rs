//! Scripted walk through every screen, capturing each one to a PNG.
//!
//! Exists because the screenshots in the README go stale every time the UI
//! moves, and re-taking them by hand is both tedious and inconsistent —
//! different window sizes, different amounts of live data, a stray cursor in
//! one of them.
//!
//! Capture goes through egui's own `ViewportCommand::Screenshot` rather than
//! an external tool, so it needs no compositor cooperation and no synthetic
//! input: on Wayland those are exactly the parts that do not work. What lands
//! in the file is the framebuffer, so there are no window shadows, no
//! decorations and no pointer.
//!
//! Not covered: the affinity, nice and I/O-priority dialogs. Those open as
//! separate OS windows via `show_viewport_immediate`, and eframe's glow
//! backend does not deliver `Event::Screenshot` for an immediate child
//! viewport — a capture addressed to one simply never replies. They have to
//! be captured by hand until that changes.

use std::path::{Path, PathBuf};

/// One screen to visit. The app maps these onto its own state; keeping the
/// list here means adding a screen is a one-line change in one place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Overview,
    Processes,
    Rules,
    ProBalance,
    GamingMode,
    HwMonitor,
    Benchmark,
    Log,
    Settings,
    GamingLauncher,
    GamingOverlay,
    GamingRecording,
    GamingSensors,
    SettingsProcesses,
    SettingsPower,
    SettingsNotifications,
    SettingsStartup,
    ProcessDetails,
    KillToast,
    RuleOffer,
}

impl Step {
    /// Filename stem, prefixed with the visit order so a directory listing
    /// reads in tour order rather than alphabetically.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Processes => "processes",
            Self::Rules => "rules",
            Self::ProBalance => "probalance",
            Self::GamingMode => "gaming-mode",
            Self::HwMonitor => "hw-monitor",
            Self::Benchmark => "benchmark",
            Self::Log => "log",
            Self::Settings => "settings",
            Self::GamingLauncher => "gaminglauncher",
            Self::GamingOverlay => "gamingoverlay",
            Self::GamingRecording => "gamingrecording",
            Self::GamingSensors => "gamingsensors",
            Self::SettingsProcesses => "settingsprocesses",
            Self::SettingsPower => "settingspower",
            Self::SettingsNotifications => "settingsnotifications",
            Self::SettingsStartup => "settingsstartup",
            Self::ProcessDetails => "dialog-process-details",
            Self::KillToast => "toast-kill-undo",
            Self::RuleOffer => "toast-rule-offer",
        }
    }
}

pub const STEPS: &[Step] = &[
    Step::Overview,
    Step::Processes,
    Step::Rules,
    Step::ProBalance,
    Step::GamingMode,
    Step::HwMonitor,
    Step::Benchmark,
    Step::Log,
    Step::Settings,
    Step::GamingLauncher,
    Step::GamingOverlay,
    Step::GamingRecording,
    Step::GamingSensors,
    Step::SettingsProcesses,
    Step::SettingsPower,
    Step::SettingsNotifications,
    Step::SettingsStartup,
    Step::ProcessDetails,
    Step::KillToast,
    Step::RuleOffer,
];

/// Frames to render before capturing a step.
///
/// One frame is not enough: egui is immediate-mode, so a screen that sizes
/// itself from what it measured last frame (tables, the plots) is still
/// settling. A newly opened dialog viewport also needs a frame or two before
/// the compositor has it mapped and painted.
const SETTLE_FRAMES: u32 = 10;

/// Frames to wait for a capture before giving up on that screen.
///
/// A step whose reply never arrives must not wedge the whole tour: without
/// this the run hangs forever and produces a partial directory with no
/// explanation of where it stopped.
const CAPTURE_TIMEOUT_FRAMES: u32 = 240;

/// Frames to wait for the daemon's first snapshot before giving up on it.
///
/// Bounded on purpose: an unbounded wait turned a machine where the snapshot
/// never arrived into a tour that hung with an empty output directory and no
/// explanation.
const WARMUP_TIMEOUT_FRAMES: u32 = 600;

pub struct Tour {
    dir: PathBuf,
    idx: usize,
    settle: u32,
    /// True once the screenshot for the current step has been requested and
    /// we are waiting for egui to hand the pixels back.
    awaiting: bool,
    /// Set once the daemon has published its first snapshot; from then on we
    /// never wait again, so a step that legitimately shows no processes
    /// cannot stall the tour.
    warmed_up: bool,
    /// Frames spent waiting for the current capture.
    waited: u32,
    /// Frames spent waiting for the daemon's first snapshot.
    warmup_waited: u32,
    pub failures: Vec<String>,
}

impl Tour {
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        Ok(Self {
            dir,
            idx: 0,
            settle: SETTLE_FRAMES,
            awaiting: false,
            warmed_up: false,
            waited: 0,
            warmup_waited: 0,
            failures: Vec::new(),
        })
    }

    /// The screen the app should be showing right now, or `None` when the
    /// tour is over.
    pub fn current(&self) -> Option<Step> {
        STEPS.get(self.idx).copied()
    }

    /// Drive one frame. Returns true when the tour has finished and the app
    /// should exit.
    ///
    /// `has_data` gates the very first capture on the daemon having published
    /// a snapshot. Its first CPU figures need two samples an interval apart,
    /// so a tour running at full repaint speed otherwise photographs every
    /// screen before any data exists — an empty process table and
    /// "Processes: 0" in the status bar, which is precisely what these
    /// screenshots are supposed to show.
    pub fn tick(&mut self, ctx: &egui::Context, has_data: bool) -> bool {
        if self.current().is_none() {
            return true;
        }

        // Immediate mode only redraws on demand; a tour that waited for the
        // normal repaint cadence would take minutes. This has to be the
        // unconditional form — request_repaint_after() let the app fall back
        // to its idle cadence and the tour stopped advancing altogether.
        ctx.request_repaint();

        if !self.warmed_up {
            if has_data {
                self.warmed_up = true;
            } else {
                self.warmup_waited += 1;
                if self.warmup_waited < WARMUP_TIMEOUT_FRAMES {
                    return false;
                }
                self.failures
                    .push("daemon published no snapshot; screens may be empty".into());
                self.warmed_up = true;
            }
        }

        let step = self.current().expect("checked above");

        if self.awaiting {
            if let Some(image) = take_screenshot_event(ctx) {
                let name = format!("{:02}-{}.png", self.idx + 1, step.slug());
                if let Err(e) = write_png(&self.dir.join(&name), &image) {
                    self.failures.push(format!("{name}: {e}"));
                } else {
                    log::info!("ui-tour: wrote {name}");
                }
                self.advance();
            } else {
                self.waited += 1;
                if self.waited >= CAPTURE_TIMEOUT_FRAMES {
                    self.failures
                        .push(format!("{}: no screenshot arrived; skipped", step.slug()));
                    self.advance();
                }
            }
            return self.current().is_none();
        }

        if self.settle > 0 {
            self.settle -= 1;
            return false;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        self.awaiting = true;
        false
    }

    fn advance(&mut self) {
        self.awaiting = false;
        self.waited = 0;
        self.settle = SETTLE_FRAMES;
        self.idx += 1;
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Pull this frame's screenshot reply out of the root viewport's events.
fn take_screenshot_event(ctx: &egui::Context) -> Option<std::sync::Arc<egui::ColorImage>> {
    ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Screenshot {
                image, viewport_id, ..
            } if *viewport_id == egui::ViewportId::ROOT => Some(image.clone()),
            _ => None,
        })
    })
}

fn write_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &image.pixels {
        rgba.extend_from_slice(&[px.r(), px.g(), px.b(), px.a()]);
    }

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| e.to_string())?
        .write_image_data(&rgba)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_a_unique_slug() {
        let mut seen = std::collections::HashSet::new();
        for step in STEPS {
            assert!(
                seen.insert(step.slug()),
                "duplicate slug: {} — the files would overwrite each other",
                step.slug()
            );
        }
    }

    #[test]
    fn steps_cover_the_whole_enum() {
        // A screen added to Step but forgotten in STEPS would silently never
        // be captured, and the gap is invisible in the output directory.
        let all = [
            Step::Overview,
            Step::Processes,
            Step::Rules,
            Step::ProBalance,
            Step::GamingMode,
            Step::HwMonitor,
            Step::Benchmark,
            Step::Log,
            Step::Settings,
            Step::GamingLauncher,
            Step::GamingOverlay,
            Step::GamingRecording,
            Step::GamingSensors,
            Step::SettingsProcesses,
            Step::SettingsPower,
            Step::SettingsNotifications,
            Step::SettingsStartup,
            Step::ProcessDetails,
            Step::KillToast,
            Step::RuleOffer,
        ];
        for step in all {
            assert!(STEPS.contains(&step), "{step:?} is missing from STEPS");
        }
    }

    #[test]
    fn png_round_trips_a_known_image() {
        let img = egui::ColorImage {
            size: [2, 1],
            source_size: egui::vec2(2.0, 1.0),
            pixels: vec![
                egui::Color32::from_rgba_premultiplied(255, 0, 0, 255),
                egui::Color32::from_rgba_premultiplied(0, 255, 0, 255),
            ],
        };
        let path = std::env::temp_dir().join(format!("argus-tour-test-{}.png", std::process::id()));
        write_png(&path, &img).expect("write");

        let decoder = png::Decoder::new(std::fs::File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);

        std::fs::remove_file(&path).ok();
    }
}
