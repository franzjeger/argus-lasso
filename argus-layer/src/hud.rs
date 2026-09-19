//! Pixel-space HUD rasterization. RGBA is premultiplied; the Vulkan blend uses ONE.
use argus_ipc::{OverlayConfig, TelemetryFrame};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const ROW_SPACING: u32 = 3;
const DIVIDER_HEIGHT: u32 = 9;
const GRAPH_HEIGHT: u32 = 40;
const GRAPH_MIN_WIDTH: u32 = 240;
const DIVIDER_COLOR: (u8, u8, u8) = (160, 174, 192);

#[derive(Default, Clone)]
pub struct FrameStats {
    last: Option<Instant>,
    samples: VecDeque<(Instant, f32)>,
    graph: GraphHistory,
}
impl FrameStats {
    pub fn record(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let ms = now.duration_since(last).as_secs_f32() * 1000.0;
            self.samples.push_back((now, ms));
            self.graph.record(now, ms);
        }
        self.last = Some(now);
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(10))
        {
            self.samples.pop_front();
        }
    }
    pub fn values(&self) -> (f32, f32, f32, f32) {
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let mut times: Vec<_> = self
            .samples
            .iter()
            .map(|(_, ms)| *ms)
            .filter(|ms| *ms > 0.0 && ms.is_finite())
            .collect();
        if times.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let ms = *times.last().unwrap();
        let recent = times.iter().rev().take(120).copied().collect::<Vec<_>>();
        let fps = 1000.0 * recent.len() as f32 / recent.iter().sum::<f32>();
        let avg = 1000.0 * times.len() as f32 / times.iter().sum::<f32>();
        times.sort_unstable_by(f32::total_cmp);
        let count = times.len().div_ceil(100);
        let low = 1000.0 * count as f32 / times[times.len() - count..].iter().sum::<f32>();
        (fps, ms, avg, low)
    }
}
/// Five seconds in fixed 1/60 s bins. Keep the maximum interval in each
/// bin so reducing the graph resolution never averages away a short stall.
#[derive(Default, Clone)]
struct GraphHistory {
    origin: Option<Instant>,
    bins: VecDeque<(u64, f32)>,
}
impl GraphHistory {
    fn record(&mut self, now: Instant, ms: f32) {
        let origin = *self.origin.get_or_insert(now);
        let bucket = (now.duration_since(origin).as_secs_f64() * 60.0) as u64;
        if let Some((id, peak)) = self.bins.back_mut().filter(|(id, _)| *id == bucket) {
            let _ = id;
            *peak = peak.max(ms);
        } else {
            self.bins.push_back((bucket, ms));
        }
        while self
            .bins
            .front()
            .is_some_and(|(id, _)| bucket.saturating_sub(*id) >= 300)
        {
            self.bins.pop_front();
        }
    }
}
impl FrameStats {
    /// Only the small graph strip is uploaded at the chosen graph frequency.
    pub fn paint_graph(
        &self,
        now: Instant,
        width: u32,
        config: &OverlayConfig,
        pixels: &mut Vec<u32>,
    ) {
        let bg = config.bg_color;
        pixels.resize(width as usize * 40, 0);
        pixels.fill(over(0, (bg.0, bg.1, bg.2), bg.3 as u32));
        let Some(origin) = self.graph.origin else {
            return;
        };
        let current = now.duration_since(origin).as_secs_f64() * 60.0;
        let c = config.metric_color(argus_ipc::OverlayMetric::Graph);
        let margin = config.margin.min(32);
        let inner = width.saturating_sub(margin * 2).max(1);
        for (bin, peak) in &self.graph.bins {
            let right = 1.0 - (current - *bin as f64) / 300.0;
            let left = right - 1.0 / 300.0;
            if right < 0.0 {
                continue;
            }
            let x0 = margin + (left.max(0.0) * inner as f64) as u32;
            let x1 = (margin + (right.max(0.0) * inner as f64).ceil() as u32).min(width - margin);
            let h =
                (peak / config.graph_max_ms.clamp(5, 100) as f32 * 38.0).clamp(1.0, 38.0) as u32;
            for x in x0..x1 {
                // A thin peak trace, not a solid block that hides the game.
                for y in 39 - h..(41 - h).min(40) {
                    let i = (y * width + x) as usize;
                    pixels[i] = over(pixels[i], (c[0], c[1], c[2]), config.text_color.3 as u32);
                }
            }
        }
    }
}
pub struct HudImage {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub graph_y: Option<u32>,
}
pub struct HudWorker {
    tx: std::sync::mpsc::SyncSender<(Option<TelemetryFrame>, String, OverlayConfig, FrameStats)>,
    result: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<HudImage>>>>,
}
impl Default for HudWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl HudWorker {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(
            Option<TelemetryFrame>,
            String,
            OverlayConfig,
            FrameStats,
        )>(1);
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let output = result.clone();
        std::thread::spawn(move || {
            let mut raster = Rasterizer::default();
            for (tel, status, config, stats) in rx {
                let image = raster.rasterize(tel.as_ref(), &status, &config, &stats);
                *output.lock().unwrap() = Some(std::sync::Arc::new(image));
            }
        });
        Self { tx, result }
    }
    pub fn request(
        &self,
        tel: Option<&TelemetryFrame>,
        status: &str,
        config: &OverlayConfig,
        stats: &FrameStats,
    ) {
        let _ = self
            .tx
            .try_send((tel.cloned(), status.into(), config.clone(), stats.clone()));
    }
    pub fn image(&self) -> Option<std::sync::Arc<HudImage>> {
        self.result.lock().unwrap().clone()
    }
}
#[derive(Default)]
pub struct Rasterizer {
    glyphs: HashMap<(char, u32), (fontdue::Metrics, Vec<u8>)>,
}
fn value<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "--".into())
}
fn watts(v: Option<f32>) -> String {
    v.map(|x| format!("{x:.0}")).unwrap_or_else(|| "--".into())
}
fn memory(used: Option<f32>, total: Option<f32>) -> String {
    match (used, total) {
        (Some(u), Some(t)) if t > 0.0 => format!("{u:5.1} / {t:5.1} GiB"),
        _ => "   -- /    -- GiB".into(),
    }
}
fn over(dst: u32, rgb: (u8, u8, u8), alpha: u32) -> u32 {
    let inv = 255 - alpha;
    let channel = |s: u8, shift: u32| {
        ((s as u32 * alpha + ((dst >> shift) & 255u32) * inv + 127) / 255).min(255)
    };
    let a = alpha + (((dst >> 24) * inv + 127) / 255);
    channel(rgb.0, 0) | channel(rgb.1, 8) << 8 | channel(rgb.2, 16) << 16 | a << 24
}
#[derive(Default)]
struct Row {
    spans: Vec<Span>,
    divider: bool,
}
struct Span {
    text: String,
    metric: Option<argus_ipc::OverlayMetric>,
}
impl Span {
    fn label(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            metric: None,
        }
    }
    fn value(metric: argus_ipc::OverlayMetric, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            metric: Some(metric),
        }
    }
}
impl Row {
    #[cfg(test)]
    fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
    fn heading(metric: argus_ipc::OverlayMetric, text: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::value(metric, text)],
            divider: false,
        }
    }
}
fn cell(metric: argus_ipc::OverlayMetric, label: &str, value: String) -> Vec<Span> {
    vec![Span::label(format!("{label} ")), Span::value(metric, value)]
}
fn add_row(rows: &mut Vec<Row>, cells: impl IntoIterator<Item = Option<Vec<Span>>>) {
    let mut row = Row::default();
    for cell in cells.into_iter().flatten() {
        if !row.spans.is_empty() {
            row.spans.push(Span::label("   "));
        }
        row.spans.extend(cell);
    }
    if !row.spans.is_empty() {
        rows.push(row);
    }
}
fn section(rows: &mut Vec<Row>, mut content: Vec<Row>, dividers: bool) {
    if let Some(first) = content.first_mut() {
        first.divider = dividers && !rows.is_empty();
    }
    rows.extend(content);
}
fn content_rows(
    tel: Option<&TelemetryFrame>,
    status: &str,
    config: &OverlayConfig,
    stats: &FrameStats,
) -> Vec<Row> {
    use argus_ipc::OverlayMetric::*;
    let f = &config.fields;
    let mut rows = Vec::new();
    if !status.is_empty() {
        rows.push(Row::heading(UnavailableReason, status));
    }
    // Frame health comes first; component detail follows in a stable order.
    let mut part = Vec::new();
    if config.show_fps {
        let (fps, ms, avg, low) = stats.values();
        add_row(
            &mut part,
            [
                f.fps.then(|| cell(Fps, "FPS", format!("{fps:>5.0}"))),
                f.frametime
                    .then(|| cell(Frametime, "FRAME", format!("{ms:>6.2} ms"))),
            ],
        );
        add_row(
            &mut part,
            [
                f.average_fps
                    .then(|| cell(AverageFps, "AVG", format!("{avg:>5.0} [10 s]"))),
                f.low_1
                    .then(|| cell(Low1, "1% LOW", format!("{low:>5.0} [10 s]"))),
            ],
        );
    }
    section(&mut rows, part, config.section_dividers);
    if let Some(t) = tel {
        let mut part = Vec::new();
        if config.show_gpu {
            if f.gpu_name {
                part.push(Row::heading(GpuName, format!("GPU  {}", t.gpu_name)));
            }
            add_row(
                &mut part,
                [
                    f.gpu_usage.then(|| {
                        cell(
                            GpuUsage,
                            "LOAD",
                            format!("{:>3} %", value(t.gpu_usage_percent)),
                        )
                    }),
                    f.gpu_temp
                        .then(|| cell(GpuTemp, "TEMP", format!("{:>3} °C", value(t.gpu_temp_c)))),
                    f.gpu_power
                        .then(|| cell(GpuPower, "POWER", format!("{:>4} W", watts(t.gpu_power_w)))),
                ],
            );
            add_row(
                &mut part,
                [
                    f.gpu_core_clock.then(|| {
                        cell(
                            GpuCoreClock,
                            "CORE",
                            format!("{:>5} MHz", value(t.gpu_core_clock_mhz)),
                        )
                    }),
                    f.gpu_mem_clock.then(|| {
                        cell(
                            GpuMemClock,
                            "MEM",
                            format!("{:>5} MHz", value(t.gpu_mem_clock_mhz)),
                        )
                    }),
                    f.gpu_fan.then(|| {
                        cell(
                            GpuFan,
                            "FAN",
                            format!("{:>3} %", value(t.gpu_fan_speed_percent)),
                        )
                    }),
                ],
            );
            add_row(
                &mut part,
                [f.vram
                    .then(|| cell(Vram, "VRAM", memory(t.vram_used_gb, t.vram_total_gb)))],
            );
            if !f.gpu_name && !part.is_empty() {
                part.insert(0, Row::heading(GpuName, "GPU"));
            }
        }
        section(&mut rows, part, config.section_dividers);
        let mut part = Vec::new();
        if config.show_cpu {
            if f.cpu_name {
                part.push(Row::heading(CpuName, format!("CPU  {}", t.cpu_name)));
            }
            add_row(
                &mut part,
                [
                    f.cpu_usage
                        .then(|| cell(CpuUsage, "LOAD", format!("{:>3} %", t.cpu_usage_percent))),
                    f.cpu_temp
                        .then(|| cell(CpuTemp, "TEMP", format!("{:>3} °C", value(t.cpu_temp_c)))),
                    f.cpu_power
                        .then(|| cell(CpuPower, "POWER", format!("{:>4} W", watts(t.cpu_power_w)))),
                ],
            );
            add_row(
                &mut part,
                [f.cpu_frequency.then(|| {
                    cell(
                        CpuFrequency,
                        "CLOCK",
                        format!("{:>5} MHz", value(t.cpu_freq_mhz)),
                    )
                })],
            );
            if f.cpu_power && f.unavailable_reason && t.cpu_power_w.is_none() {
                part.push(Row::heading(
                    UnavailableReason,
                    format!("CPU power: {}", t.cpu_power_status),
                ));
            }
            if !f.cpu_name && !part.is_empty() {
                part.insert(0, Row::heading(CpuName, "CPU"));
            }
        }
        section(&mut rows, part, config.section_dividers);
        let mut part = Vec::new();
        let cpus: Vec<_> = t
            .cpus
            .iter()
            .filter(|cpu| !config.hidden_cpu_ids.contains(&cpu.id))
            .collect();
        if config.show_cores && !cpus.is_empty() {
            let mut heading = "CPU THREADS".to_string();
            if f.thread_usage {
                heading.push_str(" · load %");
            }
            if f.thread_frequency {
                heading.push_str(" · MHz");
            }
            part.push(Row::heading(ThreadId, heading));
            for chunk in cpus.chunks(4) {
                add_row(
                    &mut part,
                    chunk.iter().map(|cpu| {
                        let mut spans = vec![Span::value(ThreadId, format!("CPU {:02}", cpu.id))];
                        if f.physical_core_id {
                            spans.push(Span::value(
                                PhysicalCoreId,
                                format!(" (core {:>2})", value(cpu.core_id)),
                            ));
                        }
                        if cpu.online {
                            if f.thread_usage {
                                spans.push(Span::value(
                                    ThreadUsage,
                                    format!(" {:>3}%", value(cpu.usage)),
                                ));
                            }
                            if f.thread_frequency {
                                spans.push(Span::value(
                                    ThreadFrequency,
                                    format!(" {:>4}", value(cpu.frequency_mhz)),
                                ));
                            }
                        } else {
                            let width = usize::from(f.thread_usage) * 5
                                + usize::from(f.thread_frequency) * 5;
                            spans.push(Span::value(
                                UnavailableReason,
                                format!(" {:<width$}", "off", width = width.saturating_sub(1)),
                            ));
                        }
                        Some(spans)
                    }),
                );
            }
        }
        section(&mut rows, part, config.section_dividers);
        let mut part = Vec::new();
        if config.show_ram {
            add_row(
                &mut part,
                [
                    f.ram_usage.then(|| {
                        cell(
                            RamUsage,
                            "RAM",
                            memory(Some(t.ram_used_gb), Some(t.ram_total_gb)),
                        )
                    }),
                    f.ram_speed.then(|| {
                        cell(
                            RamSpeed,
                            "SPEED",
                            format!("{:>5} MT/s", value(t.ram_speed_mts)),
                        )
                    }),
                ],
            );
            if f.ram_speed && f.unavailable_reason && t.ram_speed_mts.is_none() {
                part.push(Row::heading(
                    UnavailableReason,
                    format!("RAM speed: {}", t.ram_speed_status),
                ));
            }
        }
        section(&mut rows, part, config.section_dividers);
        let mut part = Vec::new();
        add_row(
            &mut part,
            [
                f.argus_mode
                    .then(|| cell(ArgusMode, "ARGUS", t.active_profile.clone())),
                f.parked
                    .then(|| cell(Parked, "PARKED", format!("{:>2} threads", t.parked_cores))),
            ],
        );
        if let Some(game) = &t.game {
            if f.game_name {
                part.push(Row::heading(
                    GameName,
                    format!("App {} · PID {}", game.name, game.pid),
                ));
            }
            if f.game_profile {
                part.push(Row::heading(
                    GameProfile,
                    format!(
                        "Launcher profile: {}",
                        game.launcher_profile
                            .as_deref()
                            .unwrap_or("No tracked profile")
                    ),
                ));
            }
            if f.game_affinity {
                part.push(Row::heading(
                    GameAffinity,
                    format!(
                        "Main thread CPUs: {}",
                        game.main_thread_cpus.as_deref().unwrap_or("unavailable")
                    ),
                ));
            }
            if f.game_priority {
                part.push(Row::heading(
                    GamePriority,
                    format!("Nice: {}", value(game.nice)),
                ));
            }
            if f.probalance {
                part.push(Row::heading(
                    Probalance,
                    format!(
                        "ProBalance: {}",
                        if game.probalance_active {
                            "adjusting this process"
                        } else {
                            "no intervention in this process"
                        }
                    ),
                ));
            }
        }
        section(&mut rows, part, config.section_dividers);
    }
    rows
}

