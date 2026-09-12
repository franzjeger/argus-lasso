//! Bounded asynchronous per-present recording. Disk I/O and sorting never run
//! on the presentation thread. Separate files for each swapchain lifetime.
use argus_ipc::capture::{self, Control, Summary};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
struct Sample {
    ticket: u64,
    at: Instant,
    swapchain: u64,
    result: i32,
}
enum Event {
    Frame(Sample),
    End(u64, u64),
}
pub struct Recorder {
    tx: SyncSender<Event>,
    active: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}
static RECORDER: OnceLock<Recorder> = OnceLock::new();
pub fn init() {
    RECORDER.get_or_init(Recorder::new);
}
pub fn ticket() -> u64 {
    RECORDER
        .get()
        .map(|r| r.active.load(Ordering::Acquire))
        .unwrap_or(0)
}
pub fn enabled() -> bool {
    ticket() != 0
}
pub fn record(ticket: u64, at: Instant, swapchain: u64, result: i32) {
    if let Some(r) = RECORDER.get() {
        if ticket != 0
            && r.active.load(Ordering::Acquire) == ticket
            && r.tx
                .try_send(Event::Frame(Sample {
                    ticket,
                    at,
                    swapchain,
                    result,
                }))
                .is_err()
        {
            r.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
pub fn end(swapchain: u64) {
    if let Some(r) = RECORDER.get() {
        let ticket = r.active.load(Ordering::Acquire);
        if ticket != 0 && r.tx.try_send(Event::End(ticket, swapchain)).is_err() {
            r.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl Recorder {
    fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(8192);
        let active = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let a = active.clone();
        let d = dropped.clone();
        std::thread::spawn(move || {
            let mut control = Control::default();
            let mut streams = HashMap::<u64, Stream>::new();
            let mut generation = 0u64;
            let mut checked = Instant::now() - Duration::from_secs(1);
            let mut began = Instant::now();
            let mut sequence = 0u32;
            loop {
                if checked.elapsed() >= Duration::from_millis(100) {
                    checked = Instant::now();
                    let next = capture::read_control();
                    let continuing = control.active
                        && next.is_active()
                        && next.session == control.session
                        && began.elapsed()
                            < Duration::from_secs(control.duration_seconds.clamp(5, 600) as u64);
                    if !continuing {
                        a.store(0, Ordering::Release);
                        // Drain already accepted samples before closing the capture.
                        while let Ok(event) = rx.try_recv() {
                            if apply(event, &mut streams, &control, generation, &mut sequence, &d)
                                .is_err()
                            {
                                d.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        for (_, stream) in streams.drain() {
                            finish(stream, d.load(Ordering::Relaxed));
                        }
                        control.active = false;
                        if next.is_active() && next.session != control.session {
                            control = next;
                            began = Instant::now();
                            generation = generation.wrapping_add(1).max(1);
                            d.store(0, Ordering::Relaxed);
                            a.store(generation, Ordering::Release);
                        }
                    }
                }
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(event) => {
                        if let Err(e) =
                            apply(event, &mut streams, &control, generation, &mut sequence, &d)
                        {
                            a.store(0, Ordering::Release);
                            control.active = false;
                            d.fetch_add(1, Ordering::Relaxed);
                            report_error(&e.to_string());
                            for (_, stream) in streams.drain() {
                                finish(stream, d.load(Ordering::Relaxed));
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Self {
            tx,
            active,
            dropped,
        }
    }
}
struct Stream {
    writer: BufWriter<std::fs::File>,
    path: PathBuf,
    summary: Summary,
    first: Instant,
    last: Option<Instant>,
    intervals: Vec<u64>,
    io_failed: bool,
}
impl Stream {
    fn new(sample: &Sample, control: &Control, sequence: u32) -> io::Result<Self> {
        let dir = capture::directory();
        fs::create_dir_all(&dir)?;
        let name = format!(
            "{}-{}-{:x}-{sequence}",
            control.session,
            std::process::id(),
            sample.swapchain
        );
        let path = dir.join(name);
        let csv = path.with_extension("csv.partial");
        let mut writer = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(csv)?,
        );
        writeln!(writer, "present_begin_ns,interval_ns,vulkan_result")?;
        let executable = fs::read_link("/proc/self/exe")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let telemetry = crate::TELEMETRY.read().ok().and_then(|t| t.frame.clone());
        let config = crate::OVERLAY_CONFIG.read().ok().map(|c| c.clone());
        use ash::vk::Handle;
        let extent = crate::OVERLAY_STATES.lock().ok().and_then(|states| {
            states
                .get(&ash::vk::SwapchainKHR::from_raw(sample.swapchain))
                .map(|s| [s.extent.width, s.extent.height])
        });
        let metadata = serde_json::json!({"schema":1,"sampled_unix_ms":capture::now_ms(),"protocol":argus_ipc::PROTOCOL_VERSION,"telemetry_at_start":telemetry,"overlay_config":config,"swapchain_extent":extent,"clock":"std::time::Instant monotonic","warmup":"user-controlled before recording","scene":"not automatically known"});
        fs::write(
            path.with_extension("metadata.json"),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        let summary=Summary{schema:1,metric:"CPU intervals between successful vkQueuePresentKHR entry timestamps; not GPU time or displayed/FG FPS".into(),build:argus_ipc::BUILD_ID.into(),session:control.session.clone(),pid:std::process::id(),executable,swapchain:sample.swapchain,..Default::default()};
        Ok(Self {
            writer,
            path,
            summary,
            first: sample.at,
            last: None,
            intervals: Vec::new(),
            io_failed: false,
        })
    }
    fn frame(&mut self, s: Sample) {
        if s.result != 0 && s.result != 1_000_001_003 {
            self.summary.failed_presents += 1;
            self.last = None;
            return;
        }
        if let Some(last) = self.last {
            let ns = s.at.saturating_duration_since(last).as_nanos() as u64;
            if ns > 0 {
                // Ten-minute captures at extreme presentation rates remain bounded.
                if self.intervals.len() >= 2_000_000 {
                    self.io_failed = true;
                    return;
                }
                self.intervals.push(ns);
                if writeln!(
                    self.writer,
                    "{},{},{}",
                    s.at.saturating_duration_since(self.first).as_nanos(),
                    ns,
                    s.result
                )
                .is_err()
                {
                    self.io_failed = true;
                }
            }
        }
        self.last = Some(s.at);
    }
}
fn apply(
    event: Event,
    streams: &mut HashMap<u64, Stream>,
    control: &Control,
    generation: u64,
    sequence: &mut u32,
    dropped: &AtomicU64,
) -> io::Result<()> {
    if !control.active {
        return Ok(());
    }
    match event {
        Event::End(ticket, id) if ticket == generation => {
            if let Some(stream) = streams.remove(&id) {
                finish(stream, dropped.load(Ordering::Relaxed));
            }
        }
        Event::Frame(sample) if sample.ticket == generation => {
            if !streams.contains_key(&sample.swapchain) {
                if streams.len() >= 16 {
                    return Err(io::Error::other(
                        "Too many concurrent swapchains; capture stopped",
                    ));
                }
                *sequence += 1;
                streams.insert(sample.swapchain, Stream::new(&sample, control, *sequence)?);
            }
            if let Some(stream) = streams.get_mut(&sample.swapchain) {
                stream.frame(sample);
                if stream.io_failed {
                    return Err(io::Error::other(
                        "Capture write failed or two-million-sample limit reached",
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn report_error(message: &str) {
    eprintln!("[Argus capture] {message}");
    let _ = fs::write(capture::directory().join("latest-error.txt"), message);
}
fn finish(mut stream: Stream, dropped: u64) {
    let (avg, low, p99) = capture::statistics(&mut stream.intervals);
    stream.summary.frames = stream.intervals.len();
    stream.summary.duration_seconds = stream.intervals.iter().map(|v| *v as f64 / 1e9).sum();
    stream.summary.average_fps = avg;
    stream.summary.low_1_fps = low;
    stream.summary.p99_frametime_ms = p99;
    stream.summary.dropped_samples = dropped;
    stream.summary.complete = !stream.io_failed
        && dropped == 0
        && stream.summary.failed_presents == 0
        && stream.summary.frames > 0;
    if stream.writer.flush().is_err() {
        stream.summary.complete = false;
    }
    let result = (|| -> io::Result<()> {
        if stream.summary.complete {
            fs::rename(
                stream.path.with_extension("csv.partial"),
                stream.path.with_extension("csv"),
            )?;
        }
        let tmp = stream.path.with_extension("summary.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&stream.summary)?)?;
        fs::rename(tmp, stream.path.with_extension("summary.json"))
    })();
    if let Err(e) = result {
        report_error(&format!("Finalization failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_or_old_generation_cannot_reopen_a_capture() {
        let mut streams = HashMap::new();
        let mut sequence = 0;
        let lost = AtomicU64::new(0);
        let mut control = Control::default();
        apply(
            Event::Frame(Sample {
                ticket: 1,
                at: Instant::now(),
                swapchain: 7,
                result: 0,
            }),
            &mut streams,
            &control,
            1,
            &mut sequence,
            &lost,
        )
        .unwrap();
        assert!(streams.is_empty());
        control.active = true;
        apply(
            Event::Frame(Sample {
                ticket: 1,
                at: Instant::now(),
                swapchain: 7,
                result: 0,
            }),
            &mut streams,
            &control,
            2,
            &mut sequence,
            &lost,
        )
        .unwrap();
        assert!(streams.is_empty());
    }
}
