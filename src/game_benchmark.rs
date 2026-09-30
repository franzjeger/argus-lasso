//! Game capture controls and desktop-authorized global shortcut registration.

use crate::gui::theme::{self, tokens};
use argus_ipc::capture::{self, Summary};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
pub struct GameBenchmark {
    duration: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    shortcut_live: Arc<AtomicBool>,
    rx: Option<mpsc::Receiver<String>>,
    status: String,
    checked: Option<Instant>,
    active: bool,
    error: String,
    results: Vec<(PathBuf, Summary)>,
    scan: Option<mpsc::Receiver<RecordingScan>>,
    compare: [Option<PathBuf>; 2],
    comparison_dirty: bool,
    graphs: [Vec<(f64, f64)>; 2],
    graph_job: Option<mpsc::Receiver<GraphResult>>,
    graph_error: String,
}
impl Default for GameBenchmark {
    fn default() -> Self {
        Self {
            duration: Arc::new(AtomicU32::new(60)),
            stop: Arc::new(AtomicBool::new(false)),
            shortcut_live: Arc::new(AtomicBool::new(false)),
            rx: None,
            status: String::new(),
            checked: None,
            active: false,
            error: String::new(),
            results: Vec::new(),
            scan: None,
            compare: [None, None],
            comparison_dirty: false,
            graphs: Default::default(),
            graph_job: None,
            graph_error: String::new(),
        }
    }
}
impl GameBenchmark {
    /// Compare the two latest recordings: A the earlier, B the later, so
    /// "Change B vs A" reads as what changed since.
    pub fn compare_recent(&mut self) -> bool {
        let Some([earlier, later]) = latest_two_runs(&self.results) else {
            return false;
        };
        let next = [Some(earlier.clone()), Some(later.clone())];
        if self.compare != next {
            self.compare = next;
            self.comparison_dirty = true;
        }
        true
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.rx {
            while let Ok(message) = rx.try_recv() {
                self.status = message;
                self.checked = None;
            }
        }
        if let Some(scan) = &self.scan {
            match scan.try_recv() {
                Ok(data) => {
                    self.active = data.active;
                    self.results = data.results;
                    self.error = data.error;
                    self.scan = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.error = "Could not read recordings.".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.scan.is_none()
            && self
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            let (tx, rx) = mpsc::channel();
            self.scan = Some(rx);
            self.checked = Some(Instant::now());
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let _ = tx.send(RecordingScan {
                    active: capture::read_control().is_active(),
                    results: capture::load_summaries(),
                    error: read_bounded_text(&capture::directory().join("latest-error.txt")),
                });
                ctx.request_repaint();
            });
        }
        if let Some(job) = &self.graph_job {
            match job.try_recv() {
                Ok(result) => {
                    self.graph_job = None;
                    if result.selection == self.compare {
                        match result.data {
                            Ok(data) => {
                                self.graphs = data;
                                self.graph_error.clear();
                            }
                            Err(e) => self.graph_error = e,
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.graph_job = None;
                    self.graph_error = "Graph loading failed.".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.heading("Game benchmark recording");
        theme::help_text(ui, "Record game performance for comparison. Results are saved locally as CSV and a summary.");
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if self.active {
                    "Stop recording"
                } else {
                    "Start recording"
                })
                .clicked()
            {
                match capture::set_active(!self.active, self.duration.load(Ordering::Relaxed)) {
                    Ok(c) => {
                        self.active = c.active;
                        self.checked = None;
                        self.status = if c.active {
                            "Recording requested — look for REC in the game"
                        } else {
                            "Recording stopped; results appear after finalization"
                        }
                        .into();
                    }
                    Err(e) => self.status = e.to_string(),
                }
            }
            let mut duration = self.duration.load(Ordering::Relaxed);
            ui.label("Automatic stop (seconds)");
            if ui
                .add(egui::DragValue::new(&mut duration).range(5..=600))
                .changed()
            {
                self.duration.store(duration, Ordering::Relaxed);
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(theme::bold(ui, "Keyboard shortcut", tokens::FONT_HEADING));
        ui.horizontal_wrapped(|ui| {
            let registered = self.shortcut_live.load(Ordering::Relaxed);
            if ui
                .add_enabled(!registered, egui::Button::new("Set up recording shortcut…"))
                .clicked()
            {
                self.register();
            }
            if ui
                .add_enabled(registered, egui::Button::new("Disable shortcut"))
                .clicked()
            {
                self.stop.store(true, Ordering::Relaxed);
            }
        });
        theme::help_text(
            ui,
            "Suggested: Shift+F2. Your desktop confirms the actual shortcut.",
        );
        if !self.error.is_empty() {
            ui.colored_label(theme::sem(ui).negative, &self.error);
        }
        if !self.status.is_empty() {
            ui.label(self.status());
        }
        ui.add_space(12.0);
        egui::CollapsingHeader::new("Measurement details & files").show(ui, |ui| {
        theme::help_text(ui, "CPU present intervals, not GPU time or displayed / generated frames. 1% low = reciprocal of the mean slowest 1%; p99 = nearest-rank frametime. No data is uploaded.");

            ui.label("Every Vulkan present interval is recorded. Each application and swapchain has a separate result.");
            ui.monospace(capture::directory().display().to_string());
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(theme::bold(ui, "Recent recordings", tokens::FONT_HEADING));
        if self.results.is_empty() {
            ui.weak("Your completed recordings will appear here.");
        }
        if ui
            .add_enabled(
                latest_two_runs(&self.results).is_some(),
                egui::Button::new("Compare latest two"),
            )
            .clicked()
        {
            self.compare_recent();
        }
        let mut selection_changed = std::mem::take(&mut self.comparison_dirty);
        for (path, result) in &self.results {
            egui::CollapsingHeader::new(recording_label(result)).id_salt(path).show(ui, |ui| {
                ui.label(format!("Average {} FPS · 1% low {} FPS · p99 {} ms", metric(result.average_fps), metric(result.low_1_fps), metric(result.p99_frametime_ms)));
                ui.label(format!("{} frames · PID {} · swapchain {:x} · lost samples {} · failed presents {}",
                    result.frames, result.pid, result.swapchain, result.dropped_samples, result.failed_presents));
                ui.label(&result.executable);
                theme::help_text(ui, &result.metric);
                ui.horizontal_wrapped(|ui| {
                    for (i, label) in ["Use as A", "Use as B"].iter().enumerate() {
                        if ui.selectable_label(self.compare[i].as_ref() == Some(path), *label).clicked() {
                            self.compare[i] = Some(path.clone());
                            selection_changed = true;
                        }
                    }
                    if ui.button("Open recording folder").clicked() {
                        let _ = std::process::Command::new("xdg-open").arg(capture::directory()).spawn();
                    }
                });
            });
        }
        if selection_changed {
            self.graphs = Default::default();
            self.graph_error.clear();
            let selection = self.compare.clone();
            let summaries: Vec<_> = selection
                .iter()
                .map(|p| {
                    self.results
                        .iter()
                        .find(|(path, _)| Some(path) == p.as_ref())
                        .cloned()
                })
                .collect();
            let (tx, rx) = mpsc::channel();
            self.graph_job = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let data = (|| {
                    let mut graphs: Graphs = Default::default();
                    for (i, entry) in summaries.iter().enumerate() {
                        if let Some((path, summary)) = entry {
                            graphs[i] = read_graph(path, summary.duration_seconds)?;
                        }
                    }
                    Ok(graphs)
                })();
                let _ = tx.send(GraphResult { selection, data });
                ctx.request_repaint();
            });
        }
        if self.compare.iter().any(Option::is_some) {
            ui.separator();
            ui.heading("Compare recordings");
            theme::help_text(ui, "Compare the same scene and settings. Different durations or incomplete recordings can bias results. FPS here describes CPU present intervals.");
            let selected: Vec<_> = self
                .compare
                .iter()
                .map(|p| {
                    self.results
                        .iter()
                        .find(|(path, _)| Some(path) == p.as_ref())
                        .map(|(_, s)| s)
                })
                .collect();
            for (i, summary) in selected.iter().enumerate() {
                ui.label(format!(
                    "{}: {}",
                    if i == 0 { "A" } else { "B" },
                    summary.map_or("Choose a recording above".into(), recording_label)
                ));
            }
            if let [Some(a), Some(b)] = selected.as_slice() {
                egui::Grid::new("recording_metrics")
                    .min_col_width(110.0)
                    .striped(true)
                    .show(ui, |ui| {
                        for label in ["Metric", "A", "B", "Change B vs A"] {
                            ui.strong(label);
                        }
                        ui.end_row();
                        for (name, av, bv) in [
                            ("Average FPS ↑", a.average_fps, b.average_fps),
                            ("1% low FPS ↑", a.low_1_fps, b.low_1_fps),
                            (
                                "p99 frametime (ms) ↓",
                                a.p99_frametime_ms,
                                b.p99_frametime_ms,
                            ),
                        ] {
                            ui.label(name);
                            ui.label(metric(av));
                            ui.label(metric(bv));
                            ui.label(delta(av, bv));
                            ui.end_row();
                        }
                    });
            }
            if self.graph_job.is_some() {
                ui.label("Loading frametimes…");
            }
            if !self.graph_error.is_empty() {
                ui.colored_label(theme::sem(ui).negative, &self.graph_error);
            }
            draw_comparison(ui, &self.graphs);
        }
    }
    pub fn register(&mut self) {
        if self.shortcut_live.swap(true, Ordering::Relaxed) {
            return;
        }
        self.stop.store(false, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = "Waiting for desktop shortcut permission…".into();
        let stop = self.stop.clone();
        let live = self.shortcut_live.clone();
        let duration = self.duration.clone();
        std::thread::spawn(move || {
            if let Err(e) = async_io::block_on(shortcut_loop(stop, duration, tx.clone())) {
                let _ = tx.send(format!(
                    "Shortcut unavailable: {e}. Recording buttons and CLI remain available."
                ));
            }
            live.store(false, Ordering::Relaxed);
        });
    }
}
async fn shortcut_loop(
    stop: Arc<AtomicBool>,
    duration: Arc<AtomicU32>,
    tx: mpsc::Sender<String>,
) -> Result<(), String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::{
        future::{select, Either},
        StreamExt,
    };
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if !REGISTERED.load(Ordering::Acquire) {
        ashpd::register_host_app(
            "io.github.franzjeger.ArgusLasso"
                .parse()
                .map_err(|e: ashpd::Error| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
        REGISTERED.store(true, Ordering::Release);
    }
    let proxy = GlobalShortcuts::new().await.map_err(|e| e.to_string())?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|e| e.to_string())?;
    let session_path = serde_json::to_value(&session)
        .map_err(|e| e.to_string())?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let events = proxy.receive_activated().await.map_err(|e| e.to_string())?;
    futures_util::pin_mut!(events);
    let binding = proxy
        .bind_shortcuts(
            &session,
            &[
                NewShortcut::new("record-benchmark", "Start / stop Argus game benchmark")
                    .preferred_trigger("SHIFT+F2"),
            ],
            None,
            Default::default(),
        )
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    let actual = binding
        .shortcuts()
        .iter()
        .find(|s| s.id() == "record-benchmark")
        .map(|s| s.trigger_description())
        .ok_or("The desktop did not bind the recording shortcut")?;
    let _ = tx.send(format!("Recording shortcut: {actual}"));
    let mut last = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        match select(
            events.next(),
            async_io::Timer::after(Duration::from_millis(200)),
        )
        .await
        {
            Either::Left((Some(event), _))
                if event.shortcut_id() == "record-benchmark"
                    && event.session_handle().as_str() == session_path
                    && last.elapsed() > Duration::from_millis(300) =>
            {
                last = Instant::now();
                let message = match capture::toggle(duration.load(Ordering::Relaxed)) {
                    Ok(c) => {
                        if c.active {
                            "Recording started".into()
                        } else {
                            "Recording stopped".into()
                        }
                    }
                    Err(e) => e.to_string(),
                };
                let _ = tx.send(message);
            }
            Either::Left((None, _)) => break,
            _ => {}
        }
    }
    session.close().await.map_err(|e| e.to_string())?;
    let _ = tx.send("Recording shortcut released".into());
    Ok(())
}

/// Read at most `MAX_ERROR_FILE_BYTES` of a text file, on failure or an
/// oversized file returning an empty string rather than the whole content.
///
/// This file is written by the in-process recorder running inside the game
/// (argus-layer), the less-trusted side of this boundary — capping the read
/// itself (not just the size checked afterward) means a huge or hostile file
/// costs at most one bounded allocation, not an attempt to load it whole.
const MAX_ERROR_FILE_BYTES: u64 = 64 * 1024;

fn read_bounded_text(path: &std::path::Path) -> String {
    match capture::read_regular_capped(path, MAX_ERROR_FILE_BYTES) {
        Ok(Some(buf)) => String::from_utf8_lossy(&buf).into_owned(),
        _ => String::new(),
    }
}

struct RecordingScan {
    active: bool,
    results: Vec<(PathBuf, Summary)>,
    error: String,
}
type Graphs = [Vec<(f64, f64)>; 2];
struct GraphResult {
    selection: [Option<PathBuf>; 2],
    data: Result<Graphs, String>,
}
fn metric(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite())
        .map_or("—".into(), |v| format!("{v:.2}"))
}
fn delta(a: Option<f64>, b: Option<f64>) -> String {
    match a.zip(b) {
        Some((a, b)) if a > 0.0 && a.is_finite() && b.is_finite() => {
            format!("{:+.1}%", (b / a - 1.0) * 100.0)
        }
        _ => "—".into(),
    }
}
/// The main result of each of the two latest recording sessions, earlier
/// first. One session writes a result per swapchain, so a game that
/// recreates its swapchain (resize, fullscreen, settings) leaves several,
/// and one that presented once leaves an empty one; the run is the one with
/// the most frames. `results` is newest first.
fn latest_two_runs(results: &[(PathBuf, Summary)]) -> Option<[&PathBuf; 2]> {
    let mut runs: Vec<(&PathBuf, &Summary)> = Vec::new();
    for (path, summary) in results.iter().filter(|(_, s)| s.frames > 0) {
        match runs
            .iter_mut()
            .find(|(_, run)| run.session == summary.session)
        {
            Some(run) if summary.frames > run.1.frames => *run = (path, summary),
            Some(_) => {}
            None => runs.push((path, summary)),
        }
    }
    match runs.as_slice() {
        [later, earlier, ..] => Some([earlier.0, later.0]),
        _ => None,
    }
}

fn recording_label(s: &Summary) -> String {
    let name = match capture::program_file_name(&s.executable) {
        "" => "Unknown application",
        name => name,
    };
    let date = s
        .session
        .split('-')
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(|ms| {
            let seconds = nix::libc::time_t::try_from(ms / 1000).ok()?;
            // SAFETY: localtime_r writes into the provided initialized tm.
            let mut tm = unsafe { std::mem::zeroed::<nix::libc::tm>() };
            if unsafe { nix::libc::localtime_r(&seconds, &mut tm) }.is_null() {
                return None;
            }
            Some(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            ))
        })
        .unwrap_or_else(|| "Unknown time".into());
    format!(
        "{name} · {date} · {:.1} s · {}",
        s.duration_seconds,
        if s.complete { "Complete" } else { "Incomplete" }
    )
}

/// Stream bounded CSV input and retain the maximum in each time bin so stalls
/// survive reduction. The graph is explicitly labelled as peak-per-bin data.
fn read_graph(summary: &std::path::Path, duration: f64) -> Result<Vec<(f64, f64)>, String> {
    use std::io::{BufRead, Read};
    if !duration.is_finite() || duration <= 0.0 {
        return Err("Recording has no valid duration.".into());
    }
    let name = summary
        .file_name()
        .and_then(|p| p.to_str())
        .and_then(|p| p.strip_suffix(".summary.json"))
        .ok_or("Invalid recording filename")?;
    let path = summary.with_file_name(format!("{name}.csv"));
    let file = capture::open_regular(&path)
        .or_else(|_| capture::open_regular(&path.with_extension("csv.partial")))
        .map_err(|e| format!("Could not read frametimes: {e}"))?;
    const LIMIT: u64 = 128 * 1024 * 1024;
    if file.metadata().map_err(|e| e.to_string())?.len() > LIMIT {
        return Err("Recording CSV exceeds 128 MiB.".into());
    }
    let mut reader = std::io::BufReader::new(file.take(LIMIT + 1));
    let mut line = String::new();
    let mut bins = PeakBins::new(duration / GRAPH_BINS as f64);
    let mut total = 0;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take(4097)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if n > 4096 || total > LIMIT {
            return Err("Recording CSV exceeds read limits.".into());
        }
        if line.starts_with("present_begin_ns,") {
            continue;
        }
        let mut values = line.trim().split(',');
        let t = values
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("Invalid CSV time")? as f64
            / 1e9;
        let ms = values
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("Invalid CSV interval")? as f64
            / 1e6;
        bins.add(t, ms);
    }
    Ok(bins.into_points())
}

const GRAPH_BINS: usize = 1000;

/// The slowest frame in each of `GRAPH_BINS` equal time slots, over a
/// recording whose end is not known up front: the summary's duration counts
/// only accepted intervals, so a recording with failed presents runs past
/// it. A frame beyond the last slot merges neighbouring slots and doubles
/// their width instead of piling up in the last one.
struct PeakBins {
    width: f64,
    bins: Vec<Option<(f64, f64)>>,
}

impl PeakBins {
    fn new(width: f64) -> Self {
        Self {
            width,
            bins: vec![None; GRAPH_BINS],
        }
    }

    fn add(&mut self, t: f64, ms: f64) {
        let slot = |width: f64| (t / width) as usize;
        while slot(self.width) >= GRAPH_BINS {
            self.width *= 2.0;
            let merged: Vec<_> = self
                .bins
                .chunks(2)
                .map(|pair| {
                    pair.iter()
                        .flatten()
                        .copied()
                        .max_by(|a, b| a.1.total_cmp(&b.1))
                })
                .collect();
            self.bins = merged;
            self.bins.resize(GRAPH_BINS, None);
        }
        let bin = &mut self.bins[slot(self.width)];
        if bin.is_none_or(|(_, old)| ms > old) {
            *bin = Some((t, ms));
        }
    }

    fn into_points(self) -> Vec<(f64, f64)> {
        self.bins.into_iter().flatten().collect()
    }
}
fn draw_comparison(ui: &mut egui::Ui, graphs: &Graphs) {
    if graphs.iter().all(Vec::is_empty) {
        return;
    }
    let colors = [theme::sem(ui).accent, theme::sem(ui).warning];
    ui.horizontal(|ui| {
        ui.colored_label(colors[0], "A");
        ui.colored_label(colors[1], "B");
        ui.label("Frametime · peak per time bin");
    });
    let max_t = graphs
        .iter()
        .flatten()
        .map(|(t, _)| *t)
        .fold(1.0_f64, f64::max);
    let max_ms = graphs
        .iter()
        .flatten()
        .map(|(_, v)| *v)
        .fold(1.0_f64, f64::max);
    ui.label(format!(
        "0–{max_t:.1} s from capture start · 0–{max_ms:.1} ms"
    ));
    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), 160.0),
        egui::Sense::hover(),
    );
    let rect = response.rect.shrink(4.0);
    painter.rect_filled(rect, 0.0, theme::plot_fill(ui));
    for (i, graph) in graphs.iter().enumerate() {
        let points: Vec<_> = graph
            .iter()
            .map(|(t, ms)| {
                egui::pos2(
                    rect.left() + (*t / max_t) as f32 * rect.width(),
                    rect.bottom() - (*ms / max_ms) as f32 * rect.height(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5_f32, colors[i]),
        ));
    }
}

