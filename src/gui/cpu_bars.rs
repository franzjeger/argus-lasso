//! Per-CPU bar widget and rolling CPU history chart.

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, Ui, Vec2};
use std::collections::VecDeque;

use crate::gui::theme;
use crate::utils;

// ── CpuBarsWidget ─────────────────────────────────────────────────────────────

/// Compact grid of per-CPU horizontal bars with load color coding and frequency.
pub struct CpuBarsWidget {
    pub cpu_pcts: Vec<f32>,
    pub cpu_freqs: Vec<Option<u64>>,
    pub offline: std::collections::HashSet<u32>,
}

impl CpuBarsWidget {
    pub fn new() -> Self {
        Self {
            cpu_pcts: Vec::new(),
            cpu_freqs: Vec::new(),
            offline: Default::default(),
        }
    }

    pub fn update(&mut self, pcts: Vec<f32>) {
        self.offline = utils::get_offline_cpus();
        self.cpu_freqs = (0..pcts.len())
            .map(|i| {
                let path = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq");
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .collect();
        self.cpu_pcts = pcts;
    }

    pub fn show(&self, ui: &mut Ui) {
        let n = self.cpu_pcts.len();
        if n == 0 {
            return;
        }

        // Compact single-line core cells: index left (weak), % right, and the
        // load shown as a background strip rather than a separate bar — this
        // fits the grid beside the history graph instead of under it.
        let avail_w = ui.available_width();
        let cell_h = 20.0;
        let gap = 3.0;
        let cell_min_w = 62.0;

        let max_cols = ((avail_w / cell_min_w) as usize).max(1).min(n);
        let cols = (1..=max_cols)
            .rev()
            .find(|&c| n.is_multiple_of(c))
            .unwrap_or(max_cols);
        let rows = n.div_ceil(cols);
        let cell_w = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32).max(46.0);

        let total_h = rows as f32 * (cell_h + gap);
        let (resp, painter) =
            ui.allocate_painter(Vec2::new(avail_w, total_h), egui::Sense::hover());
        let base = resp.rect.min;

        let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let text_color = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let cell_bg = ui.visuals().extreme_bg_color;
        let offline_bg = ui.visuals().faint_bg_color;
        let dark = ui.visuals().dark_mode;
        let font = crate::gui::theme::num_font(crate::gui::theme::tokens::FONT_SMALL);
        let hover_pos = resp.hover_pos();
        let mut hovered_cpu: Option<usize> = None;

        for i in 0..n {
            let col = (i % cols) as f32;
            let row = (i / cols) as f32;
            let x = base.x + col * (cell_w + gap);
            let y = base.y + row * (cell_h + gap);
            let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(cell_w, cell_h));

            let pct = self.cpu_pcts[i];
            let is_offline = self.offline.contains(&(i as u32));

            painter.rect_filled(
                rect,
                CornerRadius::same(3),
                if is_offline { offline_bg } else { cell_bg },
            );

            // Load as a background strip at ~33% alpha of the ramp colour
            if !is_offline && pct > 0.0 {
                let fill_w = (cell_w * pct / 100.0).clamp(0.0, cell_w);
                let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_w, cell_h));
                painter.rect_filled(
                    fill_rect,
                    CornerRadius::same(3),
                    theme::tint(theme::load_color_for(dark, pct), 84),
                );
            }

            painter.rect_stroke(
                rect,
                CornerRadius::same(3),
                Stroke::new(1.0_f32, border_color),
                egui::StrokeKind::Middle,
            );

            // Index left (weak), value right
            painter.text(
                Pos2::new(x + 5.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                i.to_string(),
                font.clone(),
                weak,
            );
            painter.text(
                Pos2::new(x + cell_w - 5.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                if is_offline {
                    "off".to_string()
                } else {
                    format!("{pct:.0}%")
                },
                font.clone(),
                if is_offline { weak } else { text_color },
            );

            if hover_pos.is_some_and(|p| rect.contains(p)) {
                hovered_cpu = Some(i);
            }
        }

        // Frequency readout moves to a hover tooltip so the cells stay compact.
        if let Some(i) = hovered_cpu {
            let freq = self
                .cpu_freqs
                .get(i)
                .copied()
                .flatten()
                .map(|khz| {
                    if khz >= 1_000_000 {
                        format!("{:.2} GHz", khz as f64 / 1_000_000.0)
                    } else {
                        format!("{} MHz", khz / 1_000)
                    }
                })
                .unwrap_or_else(|| "frequency unavailable".to_string());
            let state = if self.offline.contains(&(i as u32)) {
                "parked (offline)".to_string()
            } else {
                format!("{:.0}% load", self.cpu_pcts[i])
            };
            resp.on_hover_text(format!("CPU {i} — {state}\n{freq}"));
        }
    }
}

