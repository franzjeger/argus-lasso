//! Log tab: structured, filterable log view.
//!
//! Each line renders as three columns — time (weak) · category tag (coloured,
//! fixed width) · message (normal text colour, ellipsized). The colour carries
//! the category so the message itself stays readable, replacing the old
//! whole-line colouring which was also invisible to users as a filter.

use egui::Ui;

use super::theme::{self, tokens};

/// Log line categories, derived from the tag prefixes the logger emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Gaming,
    Rules,
    Manual,
    Hw,
    Error,
}

impl Category {
    /// All categories, in toolbar chip order.
    const ALL: [Category; 5] = [
        Category::Gaming,
        Category::Rules,
        Category::Manual,
        Category::Hw,
        Category::Error,
    ];

    /// Short tag shown in the category column and on the filter chip.
    fn label(self) -> &'static str {
        match self {
            Category::Gaming => "Gaming",
            Category::Rules => "Rules",
            Category::Manual => "Manual",
            Category::Hw => "HW",
            Category::Error => "Errors",
        }
    }

    /// Semantic colour for this category (never a hardcoded rgb literal).
    fn color(self, sem: &theme::Sem) -> egui::Color32 {
        match self {
            Category::Gaming => sem.ok,
            Category::Rules => sem.accent,
            Category::Manual => sem.manual,
            Category::Hw => sem.warning,
            Category::Error => sem.negative,
        }
    }
}

/// Classify a log line by the tag prefixes used by the logger. Errors win over
/// everything else so a failed rule application still reads as an error.
fn categorize(line: &str) -> Option<Category> {
    if line.contains("FAILED") || line.contains("failed") || line.contains("error") {
        Some(Category::Error)
    } else if line.contains("[Gaming Mode]")
        || line.contains("[Launcher]")
        || line.contains("[Profile]")
        || line.contains("[Reset]")
        || line.contains("[Park]")
    {
        Some(Category::Gaming)
    } else if line.contains("[Rule:") || line.contains("[Default]") {
        Some(Category::Rules)
    } else if line.contains("[Manual]") {
        Some(Category::Manual)
    } else if line.contains("[HW Alert]") {
        Some(Category::Hw)
    } else {
        None
    }
}

/// Split a leading `[HH:MM:SS] ` timestamp off the line. Returns (time, rest).
fn split_time(line: &str) -> (&str, &str) {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let stamp = &rest[..end];
            // Only treat it as a timestamp if it looks like one.
            let is_time = stamp.len() == 8
                && stamp.bytes().enumerate().all(|(i, b)| {
                    if i == 2 || i == 5 {
                        b == b':'
                    } else {
                        b.is_ascii_digit()
                    }
                });
            if is_time {
                return (stamp, rest[end + 1..].trim_start());
            }
        }
    }
    ("", line)
}

/// Column widths.
const TIME_W: f32 = 62.0;
const TAG_W: f32 = 100.0;

pub struct LogTab {
    pub auto_scroll: bool,
    /// Active category filters (session state — additive, empty = show all).
    active: Vec<Category>,
    /// Case-insensitive substring search over the message column.
    search: String,
}

impl LogTab {
    pub fn new() -> Self {
        Self {
            auto_scroll: true,
            active: Vec::new(),
            search: String::new(),
        }
    }