impl Rasterizer {
    pub fn rasterize(
        &mut self,
        tel: Option<&TelemetryFrame>,
        status: &str,
        config: &OverlayConfig,
        stats: &FrameStats,
    ) -> HudImage {
        let px = config.font_px.clamp(10, 24);
        let font = crate::font::get_font();
        let advance = font.metrics('0', px as f32).advance_width;
        let rows = content_rows(tel, status, config, stats);
        if rows.is_empty() && !config.show_graph {
            return HudImage {
                width: 1,
                height: 1,
                graph_y: None,
                pixels: vec![0],
            };
        }
        let margin = config.margin.min(32);
        let row_h = px + ROW_SPACING;
        let width = (rows
            .iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|s| s.text.chars().count())
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(1) as f32
            * advance)
            .ceil() as u32
            + margin * 2
            + 2;
        let width = if config.show_graph {
            width.max(GRAPH_MIN_WIDTH)
        } else {
            width
        };
        let graph_h = if config.show_graph { GRAPH_HEIGHT } else { 0 };
        let height = rows.len() as u32 * row_h
            + rows.iter().filter(|r| r.divider).count() as u32 * DIVIDER_HEIGHT
            + margin * 2
            + graph_h;
        let bg = config.bg_color;
        let mut image = HudImage {
            width,
            height,
            graph_y: config.show_graph.then(|| height - margin - GRAPH_HEIGHT),
            pixels: vec![over(0, (bg.0, bg.1, bg.2), bg.3 as u32); (width * height) as usize],
        };
        let mut top = margin;
        for row in &rows {
            if row.divider {
                let y = top + DIVIDER_HEIGHT / 2;
                for x in margin..width - margin {
                    let i = (y * width + x) as usize;
                    image.pixels[i] = over(
                        image.pixels[i],
                        DIVIDER_COLOR,
                        config.text_color.3 as u32 / 4,
                    );
                }
                top += DIVIDER_HEIGHT;
            }
            let y = top + px;
            top += row_h;
            let mut x = margin as f32;
            for span in &row.spans {
                let rgb = span.metric.map(|m| config.metric_color(m)).unwrap_or([
                    config.text_color.0,
                    config.text_color.1,
                    config.text_color.2,
                ]);
                for ch in span.text.chars() {
                    let (metrics, bitmap) = self
                        .glyphs
                        .entry((ch, px))
                        .or_insert_with(|| font.rasterize(ch, px as f32));
                    let base_x = x.round() as i32 + metrics.xmin;
                    let base_y = y as i32 - metrics.ymin - metrics.height as i32;
                    for shadow in [true, false] {
                        for r in 0..metrics.height {
                            for c in 0..metrics.width {
                                let coverage = bitmap[r * metrics.width + c] as u32;
                                let a = coverage * config.text_color.3 as u32 / 255;
                                let a = if shadow { a / 2 } else { a };
                                if a == 0 {
                                    continue;
                                }
                                let xx = base_x + c as i32 + i32::from(shadow);
                                let yy = base_y + r as i32 + i32::from(shadow);
                                if xx >= 0 && yy >= 0 && xx < width as i32 && yy < height as i32 {
                                    let i = yy as usize * width as usize + xx as usize;
                                    let rgb = if shadow {
                                        (0, 0, 0)
                                    } else {
                                        (rgb[0], rgb[1], rgb[2])
                                    };
                                    image.pixels[i] = over(image.pixels[i], rgb, a);
                                }
                            }
                        }
                    }
                    x += advance;
                }
            }
        }

