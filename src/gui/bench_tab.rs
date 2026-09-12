//! Benchmark tab — memory latency test (AIDA64-style pointer chasing).
//!
//! Results appear in a dedicated OS-level window (egui viewport) so they can
//! be moved and resized independently of the main application window.

use egui::{Color32, CornerRadius, Margin, Pos2, Rect, RichText, Stroke, Vec2};

use crate::gui::theme::{self, tokens};
use crate::mem_bench::{
    BandwidthResult, CacheSizes, MemBandwidthBench, MemLatencyBench, MemLatencyResult, TEST_SIZES,
};

// ── State ─────────────────────────────────────────────────────────────────────

pub struct BenchTab {
    bench: MemLatencyBench,
    last: MemLatencyResult,
    cache: CacheSizes,
    results_open: bool,
    auto_opened: bool,
    graph_hover: Option<usize>,
    // Bandwidth
    bw_bench: MemBandwidthBench,
    last_bw: BandwidthResult,
    /// History of completed bandwidth runs (newest last)
    bw_history: Vec<BandwidthResult>,
    // Async CSV save
    csv_tx: std::sync::mpsc::Sender<String>,
    csv_rx: std::sync::mpsc::Receiver<String>,
    csv_status: String,
}

impl BenchTab {
    pub fn new() -> Self {
        let (csv_tx, csv_rx) = std::sync::mpsc::channel();
        Self {
            bench: MemLatencyBench::new(),
            last: MemLatencyResult::default(),
            cache: CacheSizes::read(),
            results_open: false,
            auto_opened: false,
            graph_hover: None,
            bw_bench: MemBandwidthBench::new(),
            last_bw: BandwidthResult::default(),
            bw_history: Vec::new(),
            csv_tx,
            csv_rx,
            csv_status: String::new(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, opacity: f32) {
        // Drain async CSV save status messages
        while let Ok(msg) = self.csv_rx.try_recv() {
            self.csv_status = msg;
        }

        let snap = self.bench.snapshot();
        if snap.running || snap.complete {
            self.last = snap;
        }

        if self.last.complete && !self.auto_opened {
            self.results_open = true;
            self.auto_opened = true;
        }
        if self.last.running {
            self.auto_opened = false;
        }

        let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;

        // ── Progress view ─────────────────────────────────────────────────────
        if self.last.running {
            egui::Frame::new()
                .stroke(Stroke::new(1.0_f32, border_color))
                .inner_margin(egui::Margin::same(12))
                .corner_radius(CornerRadius::same(4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.bench.cancel();
                        }
                        let pct = self.last.progress * 100.0;
                        let size_str = self.last.current_size.map(fmt_size).unwrap_or_default();
                        ui.label(RichText::new("Running…").size(tokens::FONT_BODY));
                        ui.label(
                            RichText::new(format!("{pct:.0}%"))
                                .font(theme::num_font(tokens::FONT_BODY))
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!("current: {size_str}"))
                                .size(tokens::FONT_HELP)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                    ui.add_space(tokens::SPACE_S);
                    ui.add(
                        egui::ProgressBar::new(self.last.progress)
                            .animate(true)
                            .desired_width(ui.available_width()),
                    );
                });
            ui.ctx().request_repaint();
            return;
        }

        // ── Wrap everything in a scroll area so both boxes are always accessible ─
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // ── Memory latency ──────────────────────────────────
                let lat_buttons: &[(&str, bool)] = if self.last.complete {
                    &[("Run again", false), ("Show results", false)]
                } else {
                    &[("Run test", true)]
                };
                let has_results = self.last.complete;
                let clicked = bench_card(ui, "Memory latency", lat_buttons, |ui| {
                    // Duration is information, not a warning — the orange text
                    // read as something being wrong. It belongs on the same
                    // help line as the method description.
                    ui.label(
                        RichText::new(
                            "Random pointer-chasing, one cache line per hop, to reduce hardware prefetching. \
                             Takes about 15–60 s and fully loads one CPU thread.",
                        )
                        .size(tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(tokens::SPACE_S);

                    if !has_results {
                        // Mockup 4c: four level cards reading "—" tell the user
                        // nothing except that they are empty. Before a run,
                        // say what a run would produce and what was detected.
                        ui.vertical_centered(|ui| {
                            ui.add_space(tokens::SPACE_M);
                            ui.label(
                                RichText::new(
                                    "No results yet — run the test to measure L1 / L2 / L3 / DRAM latency",
                                )
                                .size(tokens::FONT_HELP)
                                .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(tokens::SPACE_XS);
                            ui.label(
                                RichText::new(format!(
                                    "Detected: L1 {} · L2 {} · L3 {}",
                                    fmt_size(self.cache.l1d),
                                    fmt_size(self.cache.l2),
                                    fmt_size(self.cache.l3),
                                ))
                                .size(tokens::FONT_HELP)
                                .color(ui.visuals().weak_text_color().gamma_multiply(0.8)),
                            );
                            ui.add_space(tokens::SPACE_M);
                        });
                        return;
                    }

                    // Cache topology + latest latency, merged into four level cards.
                    let stats = region_latencies(&self.last.points, &self.cache);
                    let cols = level_colors(ui);
                    let sizes = [
                        fmt_size(self.cache.l1d),
                        fmt_size(self.cache.l2),
                        fmt_size(self.cache.l3),
                        format!("> {}", fmt_size(self.cache.l3)),
                    ];
                    let labels = ["L1", "L2", "L3", "DRAM"];
                    let outer = card_width(ui, 4);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = tokens::SPACE_S;
                        for i in 0..4 {
                            level_card(ui, outer, labels[i], &sizes[i], stats[i], cols[i]);
                        }
                    });
                });
                match clicked {
                    Some(0) => self.bench.start(),
                    Some(1) => self.results_open = true,
                    _ => {}
                }

                ui.add_space(tokens::SPACE_M);

                // ── Memory bandwidth ────────────────────────────────
                let bw = self.bw_bench.snapshot();
                let was_running = self.last_bw.running;
                if bw.running || bw.complete {
                    self.last_bw = bw.clone();
                }
                // Push to history when a run just completed
                if was_running && self.last_bw.complete {
                    self.bw_history.push(self.last_bw.clone());
                }

                let bw_buttons: &[(&str, bool)] = if self.last_bw.running {
                    &[]
                } else if self.last_bw.complete {
                    &[("Run again", false)]
                } else {
                    // Filled accent before the first run: with no results on
                    // the card, running the test is the only thing to do here.
                    // After a run it degrades to an outlined "Run again".
                    &[("Run test", true)]
                };
                let clicked = bench_card(ui, "Memory bandwidth", bw_buttons, |ui| {
                    ui.label(
                        RichText::new(
                            "Sequential read, write and copy throughput over a 256 MiB buffer \
                             (DRAM pressure), reported in GB/s.",
                        )
                        .size(tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(tokens::SPACE_S);

                    if self.last_bw.running {
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                self.bw_bench.cancel();
                            }
                            let stage = match (self.last_bw.progress * 3.0) as u32 {
                                0 => "Read…",
                                1 => "Write…",
                                _ => "Copy…",
                            };
                            ui.label(
                                RichText::new(format!("Running {stage}"))
                                    .size(tokens::FONT_HELP)
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                        ui.add_space(tokens::SPACE_XS);
                        ui.add(
                            egui::ProgressBar::new(self.last_bw.progress)
                                .animate(true)
                                .desired_width(ui.available_width()),
                        );
                        ui.ctx().request_repaint();
                    } else if self.last_bw.complete {
                        let prev = if self.bw_history.len() >= 2 {
                            Some(&self.bw_history[self.bw_history.len() - 2])
                        } else {
                            None
                        };
                        let cur = &self.last_bw;
                        let rows = [
                            ("READ", cur.read_gb_s, prev.map(|p| p.read_gb_s)),
                            ("WRITE", cur.write_gb_s, prev.map(|p| p.write_gb_s)),
                            ("COPY", cur.copy_gb_s, prev.map(|p| p.copy_gb_s)),
                        ];
                        let outer = card_width(ui, 3);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = tokens::SPACE_S;
                            for (label, value, previous) in rows {
                                bandwidth_card(ui, outer, label, value, previous);
                            }
                        });
                        if self.bw_history.len() > 1 {
                            ui.add_space(tokens::SPACE_XS);
                            ui.label(
                                RichText::new(format!("{} runs recorded", self.bw_history.len()))
                                    .size(tokens::FONT_SMALL)
                                    .color(ui.visuals().weak_text_color()),
                            );
                        }
                    } else {
                        // Mockup 4c: say the card is waiting for a run rather
                        // than rendering nothing at all.
                        ui.vertical_centered(|ui| {
                            ui.add_space(tokens::SPACE_M);
                            ui.label(
                                RichText::new("No results yet")
                                    .size(tokens::FONT_HELP)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(tokens::SPACE_M);
                        });
                    }

                    if !self.csv_status.is_empty() {
                        ui.add_space(tokens::SPACE_XS);
                        ui.label(
                            RichText::new(&self.csv_status)
                                .size(tokens::FONT_HELP)
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
                if clicked.is_some() {
                    self.bw_bench.start();
                }
            }); // end ScrollArea

        // ── Separate OS window for results ────────────────────────────────────
        if self.results_open && self.last.complete && !self.last.points.is_empty() {
            let ctx = ui.ctx().clone();
            let points = self.last.points.clone();
            let cache = self.cache.clone();
            let old_hover = self.graph_hover;
            let mut new_hover: Option<usize> = None;
            let mut close_requested = false;
            let csv_tx_clone = self.csv_tx.clone();

            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("bench_results"),
                egui::ViewportBuilder::default()
                    .with_title("Argus-Lasso — Memory latency results")
                    .with_app_id("argus-lasso")
                    .with_transparent(true)
                    .with_inner_size([800.0, 600.0])
                    .with_min_inner_size([540.0, 400.0])
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    }),
                |vp_ui, _class| {
                    theme::apply_viewport_opacity(vp_ui, opacity);
                    let ctx = &vp_ui.ctx().clone();
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_requested = true;
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        new_hover =
                            show_results(ui, &points, &cache, old_hover, csv_tx_clone.clone());
                    });
                },
            );

            self.graph_hover = new_hover;
            if close_requested {
                self.results_open = false;
            }
        }
    }
}

