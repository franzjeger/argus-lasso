//! Hardware Monitor tab — HWiNFO-style sensor view.
//!
//! One table frame holds everything: a sticky column header, then group header
//! rows (coloured category marker + "CATEGORY · device") followed by their
//! sensor rows. All numeric cells are monospace and right-aligned so live
//! refresh cannot make the columns jitter. Colour comes exclusively from
//! [`crate::gui::theme`] — the NOW value is the only semantically coloured cell,
//! min/max/avg stay weak grey.

use egui::{Color32, Stroke, Ui, Vec2};

use crate::gui::theme::{self, tokens};
use crate::hw_monitor::{HwMonitorData, Sensor, SensorGroup, HISTORY_LEN};

// Category display order
const CATEGORY_ORDER: &[&str] = &["CPU", "GPU", "Memory", "Storage", "Network", "System"];

// Categories offered as filter chips in the toolbar
const CHIP_CATEGORIES: &[&str] = &["CPU", "GPU", "Memory", "Storage", "Network"];

const GROUP_ROW_H: f32 = 24.0;
const HEADER_H: f32 = 22.0;
const LABEL_INDENT: f32 = 20.0;
const CELL_PAD: f32 = 6.0;
const STRIPE_W: f32 = 3.0;

// ── State ─────────────────────────────────────────────────────────────────────

pub struct HwMonitorTab {
    pub show_sparklines: bool,
    pub filter: String,
    /// Active category chips (session state). Empty = show every category.
    cat_filter: Vec<&'static str>,
    /// Fixed column widths: [value, min, max, avg].  Name column fills the rest.
    pub col_widths: [f32; 4],
    last_avail_w: f32,
    /// Set to true when user drags a column handle; app.rs persists col_widths to config.
    pub cols_dirty: bool,
}

impl HwMonitorTab {
    pub fn new() -> Self {
        Self {
            show_sparklines: true,
            filter: String::new(),
            cat_filter: Vec::new(),
            col_widths: [100.0, 72.0, 72.0, 72.0],
            last_avail_w: 0.0,
            cols_dirty: false,
        }
    }

    pub fn new_with_widths(widths: &[f32]) -> Self {
        let mut s = Self::new();
        if widths.len() == 4 {
            s.col_widths = [widths[0], widths[1], widths[2], widths[3]];
        }
        s
    }

    /// True when `category` passes the chip filter (no chips = everything).
    fn category_enabled(&self, category: &str) -> bool {
        self.cat_filter.is_empty() || self.cat_filter.contains(&category)
    }