    /// Show with a clear button. Returns (clear_requested, save_requested).
    pub fn show_with_clear(
        &mut self,
        ui: &mut Ui,
        lines: &std::collections::VecDeque<String>,
    ) -> (bool, bool) {
        let mut clear = false;
        let mut save = false;
        let sem = theme::sem(ui);

        // ── Toolbar: search · chips · spacer · Auto-scroll · Save… · Clear ──
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search log…")
                    .desired_width(200.0),
            );
            if !self.search.is_empty() && ui.small_button("✕").clicked() {
                self.search.clear();
            }
            ui.add_space(tokens::SPACE_S);
            for cat in Category::ALL {
                let on = self.active.contains(&cat);
                if cat_chip(ui, cat.label(), on, cat.color(&sem)) {
                    if on {
                        self.active.retain(|c| *c != cat);
                    } else {
                        self.active.push(cat);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Destructive action last (rightmost).
                if ui
                    .button(egui::RichText::new("Clear log").color(sem.negative))
                    .clicked()
                {
                    clear = true;
                }
                if ui.button("Save log…").clicked() {
                    save = true;
                }
                ui.checkbox(&mut self.auto_scroll, "Auto-scroll");
            });
        });
        ui.separator();

        // ── Filter first, then virtualise over the filtered set ────────────
        let needle = self.search.to_lowercase();
        let filtered: Vec<(&str, &str, Option<Category>)> = lines
            .iter()
            .filter_map(|line| {
                let cat = categorize(line);
                if !self.active.is_empty() && !cat.is_some_and(|c| self.active.contains(&c)) {
                    return None;
                }
                let (time, msg) = split_time(line);
                if !needle.is_empty() && !msg.to_lowercase().contains(&needle) {
                    return None;
                }
                Some((time, msg, cat))
            })
            .collect();

        // show_rows virtualizes the list — only visible rows are laid out,
        // instead of all 2000 buffered lines on every repaint.
        let font = egui::FontId::monospace(11.0);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace).max(14.0);
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .stick_to_bottom(self.auto_scroll)
            .show_rows(ui, row_height, filtered.len(), |ui, range| {
                let width = ui.available_width();
                for (time, msg, cat) in &filtered[range] {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let painter = ui.painter();
                    let mid = egui::Align2::LEFT_CENTER;
                    let y = rect.center().y;

                    // Time column — weak, monospace, never wraps.
                    if !time.is_empty() {
                        painter.text(egui::pos2(rect.left(), y), mid, time, font.clone(), weak);
                    }

                    // Category column — fixed width, category colour, nowrap.
                    if let Some(c) = cat {
                        let col = c.color(&sem);
                        let galley = painter.layout_no_wrap(
                            c.label().to_string(),
                            egui::FontId::proportional(tokens::FONT_LABEL),
                            col,
                        );
                        painter.galley(
                            egui::pos2(rect.left() + TIME_W, y - galley.size().y * 0.5),
                            galley,
                            col,
                        );
                    }
                    let msg_x = rect.left() + TIME_W + TAG_W;

                    // Message column — normal text colour, ellipsized.
                    let avail = (rect.right() - msg_x - tokens::SPACE_XS).max(16.0);
                    let mut job = egui::text::LayoutJob::single_section(
                        (*msg).to_string(),
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: text_col,
                            ..Default::default()
                        },
                    );
                    job.wrap = egui::text::TextWrapping {
                        max_width: avail,
                        max_rows: 1,
                        break_anywhere: true,
                        overflow_character: Some('…'),
                    };
                    let galley = painter.layout_job(job);
                    painter.galley(
                        egui::pos2(msg_x, y - galley.size().y * 0.5),
                        galley,
                        text_col,
                    );
                }
            });

        (clear, save)
    }
}

/// Pill filter chip in a category colour — same geometry as [`theme::chip`],
/// which is accent-only, but tinted per category so the colour code in the
/// rows and the chips match.
fn cat_chip(ui: &mut egui::Ui, label: &str, active: bool, color: egui::Color32) -> bool {
    let galley = ui.painter().layout_no_wrap(
        if active {
            format!("{label} ✕")
        } else {
            label.to_string()
        },
        egui::FontId::proportional(tokens::FONT_LABEL),
        color,
    );
    let pad = egui::vec2(9.0, 4.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if active {
            theme::tint(color, 46)
        } else if resp.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };
        let stroke = if active {
            egui::Stroke::new(1.0_f32, color)
        } else {
            egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
        };
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(12),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(rect.min + pad, galley, egui::Color32::WHITE);
    }
    resp.clicked()
}