// ── Results content (rendered inside the separate viewport) ───────────────────

fn show_results(
    ui: &mut egui::Ui,
    points: &[crate::mem_bench::LatencyPoint],
    cache: &CacheSizes,
    hover_in: Option<usize>,
    csv_tx: std::sync::mpsc::Sender<String>,
) -> Option<usize> {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let weak = ui.visuals().weak_text_color();
    let sem = theme::sem(ui);
    let dark = ui.visuals().dark_mode;
    let cols = level_colors(ui);
    let max_ns = points.iter().map(|p| p.latency_ns).fold(0.0_f64, f64::max);

    // ── Latency summary cards ─────────────────────────────────────────────────
    let stats = region_latencies(points, cache);
    let sizes = [
        fmt_size(cache.l1d),
        fmt_size(cache.l2),
        fmt_size(cache.l3),
        format!("> {}", fmt_size(cache.l3)),
    ];
    let labels = ["L1", "L2", "L3", "DRAM"];
    let outer = card_width(ui, 4);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = tokens::SPACE_S;
        for i in 0..4 {
            level_card(ui, outer, labels[i], &sizes[i], stats[i], cols[i]);
        }
    });

    ui.add_space(tokens::SPACE_S);

    ui.horizontal(|ui| {
        // Run again lives on the Benchmark tab — this window only reports.
        ui.label(
            RichText::new("Close this window to run again from Tools → Memory benchmarks.")
                .color(weak)
                .size(tokens::FONT_SMALL),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Save CSV").clicked() {
                let csv = build_csv(points);
                let tx = csv_tx.clone();
                std::thread::spawn(move || {
                    let path = match crate::file_dialog::save("memory_latency.csv", "*.csv") {
                        Some(p) => p,
                        None => return,
                    };
                    let msg = match std::fs::write(&path, &csv) {
                        Ok(_) => "CSV saved.".to_string(),
                        Err(e) => format!("Save failed: {e}"),
                    };
                    tx.send(msg).ok();
                });
            }
        });
    });

    ui.separator();

    // ── Graph ─────────────────────────────────────────────────────────────────
    let avail = ui.available_size();
    let graph_h = (avail.y * 0.58).clamp(160.0, 420.0);

    let (outer_rect, response) =
        ui.allocate_exact_size(Vec2::new(avail.x, graph_h), egui::Sense::hover());

    let ml = 62.0;
    let mr = 12.0;
    let mt = 12.0;
    let mb = 34.0;
    let graph_rect = Rect::from_min_size(
        Pos2::new(outer_rect.min.x + ml, outer_rect.min.y + mt),
        Vec2::new(outer_rect.width() - ml - mr, graph_h - mt - mb),
    );

    let y_max = nice_ceil(max_ns * 1.12);
    let log_min = (*TEST_SIZES.first().unwrap() as f64).log2();
    let log_max = (*TEST_SIZES.last().unwrap() as f64).log2();
    let to_x = |b: usize| -> f32 {
        let t = ((b as f64).log2() - log_min) / (log_max - log_min);
        graph_rect.left() + t as f32 * graph_rect.width()
    };
    let to_y = |ns: f64| -> f32 { graph_rect.bottom() - (ns / y_max) as f32 * graph_rect.height() };

    let painter = ui.painter_at(outer_rect);

    // Cache region bands — same semantic ramp as the level cards
    let regions: [(usize, usize, &str, Color32); 4] = [
        (0, cache.l1d, "L1", cols[0]),
        (cache.l1d, cache.l2, "L2", cols[1]),
        (cache.l2, cache.l3, "L3", cols[2]),
        (cache.l3, *TEST_SIZES.last().unwrap() + 1, "DRAM", cols[3]),
    ];
    for (from, to, label, tcol) in regions {
        let rx1 = to_x(from.max(*TEST_SIZES.first().unwrap()));
        let rx2 = to_x(to.min(*TEST_SIZES.last().unwrap()));
        if rx2 <= rx1 {
            continue;
        }
        let band = Rect::from_min_max(
            Pos2::new(rx1, graph_rect.top()),
            Pos2::new(rx2, graph_rect.bottom()),
        );
        painter.rect_filled(band, 0.0, theme::tint(tcol, 30));
        painter.text(
            Pos2::new((rx1 + rx2) / 2.0, graph_rect.top() + 5.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::proportional(tokens::FONT_SMALL),
            tcol,
        );
    }

    painter.rect_filled(graph_rect, 2.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        graph_rect,
        2.0,
        Stroke::new(1.0_f32, border_color),
        egui::StrokeKind::Outside,
    );

    // Y grid
    for &ns in &nice_y_steps(y_max, 6) {
        let y = to_y(ns);
        if y < graph_rect.top() || y > graph_rect.bottom() {
            continue;
        }
        painter.line_segment(
            [
                Pos2::new(graph_rect.left(), y),
                Pos2::new(graph_rect.right(), y),
            ],
            Stroke::new(1.0_f32, theme::tint(weak, 45)),
        );
        painter.text(
            Pos2::new(graph_rect.left() - 5.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{ns:.0} ns"),
            theme::num_font(10.0),
            weak,
        );
    }

    // X grid
    for &size in TEST_SIZES {
        let x = to_x(size);
        painter.line_segment(
            [
                Pos2::new(x, graph_rect.top()),
                Pos2::new(x, graph_rect.bottom()),
            ],
            Stroke::new(1.0_f32, theme::tint(weak, 35)),
        );
        painter.text(
            Pos2::new(x, graph_rect.bottom() + 5.0),
            egui::Align2::CENTER_TOP,
            fmt_size_short(size),
            theme::num_font(9.5),
            weak,
        );
    }

    let mut hover_out: Option<usize> = None;

    if points.len() >= 2 {
        let pts: Vec<Pos2> = points
            .iter()
            .map(|p| Pos2::new(to_x(p.size_bytes), to_y(p.latency_ns)))
            .collect();

        // Fill
        let mut fill = pts.clone();
        fill.push(Pos2::new(pts.last().unwrap().x, graph_rect.bottom()));
        fill.push(Pos2::new(pts[0].x, graph_rect.bottom()));
        painter.add(egui::Shape::convex_polygon(
            fill,
            theme::tint(sem.accent, 22),
            Stroke::NONE,
        ));

        for seg in pts.windows(2) {
            painter.line_segment([seg[0], seg[1]], Stroke::new(2.0_f32, sem.accent));
        }

        // Hover detection
        if let Some(mouse) = response.hover_pos() {
            let mut best = f32::MAX;
            for (i, pt) in pts.iter().enumerate() {
                let d = (pt.x - mouse.x).abs();
                if d < best && d < 18.0 {
                    best = d;
                    hover_out = Some(i);
                }
            }
        }

        // Dots
        for (i, (p, pt)) in points.iter().zip(pts.iter()).enumerate() {
            let hover = hover_in == Some(i) || hover_out == Some(i);
            let r = if hover { 6.0 } else { 4.0 };
            let col = latency_color(dark, p.latency_ns, max_ns);
            painter.circle_filled(*pt, r, col);
            painter.circle_stroke(*pt, r, Stroke::new(1.0_f32, ui.visuals().extreme_bg_color));

            if hover {
                let tip = format!("{}  →  {:.1} ns", fmt_size(p.size_bytes), p.latency_ns);
                let tpos = Pos2::new(
                    (pt.x + 10.0).min(graph_rect.right() - 148.0),
                    (pt.y - 26.0).max(graph_rect.top() + 4.0),
                );
                let bg = Rect::from_min_size(tpos - Vec2::splat(3.0), Vec2::new(152.0, 22.0));
                painter.rect_filled(bg, 3.0, Color32::from_black_alpha(190));
                painter.text(
                    tpos,
                    egui::Align2::LEFT_TOP,
                    tip,
                    theme::num_font(tokens::FONT_LABEL),
                    Color32::WHITE,
                );
            }
        }
    }

    ui.add_space(tokens::SPACE_XS);
    ui.separator();

    // ── Detail table ──────────────────────────────────────────────────────────
    ui.label(
        RichText::new("Details")
            .strong()
            .size(tokens::FONT_HEADING)
            .color(crate::gui::theme::strong_color(ui)),
    );
    ui.add_space(tokens::SPACE_XS);

    egui::Frame::new()
        .stroke(Stroke::new(1.0_f32, border_color))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            let hdr_bg = ui.visuals().widgets.noninteractive.bg_fill;
            let (hr, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), egui::Sense::hover());
            ui.painter().rect_filled(hr, 0.0, hdr_bg);
            let fh = egui::FontId::proportional(tokens::FONT_SMALL);
            for (lbl, off) in [
                ("Working set", 10.0_f32),
                ("Latency", 130.0),
                ("Region", 230.0),
            ] {
                ui.painter().text(
                    Pos2::new(hr.min.x + off, hr.center().y),
                    egui::Align2::LEFT_CENTER,
                    lbl,
                    fh.clone(),
                    weak,
                );
            }
            ui.painter().line_segment(
                [hr.left_bottom(), hr.right_bottom()],
                Stroke::new(1.0_f32, border_color),
            );

            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    ui.style_mut().spacing.item_spacing.y = 0.0;
                    for (idx, p) in points.iter().enumerate() {
                        let row_h = 19.0;
                        let (rr, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), row_h),
                            egui::Sense::hover(),
                        );
                        let bg = if idx % 2 == 0 {
                            ui.visuals().extreme_bg_color
                        } else {
                            ui.visuals().faint_bg_color
                        };
                        ui.painter().rect_filled(rr, 0.0, bg);
                        if idx > 0 {
                            ui.painter().line_segment(
                                [rr.left_top(), rr.right_top()],
                                Stroke::new(1.0_f32, border_color),
                            );
                        }
                        let (region, rcol) = if p.size_bytes <= cache.l1d {
                            ("L1", cols[0])
                        } else if p.size_bytes <= cache.l2 {
                            ("L2", cols[1])
                        } else if p.size_bytes <= cache.l3 {
                            ("L3", cols[2])
                        } else {
                            ("DRAM", cols[3])
                        };
                        let cy = rr.center().y;
                        let rx = rr.min.x;
                        ui.painter().text(
                            Pos2::new(rx + 10.0, cy),
                            egui::Align2::LEFT_CENTER,
                            fmt_size(p.size_bytes),
                            theme::num_font(tokens::FONT_HELP),
                            ui.visuals().text_color(),
                        );
                        ui.painter().text(
                            Pos2::new(rx + 130.0, cy),
                            egui::Align2::LEFT_CENTER,
                            format!("{:.2} ns", p.latency_ns),
                            theme::num_font(tokens::FONT_HELP),
                            latency_color(dark, p.latency_ns, max_ns),
                        );
                        ui.painter().text(
                            Pos2::new(rx + 230.0, cy),
                            egui::Align2::LEFT_CENTER,
                            region,
                            egui::FontId::proportional(tokens::FONT_HELP),
                            rcol,
                        );
                    }
                });
        });

    hover_out
}

