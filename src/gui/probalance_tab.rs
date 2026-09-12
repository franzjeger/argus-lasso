//! ProBalance settings tab — live throttle view + configuration.

use crate::config::ProBalanceConfig;
use egui::{RichText, Ui};

pub struct ProBalanceTab {
    pub cfg: ProBalanceConfig,
    /// Buffer behind the "+ add pattern" chip.
    new_exempt: String,
    /// Last applied config — the apply bar is enabled only while `cfg` differs.
    saved: ProBalanceConfig,
    /// True while the "+ add pattern" chip is expanded into a text field.
    adding_exempt: bool,
}

impl ProBalanceTab {
    pub fn new(cfg: ProBalanceConfig) -> Self {
        Self {
            saved: cfg.clone(),
            cfg,
            new_exempt: String::new(),
            adding_exempt: false,
        }
    }

    /// Plain-language summary of the thresholds, for the status card.
    fn summary(&self) -> String {
        format!(
            "Per-process CPU: throttle above {:.0}% for {:.0} s · restore below {:.0}%",
            self.cfg.cpu_threshold_percent,
            self.cfg.consecutive_seconds,
            self.cfg.restore_threshold_percent
        )
    }

    /// Returns Some(updated_config) when Apply is clicked.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &[crate::monitor::ProcInfo],
        throttle_infos: &[crate::probalance::ThrottleInfo],
    ) -> Option<ProBalanceConfig> {
        use crate::gui::theme::{self as th, tokens};
        const LABEL_W: f32 = tokens::FORM_LABEL_W;
        let s = th::sem(ui);

        // The apply bar has to sit on the panel's bottom edge, the same as
        // Settings — floating it after however tall the content happened to
        // be put the same control in a different place on each tab. Reserving
        // the bar's height and scrolling the body is what pins it.
        let bar_h = 44.0;
        let body_h = (ui.available_height() - bar_h).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt("probalance_body")
            .max_height(body_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {

        // ── Status card: state, plain-language summary, live count ────────
        crate::gui::theme::card_untitled(ui, |ui| {
            ui.horizontal(|ui| {
                th::toggle(ui, &mut self.cfg.enabled);
                ui.add_space(tokens::SPACE_S);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(if self.cfg.enabled {
                            "ProBalance is on"
                        } else {
                            "ProBalance is off"
                        })
                        .font(th::bold_font(tokens::FONT_HERO))
                        .color(crate::gui::theme::strong_color(ui)),
                    );
                    ui.label(
                        RichText::new(self.summary())
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                    );
                    // When nothing is throttled the fact belongs here, under
                    // the state it qualifies — as its own grey strip below a
                    // card it read as an unrelated warning.
                    if throttle_infos.is_empty() {
                        ui.label(
                            RichText::new(if self.cfg.enabled {
                                "Nothing is being throttled right now"
                            } else {
                                "Nothing is being throttled"
                            })
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let n = throttle_infos.len();
                    if n > 0 {
                        th::badge_outline_colored(ui, &format!("{n} throttled now"), s.warning);
                    }
                });
            });
        });
        ui.add_space(tokens::SPACE_M);

        // ── Live throttle view ────────────────────────────────────────────
        // Hidden entirely when empty: an empty table card is a panel-sized
        // reminder that there is nothing to show. The status card above
        // already says so in one line.
        if !throttle_infos.is_empty() {
            crate::gui::theme::card_untitled(ui, |ui| {
                egui::Grid::new("pb_throttle_rows")
                    .num_columns(5)
                    .min_row_height(tokens::ROW_H_DENSE)
                    .spacing([tokens::SPACE_M, 2.0])
                    .show(ui, |ui| {
                        for h in ["PID", "NAME", "CPU%", "THROTTLE", "RESTORE IN"] {
                            ui.label(th::header_text(ui, h, false));
                        }
                        ui.end_row();

                        let cpu_map: std::collections::HashMap<u32, f32> =
                            snapshot.iter().map(|p| (p.pid, p.cpu_percent)).collect();

                        for info in throttle_infos {
                            let cpu = cpu_map.get(&info.pid).copied().unwrap_or(info.cpu_percent);

                            ui.label(
                                RichText::new(info.pid.to_string())
                                    .font(th::num_font(tokens::FONT_BODY))
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.label(RichText::new(&info.name).color(s.warning));
                            ui.label(
                                RichText::new(format!("{cpu:.1}"))
                                    .font(th::num_font(tokens::FONT_BODY))
                                    .color(th::load_color(ui, cpu)),
                            );
                            match &info.unit {
                                Some(unit) => {
                                    ui.label(
                                        RichText::new(format!("unit {unit}"))
                                            .size(tokens::FONT_HELP)
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                None => {
                                    ui.label(
                                        RichText::new(format!(
                                            "nice {} → {}",
                                            info.original_nice, info.throttle_nice
                                        ))
                                        .size(tokens::FONT_HELP)
                                        .color(ui.visuals().weak_text_color()),
                                    );
                                }
                            }
                            restore_progress(
                                ui,
                                info.consecutive_low,
                                info.restore_hysteresis,
                                s.warning,
                            );
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(tokens::SPACE_M);
        }

        // ── Throttling and restore ────────────────────────────────────────
        let max_process_cpu = crate::utils::get_cpu_count().max(1) as f32 * 100.0;
        let throttle_max = max_process_cpu.max(self.cfg.cpu_threshold_percent);
        let restore_max = max_process_cpu.max(self.cfg.restore_threshold_percent);
        th::card_hinted(ui, "Throttling and restore",
            "Per-process scale: 100% = one logical CPU fully busy; 200% = two. Overall CPU pressure is not checked before lowering priority.", |ui| {
            egui::Grid::new("pb_thresholds")
                .num_columns(2)
                .min_row_height(tokens::ROW_H)
                .spacing([tokens::SPACE_S, tokens::SPACE_XS])
                .show(ui, |ui| {
                    form_label(ui, LABEL_W, "Throttle method");
                    egui::ComboBox::from_id_salt("pb_method")
                        .selected_text(match self.cfg.method.as_str() {
                            "cgroup" => "cgroup (per-app CPUWeight)",
                            "auto" => "auto (cgroup, nice fallback)",
                            _ => "nice (process priority)",
                        })
                        .show_ui(ui, |ui| {
                            for (val, label) in [
                                ("nice", "nice (process priority)"),
                                ("cgroup", "cgroup (per-app CPUWeight)"),
                                ("auto", "auto (cgroup, nice fallback)"),
                            ] {
                                ui.selectable_value(&mut self.cfg.method, val.to_string(), label);
                            }
                        });
                    ui.end_row();

                    form_label(ui, LABEL_W, "Throttle above");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.cpu_threshold_percent)
                                .range(10.0f32..=throttle_max)
                                .suffix(" %"),
                        );
                        weak(ui, "for");
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.consecutive_seconds)
                                .range(1.0f32..=60.0)
                                .suffix(" s"),
                        );
                    });
                    ui.end_row();

                    form_label(ui, LABEL_W, "Nice adjustment");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.nice_adjustment)
                                .range(1..=19)
                                .prefix("+"),
                        );
                        weak(ui, "capped at");
                        ui.add(egui::DragValue::new(&mut self.cfg.nice_floor).range(1..=19));
                    });
                    ui.end_row();

                    form_label(ui, LABEL_W, "Restore below");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.restore_threshold_percent)
                                .range(1.0f32..=restore_max)
                                .suffix(" %"),
                        );
                        weak(ui, "for");
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.restore_hysteresis_seconds)
                                .range(1.0f32..=120.0)
                                .suffix(" s"),
                        );
                    });
                    ui.end_row();

                    if self.cfg.method != "nice" {
                        form_label(ui, LABEL_W, "Throttled CPUWeight");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.cfg.cgroup_throttle_weight)
                                    .range(1..=100),
                            );
                            weak(ui, "kernel default is 100");
                        });
                        ui.end_row();

                        form_label(ui, LABEL_W, "Hard CPU quota");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.cfg.cgroup_quota_percent)
                                    .range(0..=800)
                                    .suffix(" %"),
                            );
                            weak(ui, "0 = no cap");
                        });
                        ui.end_row();
                    }
                });
        });
        ui.add_space(tokens::SPACE_M);

        // ── Exempt processes (chips) ──────────────────────────────────────
        th::card(ui, "Exempt processes", |ui| {
            ui.label(
                RichText::new("Processes whose name contains one of these patterns are never throttled.")
                    .size(tokens::FONT_HELP)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(tokens::SPACE_S);

            let mut remove: Option<usize> = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(tokens::SPACE_XS, tokens::SPACE_XS);
                for (i, pat) in self.cfg.exempt_patterns.iter().enumerate() {
                    if removable_chip(ui, pat) {
                        remove = Some(i);
                    }
                }

                if self.adding_exempt {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.new_exempt)
                            .hint_text("pattern")
                            .desired_width(140.0),
                    );
                    resp.request_focus();
                    let commit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if commit {
                        let pat = self.new_exempt.trim().to_string();
                        if !pat.is_empty() && !self.cfg.exempt_patterns.contains(&pat) {
                            self.cfg.exempt_patterns.push(pat);
                        }
                        self.new_exempt.clear();
                        self.adding_exempt = false;
                    } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.new_exempt.clear();
                        self.adding_exempt = false;
                    }
                } else if add_chip(ui, "+ add pattern") {
                    self.adding_exempt = true;
                }
            });
            if let Some(i) = remove {
                self.cfg.exempt_patterns.remove(i);
            }
        });

        });

        // ── Apply bar ─────────────────────────────────────────────────────
        let dirty = self.cfg != self.saved;
        let (discard, apply) = th::apply_bar(ui, dirty);
        if discard {
            self.cfg = self.saved.clone();
        }
        if apply {
            self.saved = self.cfg.clone();
            return Some(self.cfg.clone());
        }

        None
    }
}