        image
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_preserves_short_spikes_and_has_fixed_time_window() {
        let mut g = GraphHistory::default();
        let start = Instant::now();
        g.record(start, 2.0);
        g.record(start + Duration::from_millis(3), 25.0);
        g.record(start + Duration::from_millis(5), 1.0);
        assert_eq!(g.bins.len(), 1);
        assert_eq!(g.bins[0].1, 25.0);
        g.record(start + Duration::from_secs(5), 3.0);
        assert_eq!(g.bins.len(), 1);
        assert_eq!(g.bins[0].1, 3.0);
    }

    #[test]
    fn selections_remove_individual_values_and_preserve_cpu_ids() {
        let t = TelemetryFrame {
            gpu_name: "GPU MODEL".into(),
            gpu_temp_c: Some(42),
            cpus: vec![
                argus_ipc::LogicalCpu {
                    id: 3,
                    online: true,
                    usage: Some(7),
                    frequency_mhz: Some(5000),
                    ..Default::default()
                },
                argus_ipc::LogicalCpu {
                    id: 19,
                    online: true,
                    usage: Some(9),
                    frequency_mhz: Some(5100),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut c = OverlayConfig {
            show_cpu: false,
            show_ram: false,
            show_fps: false,
            ..Default::default()
        };
        c.fields.gpu_name = false;
        c.fields.gpu_usage = false;
        c.fields.gpu_power = false;
        c.fields.gpu_core_clock = false;
        c.fields.gpu_mem_clock = false;
        c.fields.gpu_fan = false;
        c.fields.vram = false;
        c.fields.thread_frequency = false;
        c.fields.physical_core_id = false;
        c.fields.parked = false;
        c.fields.argus_mode = false;
        c.hidden_cpu_ids = vec![3];
        let rows = content_rows(Some(&t), "", &c, &FrameStats::default())
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rows.contains("Temp  42 °C"));
        assert!(rows.contains("CPU 19"));
        for absent in ["GPU MODEL", "POWER", "MHz", "CPU 03", "5100", "ARGUS"] {
            assert!(!rows.contains(absent), "{rows}");
        }
        c.show_gpu = false;
        c.show_cores = false;
        c.bg_color.3 = 255;
        assert_eq!(
            Rasterizer::default()
                .rasterize(Some(&t), "", &c, &FrameStats::default())
                .pixels,
            vec![0]
        );
    }

    #[test]
    fn metric_color_changes_preserve_layout_alpha_and_other_values() {
        let mut raster = Rasterizer::default();
        let mut config = OverlayConfig::default();
        let stats = FrameStats::default();
        let before = raster.rasterize(None, "", &config, &stats);
        config
            .value_colors
            .insert(argus_ipc::OverlayMetric::Fps, [255, 40, 120]);
        let after = raster.rasterize(None, "", &config, &stats);
        assert_eq!((before.width, before.height), (after.width, after.height));
        let mut changed = 0;
        for (a, b) in before.pixels.iter().zip(&after.pixels) {
            assert_eq!(a >> 24, b >> 24, "color must not alter transparency");
            changed += usize::from(a != b);
        }
        assert!(changed > 0);
        // Only FPS on the first row changes; AVG / low on row two must be identical.
        let second_row = ((config.margin + config.font_px + 3) * before.width) as usize;
        assert_eq!(before.pixels[second_row..], after.pixels[second_row..]);
    }

    #[test]
    fn zero_background_and_text_alpha_are_independent() {
        let mut r = Rasterizer::default();
        let mut c = OverlayConfig::default();
        let img = r.rasterize(None, "Telemetri frakoblet", &c, &FrameStats::default());
        assert_eq!(img.pixels[0], 0);
        assert!(img.pixels.iter().any(|p| p >> 24 > 0));
        c.text_color.3 = 0;
        assert!(r
            .rasterize(None, "text", &c, &FrameStats::default())
            .pixels
            .iter()
            .all(|p| *p == 0));
        c.bg_color.3 = 128;
        assert!(r
            .rasterize(None, "text", &c, &FrameStats::default())
            .pixels
            .iter()
            .all(|p| p >> 24 == 128));
    }
    #[test]
    fn samples_every_frame_and_low_is_slowest_one_percent_mean() {
        let mut s = FrameStats::default();
        let mut t = Instant::now();
        s.record(t);
        for i in 0..200 {
            t += Duration::from_millis(if i == 0 {
                20
            } else if i == 1 {
                10
            } else {
                2
            });
            s.record(t);
        }
        assert_eq!(s.samples.len(), 200);
        assert!((s.values().3 - 1000.0 / 15.0).abs() < 0.01);
    }
}