// ── Cards ─────────────────────────────────────────────────────────────────────

/// Bench section card: 15px bold title on the left, run buttons top-right.
/// `buttons` is `(label, primary)`; the first entry renders rightmost.
/// Returns the index of the clicked button, if any.
fn bench_card(
    ui: &mut egui::Ui,
    title: &str,
    buttons: &[(&str, bool)],
    add_contents: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let border_color = theme::tint(ui.visuals().text_color(), 22);
    let mut clicked = None;
    egui::Frame::new()
        .stroke(Stroke::new(1.0_f32, border_color))
        .inner_margin(Margin::same(16))
        .corner_radius(CornerRadius::same(8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .strong()
                        .size(tokens::FONT_HEADING)
                        .color(crate::gui::theme::strong_color(ui)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let s = theme::sem(ui);
                    for (i, (label, primary)) in buttons.iter().enumerate() {
                        let btn = if *primary {
                            egui::Button::new(RichText::new(*label).color(s.on_accent).strong())
                                .fill(s.accent)
                        } else {
                            egui::Button::new(RichText::new(*label))
                        };
                        if ui.add(btn).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            });
            theme::divider(ui);
            add_contents(ui);
        });
    clicked
}

/// Outer width for `n` equally sized cards laid out across the current row.
fn card_width(ui: &egui::Ui, n: usize) -> f32 {
    let gaps = tokens::SPACE_S * (n.saturating_sub(1)) as f32;
    ((ui.available_width() - gaps) / n as f32).max(80.0)
}

