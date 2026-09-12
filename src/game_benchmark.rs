//! Game capture controls and desktop-authorized global shortcut registration.
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
        }
    }
}
impl GameBenchmark {
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
        if self
            .checked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.active = capture::read_control().is_active();
            self.results = capture::load_summaries();
            self.error = std::fs::read_to_string(capture::directory().join("latest-error.txt"))
                .unwrap_or_default();
            self.checked = Some(Instant::now());
        }
        ui.heading("Game benchmark recording");
        ui.label("Record game performance for a repeatable comparison. Results are saved locally as CSV and a summary.");
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
                match capture::toggle(self.duration.load(Ordering::Relaxed)) {
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
        ui.label(egui::RichText::new("Keyboard shortcut").strong());
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
        ui.small("Suggested: Shift+F2. Your desktop confirms the actual shortcut.");
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
        }
        if !self.status.is_empty() {
            ui.label(self.status());
        }
        ui.add_space(12.0);
        egui::CollapsingHeader::new("Measurement details & files").show(ui, |ui| {
        ui.small("CPU present intervals, not GPU time or displayed / generated frames. 1% low = reciprocal of the mean slowest 1%; p99 = nearest-rank frametime. No data is uploaded.");

            ui.label("Every Vulkan present interval is recorded. Each application and swapchain has a separate result.");
            ui.monospace(capture::directory().display().to_string());
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Recent recordings").strong());
        if self.results.is_empty() {
            ui.weak("Your completed recordings will appear here.");
        }
        for (path, result) in &self.results {
            egui::CollapsingHeader::new(format!(
                "{} · PID {} · {} frames{}",
                result.session,
                result.pid,
                result.frames,
                if result.complete {
                    ""
                } else {
                    " · INCOMPLETE"
                }
            ))
            .id_salt(path)
            .show(ui, |ui| {
                let val =
                    |v: Option<f64>| v.map(|n| format!("{n:.2}")).unwrap_or_else(|| "—".into());
                ui.label(format!(
                    "Average {} FPS · 1% low {} FPS · p99 {} ms · {:.2} s",
                    val(result.average_fps),
                    val(result.low_1_fps),
                    val(result.p99_frametime_ms),
                    result.duration_seconds
                ));
                ui.label(format!(
                    "Lost samples: {} · failed presents: {}",
                    result.dropped_samples, result.failed_presents
                ));
                ui.label(&result.executable);
                ui.small(&result.metric);
                if ui.button("Open recording folder").clicked() {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(capture::directory())
                        .spawn();
                }
            });
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