    // ── Toolbar ───────────────────────────────────────────────────────────────

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let w = (ui.available_width() * 0.28).clamp(120.0, 240.0);
            // Same field styling as the Processes filter: a frameless edit
            // floats in the toolbar with nothing marking it as an input.
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .desired_width(w)
                    .hint_text("🔍  Search sensors…"),
            );
            if !self.filter.is_empty() && ui.small_button("✕").clicked() {
                self.filter.clear();
            }

            ui.add_space(tokens::SPACE_S);

            for cat in CHIP_CATEGORIES {
                let active = self.cat_filter.contains(cat);
                if theme::chip(ui, cat, active) {
                    if active {
                        self.cat_filter.retain(|c| c != cat);
                    } else {
                        self.cat_filter.push(cat);
                    }
                }
                ui.add_space(tokens::SPACE_XS);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("History graphs")
                        .size(tokens::FONT_LABEL)
                        .color(ui.visuals().weak_text_color()),
                );
                theme::toggle(ui, &mut self.show_sparklines);
            });
        });
    }

    // ── Main ──────────────────────────────────────────────────────────────────

    pub fn show(&mut self, ui: &mut Ui, data: &HwMonitorData) {
        self.toolbar(ui);
        ui.add_space(tokens::SPACE_S);

        if data.groups.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Reading sensors…").color(ui.visuals().weak_text_color()),
                );
            });
            return;
        }

        // ── Filtering (chips + search) ────────────────────────────────────────
        let search = self.filter.trim().to_lowercase();
        let visible = self.collect_visible(data, &search);

        // ── Column widths — auto-scale when the window is resized ─────────────
        let sparkline_w: f32 = if self.show_sparklines { 80.0 } else { 0.0 };
        let avail_w = ui.available_width();
        if self.last_avail_w > 0.0 && (avail_w - self.last_avail_w).abs() > 4.0 {
            let ratio = avail_w / self.last_avail_w.max(1.0);
            for w in &mut self.col_widths {
                *w = (*w * ratio).clamp(30.0, 300.0);
            }
        }
        self.last_avail_w = avail_w;

        let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;

        // ── One table frame for the whole tab ─────────────────────────────────
        egui::Frame::new()
            .stroke(Stroke::new(1.0_f32, border_color))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(1))
            .show(ui, |ui| {
                let inner_w = ui.available_width();
                let fixed_w: f32 = self.col_widths.iter().sum::<f32>() + sparkline_w;
                let col_name = (inner_w - fixed_w - 4.0).max(130.0);

                self.column_header(ui, inner_w, col_name, sparkline_w);

                if visible.is_empty() {
                    ui.add_space(tokens::SPACE_M);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("No sensors match the current filter")
                                .size(tokens::FONT_HELP)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                    ui.add_space(tokens::SPACE_M);
                    return;
                }

                let cols = [
                    self.col_widths[0],
                    self.col_widths[1],
                    self.col_widths[2],
                    self.col_widths[3],
                ];
                let show_spark = self.show_sparklines;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (group, sensors) in &visible {
                            group_header_row(ui, group);
                            for (row_idx, sensor) in sensors.iter().enumerate() {
                                sensor_row(
                                    ui,
                                    sensor,
                                    row_idx,
                                    col_name,
                                    cols,
                                    sparkline_w,
                                    show_spark,
                                );
                            }
                        }
                    });
            });
    }

    /// Groups (in category order) that survive the chip + search filters,
    /// paired with their visible sensors.
    fn collect_visible<'a>(
        &self,
        data: &'a HwMonitorData,
        search: &str,
    ) -> Vec<(&'a SensorGroup, Vec<&'a Sensor>)> {
        let mut ordered: Vec<&str> = CATEGORY_ORDER.to_vec();
        for g in &data.groups {
            if !ordered.contains(&g.category) {
                ordered.push(g.category);
            }
        }

        let mut out = Vec::new();
        for category in ordered {
            if !self.category_enabled(category) {
                continue;
            }
            for group in data.groups.iter().filter(|g| g.category == category) {
                // A search hit on the group (or category) name shows all its sensors.
                let group_hit = search.is_empty()
                    || group.name.to_lowercase().contains(search)
                    || category.to_lowercase().contains(search);
                let sensors: Vec<&Sensor> = group
                    .sensors
                    .iter()
                    .filter(|s| group_hit || s.label.to_lowercase().contains(search))
                    .collect();
                if !sensors.is_empty() {
                    out.push((group, sensors));
                }
            }
        }
        out
    }

    /// Sticky column header with drag-to-resize handles between fixed columns.
    fn column_header(&mut self, ui: &mut Ui, avail_w: f32, col_name: f32, sparkline_w: f32) {
        let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let (hdr_rect, _) =
            ui.allocate_exact_size(Vec2::new(avail_w, HEADER_H), egui::Sense::hover());
        ui.painter()
            .rect_filled(hdr_rect, 0.0, ui.visuals().widgets.noninteractive.bg_fill);
        ui.painter().line_segment(
            [hdr_rect.left_bottom(), hdr_rect.right_bottom()],
            Stroke::new(1.0_f32, border_color),
        );

        // SENSOR — left aligned
        paint_header(ui, "SENSOR", hdr_rect, hdr_rect.min.x + CELL_PAD, false);

        // Numeric headers — right aligned over their column
        let mut x = hdr_rect.min.x + col_name;
        for (w, label) in [
            (self.col_widths[0], "VALUE"),
            (self.col_widths[1], "MIN"),
            (self.col_widths[2], "MAX"),
            (self.col_widths[3], "AVG"),
        ] {
            paint_header(ui, label, hdr_rect, x + w - CELL_PAD, true);
            x += w;
        }
        if self.show_sparklines && sparkline_w > 0.0 {
            paint_header(ui, "HISTORY", hdr_rect, x + CELL_PAD, false);
        }

        // Separator after SENSOR column (not resizable — name fills the rest)
        let name_edge = hdr_rect.min.x + col_name;
        ui.painter().line_segment(
            [
                egui::pos2(name_edge, hdr_rect.min.y),
                egui::pos2(name_edge, hdr_rect.max.y),
            ],
            Stroke::new(1.0_f32, border_color),
        );

        // Resize handles between the four fixed columns (value/min/max/avg)
        let accent = theme::sem(ui).accent;
        let mut col_deltas = [0.0f32; 4];
        let mut hx = name_edge;
        for (i, delta) in col_deltas.iter_mut().enumerate() {
            hx += self.col_widths[i];
            let handle = egui::Rect::from_min_size(
                egui::pos2(hx - 3.0, hdr_rect.min.y),
                egui::vec2(6.0, HEADER_H),
            );
            let resp = ui.interact(
                handle,
                egui::Id::new(("hw_col_resize", i)),
                egui::Sense::drag(),
            );
            let line_col = if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                accent
            } else {
                border_color
            };
            ui.painter().line_segment(
                [
                    egui::pos2(hx, hdr_rect.min.y),
                    egui::pos2(hx, hdr_rect.max.y),
                ],
                Stroke::new(1.0_f32, line_col),
            );
            if resp.dragged() {
                *delta = resp.drag_delta().x;
            }
        }
        // Apply drag deltas for next frame
        self.cols_dirty = false;
        for (i, &d) in col_deltas.iter().enumerate() {
            if d.abs() > 0.001 {
                self.col_widths[i] = (self.col_widths[i] + d).max(30.0);
                self.cols_dirty = true;
            }
        }
    }
}