/// Cache-level card: coloured left border, weak "L2 · 16 MiB" label, big
/// monospace latency.
fn level_card(
    ui: &mut egui::Ui,
    outer_w: f32,
    label: &str,
    size: &str,
    ns: Option<f64>,
    color: Color32,
) {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    egui::Frame::new()
        .stroke(Stroke::new(1.0_f32, border_color))
        .inner_margin(Margin {
            left: 12,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .corner_radius(CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_width((outer_w - 24.0).max(56.0));
            let top = ui.min_rect().min;
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(format!("{label} · {size}"))
                        .size(tokens::FONT_SMALL)
                        .color(ui.visuals().weak_text_color()),
                );
                ui.label(
                    RichText::new(match ns {
                        Some(v) => format!("{v:.1} ns"),
                        None => "—".into(),
                    })
                    .font(theme::num_font(19.0))
                    .strong()
                    .color(color),
                );
            });
            // Left border in the ramp colour
            let h = ui.min_rect().height();
            let stripe = Rect::from_min_size(
                Pos2::new(top.x - 12.0, top.y - 8.0),
                Vec2::new(3.0, h + 16.0),
            );
            ui.painter().rect_filled(stripe, 0.0, color);
        });
}

/// Bandwidth card: READ/WRITE/COPY label, big monospace GB/s, delta line.
fn bandwidth_card(ui: &mut egui::Ui, outer_w: f32, label: &str, value: f64, prev: Option<f64>) {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let s = theme::sem(ui);
    egui::Frame::new()
        .stroke(Stroke::new(1.0_f32, border_color))
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_width((outer_w - 26.0).max(56.0));
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(label)
                        .size(tokens::FONT_SMALL)
                        .color(ui.visuals().weak_text_color()),
                );
                ui.label(
                    RichText::new(format!("{value:.2} GB/s"))
                        .font(theme::num_font(19.0))
                        .strong(),
                );
                let (text, color) = match prev {
                    Some(p) => {
                        let d = value - p;
                        let t = format!("{d:+.2} vs. previous");
                        if d > 0.05 {
                            (t, s.ok)
                        } else if d < -0.05 {
                            (t, s.negative)
                        } else {
                            (t, ui.visuals().weak_text_color())
                        }
                    }
                    None => (
                        "no previous run".to_string(),
                        ui.visuals().weak_text_color(),
                    ),
                };
                ui.label(
                    RichText::new(text)
                        .font(theme::num_font(tokens::FONT_HELP))
                        .color(color),
                );
            });
        });
}