#[cfg(test)]
mod comparison_tests {
    use super::*;

    fn run(name: &str, session: &str, frames: usize) -> (PathBuf, Summary) {
        let summary = Summary {
            session: session.into(),
            frames,
            ..Summary::default()
        };
        (PathBuf::from(name), summary)
    }

    /// Frames after the summary's duration (which leaves out failed
    /// presents) used to collapse into the last slot of the graph.
    #[test]
    fn a_recording_longer_than_its_duration_keeps_its_shape() {
        let mut bins = PeakBins::new(1.0 / GRAPH_BINS as f64);
        for i in 0..3000 {
            let t = i as f64 / 1000.0; // three seconds for a one-second duration
            bins.add(t, if i == 2500 { 50.0 } else { 16.0 });
        }
        let points = bins.into_points();
        assert!(points.len() > GRAPH_BINS / 2, "{} points", points.len());
        assert!(points.last().unwrap().0 > 2.9);
        assert!(
            points
                .iter()
                .any(|&(t, ms)| ms == 50.0 && (t - 2.5).abs() < 1e-9),
            "the spike survives"
        );
        assert!(points.windows(2).all(|w| w[0].0 < w[1].0));
    }

    /// A swapchain recreated during the latest recording used to be
    /// compared with the rest of that same recording, newest as A.
    #[test]
    fn the_latest_two_runs_are_two_sessions_earlier_first() {
        // Newest first, as load_summaries lists them.
        let results = [
            run("s2-swapchain-b", "s2", 900),
            run("s2-swapchain-a", "s2", 3000),
            run("s2-one-present", "s2", 0),
            run("s1-swapchain", "s1", 2800),
            run("s0-swapchain", "s0", 2500),
        ];
        let [a, b] = latest_two_runs(&results).unwrap();
        assert_eq!(a, &PathBuf::from("s1-swapchain"));
        assert_eq!(b, &PathBuf::from("s2-swapchain-a"));

        assert!(latest_two_runs(&results[..3]).is_none(), "one session only");
    }
    #[test]
    fn differences_handle_missing_zero_and_nonfinite_metrics() {
        assert_eq!(delta(Some(100.0), Some(110.0)), "+10.0%");
        assert_eq!(delta(Some(0.0), Some(110.0)), "—");
        assert_eq!(metric(Some(f64::NAN)), "—");
    }
    #[test]
    fn graph_preserves_stalls_and_reads_partial_captures() {
        let dir = std::env::temp_dir().join(format!("argus-graph-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("sample.csv.partial"), "present_begin_ns,interval_ns,vulkan_result\n1000000,1000000,0\n2000000,50000000,0\n3000000,2000000,0\n").unwrap();
        let points = read_graph(&dir.join("sample.summary.json"), 60.0).unwrap();
        assert_eq!(points, [(0.002, 50.0)]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