impl Default for HwMonitorTab {
    fn default() -> Self {
        Self::new()
    }
}

// ── Row painters ──────────────────────────────────────────────────────────────

/// Weak-grey column header label, optionally right aligned at `x`.
fn paint_header(ui: &Ui, label: &str, rect: egui::Rect, x: f32, right: bool) {
    let galley = egui::WidgetText::from(theme::header_text(ui, label, false)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let pos = egui::pos2(
        if right { x - galley.size().x } else { x },
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(pos, galley, Color32::WHITE);
}

/// 24px group header: 3px category marker stripe + "CATEGORY · device", nowrap.
fn group_header_row(ui: &mut Ui, group: &SensorGroup) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), GROUP_ROW_H),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().widgets.noninteractive.bg_fill);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );

    let color = category_color(ui, group.category);
    let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(STRIPE_W, rect.height()));
    painter.rect_filled(stripe, 0.0, color);

    let title = format!("{} · {}", group.category.to_uppercase(), group.name);
    let galley = egui::WidgetText::from(
        egui::RichText::new(title)
            .size(tokens::FONT_BODY)
            .font(theme::bold_font(tokens::FONT_BODY))
            .color(crate::gui::theme::strong_color(ui)),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (rect.width() - STRIPE_W - CELL_PAD * 2.0).max(16.0),
        egui::TextStyle::Body,
    );
    painter.galley(
        egui::pos2(
            rect.min.x + STRIPE_W + CELL_PAD,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        Color32::WHITE,
    );
}