// ── Colours ───────────────────────────────────────────────────────────────────

/// L1 → DRAM ramp taken straight from the semantic palette.
fn level_colors(ui: &egui::Ui) -> [Color32; 4] {
    let s = theme::sem(ui);
    [s.ok, s.mid, s.warning, s.negative]
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Average latency per cache level: [L1, L2, L3, DRAM].
/// DRAM uses the upper half of the out-of-L3 samples (steady state).
fn region_latencies(
    points: &[crate::mem_bench::LatencyPoint],
    cache: &CacheSizes,
) -> [Option<f64>; 4] {
    let avg_for = |lo: usize, hi: usize| -> Option<f64> {
        let v: Vec<f64> = points
            .iter()
            .filter(|p| p.size_bytes > lo && p.size_bytes <= hi)
            .map(|p| p.latency_ns)
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(v.iter().sum::<f64>() / v.len() as f64)
        }
    };
    let dram_ns: Option<f64> = {
        let v: Vec<f64> = points
            .iter()
            .filter(|p| p.size_bytes > cache.l3)
            .map(|p| p.latency_ns)
            .collect();
        if v.is_empty() {
            None
        } else {
            let s = v.len() / 2;
            Some(v[s..].iter().sum::<f64>() / (v.len() - s) as f64)
        }
    };
    [
        avg_for(0, cache.l1d),
        avg_for(cache.l1d, cache.l2),
        avg_for(cache.l2, cache.l3),
        dram_ns,
    ]
}