// ── CpuHistoryWidget ──────────────────────────────────────────────────────────

/// Rolling area chart of average CPU utilisation (120 samples).
pub struct CpuHistoryWidget {
    pub history: VecDeque<f32>,
    pub max_samples: usize,
}

impl CpuHistoryWidget {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            max_samples: 120,
        }
    }

    pub fn push(&mut self, avg: f32) {
        self.history.push_back(avg);
        while self.history.len() > self.max_samples {
            self.history.pop_front();
        }
    }

    pub fn show(&self, ui: &mut Ui) {
        if self.history.len() < 2 {
            // Still claim the space and draw the frame: collapsing to nothing
            // for the first two samples left a card-shaped hole beside the
            // core grid every time the app started.
            let h = ui.available_height().max(48.0);
            let (resp, painter) =
                ui.allocate_painter(Vec2::new(ui.available_width(), h), egui::Sense::hover());
            painter.rect_filled(
                resp.rect,
                CornerRadius::ZERO,
                crate::gui::theme::plot_fill(ui),
            );
            axis_labels(ui, &painter, resp.rect);
            return;
        }

        // Fill the card rather than a fixed 48px: the graph shares a row with
        // the core grid and the two are framed as a pair, so a short plot
        // inside a tall card reads as a rendering fault.
        let h = ui.available_height().max(48.0);
        let avail_w = ui.available_width();
        let (resp, painter) = ui.allocate_painter(Vec2::new(avail_w, h), egui::Sense::hover());
        let rect = resp.rect;

        painter.rect_filled(rect, CornerRadius::ZERO, crate::gui::theme::plot_fill(ui));
        // Without a scale the curve says "something moved", not how much —
        // the Overview graph has carried these labels all along.
        axis_labels(ui, &painter, rect);

        let n = self.history.len();
        let slot_w = avail_w / self.max_samples as f32;

        let pts: Vec<Pos2> = self
            .history
            .iter()
            .enumerate()
            .map(|(i, &pct)| {
                let offset = self.max_samples - n;
                let x = rect.min.x + (offset + i) as f32 * slot_w;
                let y = rect.max.y - 2.0 - (h - 4.0) * pct / 100.0;
                Pos2::new(x, y)
            })
            .collect();

        // Build filled path
        let last_avg = *self.history.back().unwrap_or(&0.0);
        let fill_color = {
            let c = theme::cpu_load_color(last_avg);
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 100)
        };

        // Fill as one trapezoid per segment — the curve itself is not convex,
        // and feeding it to convex_polygon makes the tessellator self-overlap.
        let bottom = rect.max.y - 2.0;
        for pair in pts.windows(2) {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pair[0],
                    pair[1],
                    Pos2::new(pair[1].x, bottom),
                    Pos2::new(pair[0].x, bottom),
                ],
                fill_color,
                Stroke::NONE,
            ));
        }

        // Line on top
        painter.add(egui::Shape::line(
            pts,
            Stroke::new(1.5_f32, theme::cpu_load_color(last_avg)),
        ));

        // Border
        let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0_f32, border_color),
            egui::StrokeKind::Middle,
        );
    }
}

/// 0 / 100 % markers on a CPU history plot, drawn inside its top and bottom
/// edges so they never collide with the curve's extremes.
fn axis_labels(ui: &Ui, painter: &egui::Painter, rect: egui::Rect) {
    let col = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(crate::gui::theme::tokens::FONT_SMALL);
    painter.text(
        rect.left_top() + Vec2::new(4.0, 1.0),
        egui::Align2::LEFT_TOP,
        "100%",
        font.clone(),
        col,
    );
    painter.text(
        rect.left_bottom() + Vec2::new(4.0, -1.0),
        egui::Align2::LEFT_BOTTOM,
        "0%",
        font,
        col,
    );
}