/// 22px sensor row: indented label + monospace right-aligned numerics.
fn sensor_row(
    ui: &mut Ui,
    sensor: &Sensor,
    row_idx: usize,
    col_name: f32,
    cols: [f32; 4],
    sparkline_w: f32,
    show_spark: bool,
) {
    let row_h = tokens::ROW_H_DENSE;
    let (row_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), egui::Sense::hover());

    let bg = if row_idx.is_multiple_of(2) {
        ui.visuals().extreme_bg_color
    } else {
        ui.visuals().faint_bg_color
    };
    ui.painter().rect_filled(row_rect, 0.0, bg);

    let weak = ui.visuals().weak_text_color();
    let now_color = value_color(ui, sensor.value, sensor.unit);

    // Sensor label — indented, clipped to the name column
    let name_rect = egui::Rect::from_min_size(row_rect.min, egui::vec2(col_name, row_h));
    ui.painter_at(name_rect).text(
        egui::pos2(row_rect.min.x + LABEL_INDENT, row_rect.center().y),
        egui::Align2::LEFT_CENTER,
        sensor.label,
        egui::FontId::proportional(tokens::FONT_HELP),
        ui.visuals().text_color(),
    );

    // Numeric cells — monospace, right aligned
    let mut rx = row_rect.min.x + col_name;
    // Idle clock rows in full strength turned the frequency section into a
    // solid grey mass; a core sitting at its floor is not information worth
    // that much weight.
    let now_color = if sensor.unit == "MHz" && is_idle_clock(sensor) {
        weak
    } else {
        now_color
    };
    let cells: [(f32, Color32, f32); 4] = [
        (sensor.value, now_color, tokens::FONT_HELP),
        (sensor.min_display(), weak, tokens::FONT_LABEL),
        (sensor.max_display(), weak, tokens::FONT_LABEL),
        (sensor.avg(), weak, tokens::FONT_LABEL),
    ];
    for (i, (v, color, size)) in cells.iter().enumerate() {
        let w = cols[i];
        let cell = egui::Rect::from_min_size(egui::pos2(rx, row_rect.min.y), egui::vec2(w, row_h));
        ui.painter_at(cell).text(
            egui::pos2(rx + w - CELL_PAD, row_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            fmt_val(*v, sensor.unit),
            theme::num_font(*size),
            *color,
        );
        rx += w;
    }

    // Sparkline — stroked in the same colour as the current value
    if show_spark && sparkline_w > 0.0 {
        let spark_rect = egui::Rect::from_min_size(
            egui::pos2(rx + 2.0, row_rect.min.y + 3.0),
            Vec2::new(sparkline_w - 6.0, row_h - 6.0),
        );
        // 60% alpha: at full strength a column of sparklines competes with
        // the values it is meant to annotate, and for units with no ramp
        // (MHz, RPM, V) the plain white line was the loudest thing in the row.
        draw_sparkline(
            ui,
            spark_rect,
            sensor.ordered_history(),
            now_color.gamma_multiply(0.6),
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Category marker colour, taken from the shared ramp / semantic palette so
/// Breeze Light gets its AA-contrast variants automatically.
fn category_color(ui: &Ui, cat: &str) -> Color32 {
    let s = theme::sem(ui);
    match cat {
        "CPU" => s.accent,
        "GPU" => s.ok,
        "Memory" => s.manual,
        "Storage" => s.warning,
        "Network" => s.mid,
        _ => ui.visuals().weak_text_color(),
    }
}

fn fmt_val(v: f32, unit: &str) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    match unit {
        "°C" => format!("{v:.1} °C"),
        "RPM" => format!("{v:.0} RPM"),
        "W" => format!("{v:.2} W"),
        "V" => format!("{v:.3} V"),
        "MHz" => format!("{v:.0} MHz"),
        "GiB" => format!("{v:.2} GiB"),
        "MB" => format!("{v:.0} MB"),
        "MiB/s" => format!("{v:.2} MiB/s"),
        "%" => format!("{v:.1}%"),
        "Wh" => format!("{v:.2} Wh"),
        "" => format!("{v:.2}"),
        u => format!("{v:.2} {u}"),
    }
}

/// Semantic colour for the NOW value only — mapped onto the shared load ramp.
fn value_color(ui: &Ui, v: f32, unit: &str) -> Color32 {
    match unit {
        "°C" => theme::load_color(ui, temp_pct(v)),
        "%" => theme::load_color(ui, v),
        "W" => theme::load_color(ui, (v / 300.0 * 100.0).clamp(0.0, 100.0)),
        _ => ui.visuals().text_color(),
    }
}

/// True when a clock sensor is sitting near the bottom of its observed range.
///
/// Relative to what this machine has actually shown rather than an absolute
/// MHz threshold, which would be wrong on any CPU with a different floor.
fn is_idle_clock(sensor: &crate::hw_monitor::Sensor) -> bool {
    let span = sensor.max - sensor.min;
    span > 1.0 && sensor.value <= sensor.min + span * 0.25
}

/// Map a temperature onto the 0–100 load ramp: 40 °C → cool, 100 °C → critical.
/// Keeps the old thresholds roughly in place (70 °C ≈ warning, 85 °C ≈ hot).
fn temp_pct(c: f32) -> f32 {
    ((c - 40.0) / 60.0 * 100.0).clamp(0.0, 100.0)
}

fn draw_sparkline(
    ui: &mut Ui,
    rect: egui::Rect,
    history: impl Iterator<Item = f32> + Clone,
    color: Color32,
) {
    if history.clone().nth(1).is_none() {
        return;
    }

    let painter = ui.painter_at(rect);
    let vals = history;

    let lo = vals.clone().fold(f32::INFINITY, f32::min);
    let hi = vals.clone().fold(f32::NEG_INFINITY, f32::max);
    let range = (hi - lo).max(0.001);

    let w = rect.width();
    let h = rect.height();

    let px = |i: usize| rect.left() + i as f32 / (HISTORY_LEN as f32 - 1.0) * w;
    let py = |v: f32| rect.bottom() - (v - lo) / range * (h - 1.0);

    painter.rect_filled(rect, 2.0, theme::tint(color, 20));

    let mut points = vals.enumerate().map(|(i, v)| egui::pos2(px(i), py(v)));
    if let Some(mut previous) = points.next() {
        for point in points {
            painter.line_segment([previous, point], Stroke::new(1.0_f32, color));
            previous = point;
        }
    }
}