/// Left-aligned form label in a fixed-width grid cell.
fn form_label(ui: &mut Ui, width: f32, text: &str) {
    ui.horizontal(|ui| {
        ui.set_min_width(width);
        ui.label(text);
    });
}

/// Inline helper text between two paired fields.
fn weak(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(crate::gui::theme::tokens::FONT_HELP)
            .color(ui.visuals().weak_text_color()),
    );
}

/// "RESTORE IN" cell: a thin progress bar plus the remaining seconds.
fn restore_progress(ui: &mut Ui, elapsed_low: f32, hysteresis: f32, color: egui::Color32) {
    use crate::gui::theme::{self as th, tokens};
    let remaining = (hysteresis - elapsed_low).max(0.0);
    let frac = if hysteresis > 0.0 {
        (elapsed_low / hysteresis).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(100.0, 8.0), egui::Sense::hover());
        let r = egui::CornerRadius::same(4);
        ui.painter().rect_filled(rect, r, th::tint(color, 46));
        if frac > 0.0 {
            let mut fill = rect;
            fill.set_width(rect.width() * frac);
            ui.painter().rect_filled(fill, r, color);
        }
        ui.add_space(tokens::SPACE_XS);
        let label = if remaining < 0.5 {
            "restoring…".to_string()
        } else {
            format!("{remaining:.1}s")
        };
        ui.label(
            RichText::new(label)
                .font(th::num_font(tokens::FONT_HELP))
                .color(ui.visuals().weak_text_color()),
        );
    });
}

/// A filled chip with a × affordance. Returns true when the user removes it.
fn removable_chip(ui: &mut Ui, label: &str) -> bool {
    use crate::gui::theme::{self as th, tokens};
    let text = format!("{label}  ✕");
    let btn = egui::Button::new(
        RichText::new(text)
            .size(tokens::FONT_LABEL)
            .color(ui.visuals().text_color()),
    )
    .fill(th::tint(ui.visuals().weak_text_color(), 34))
    .stroke(egui::Stroke::NONE)
    .corner_radius(egui::CornerRadius::same(9));
    ui.add(btn).on_hover_text("Remove pattern").clicked()
}

/// A dashed, low-emphasis "add" chip.
fn add_chip(ui: &mut Ui, label: &str) -> bool {
    use crate::gui::theme::tokens;
    let btn = egui::Button::new(
        RichText::new(label)
            .size(tokens::FONT_LABEL)
            .color(ui.visuals().weak_text_color()),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()))
    .corner_radius(egui::CornerRadius::same(9));
    ui.add(btn).clicked()
}