fn build_csv(points: &[crate::mem_bench::LatencyPoint]) -> String {
    let mut s = String::from("size_bytes,size_label,latency_ns\n");
    for p in points {
        s.push_str(&format!(
            "{},{},{:.3}\n",
            p.size_bytes,
            fmt_size(p.size_bytes),
            p.latency_ns
        ));
    }
    s
}

pub fn fmt_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}

fn fmt_size_short(bytes: usize) -> &'static str {
    match bytes {
        4096 => "4K",
        8192 => "8K",
        16384 => "16K",
        32768 => "32K",
        65536 => "64K",
        131072 => "128K",
        262144 => "256K",
        524288 => "512K",
        1048576 => "1M",
        2097152 => "2M",
        4194304 => "4M",
        8388608 => "8M",
        16777216 => "16M",
        33554432 => "32M",
        67108864 => "64M",
        134217728 => "128M",
        268435456 => "256M",
        _ => "?",
    }
}

/// Latency → semantic ramp position (fast = ok, slow = negative).
fn latency_color(dark_mode: bool, ns: f64, max_ns: f64) -> Color32 {
    let pct = (ns / max_ns.max(1.0)).clamp(0.0, 1.0) as f32 * 100.0;
    theme::load_color_for(dark_mode, pct)
}

fn nice_ceil(v: f64) -> f64 {
    if v <= 0.0 {
        return 100.0;
    }
    let mag = 10.0_f64.powf(v.log10().floor());
    let frac = v / mag;
    (if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    }) * mag
}

fn nice_y_steps(max: f64, n: usize) -> Vec<f64> {
    let step = nice_ceil(max / n as f64);
    let mut v = Vec::new();
    let mut cur = step;
    while cur <= max + step * 0.5 {
        v.push(cur);
        cur += step;
    }
    v
}
