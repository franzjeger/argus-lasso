//! Rules tab: compact toolbar + rule table (design 2d).

use std::sync::{Arc, Mutex};

use egui::RichText;
use egui_extras::{Column, TableBuilder};

use crate::gui::dialogs::{RuleEditDialog, RulePresetsDialog};
use crate::gui::theme::{self, tokens};
use crate::rules::{Rule, RuleEngine};

/// Result from a background file-dialog thread.
enum FileDialogResult {
    /// Export finished — carries a status string.
    ExportDone(String),
    /// Import finished — carries parsed rules or an error string.
    ImportDone(Result<Vec<crate::config::RuleConfig>, String>),
}

pub struct RulesTab {
    pub selected_rule_id: Option<String>,
    pub edit_dialog: Option<RuleEditDialog>,
    pub presets_dialog: Option<RulePresetsDialog>,
    pub status: String,
    pub profile_name: String,
    pub selected_profile: String,
    pub test_input: String,
    /// Receives results from background file-dialog threads.
    file_rx: std::sync::mpsc::Receiver<FileDialogResult>,
    file_tx: std::sync::mpsc::Sender<FileDialogResult>,
    // Confirm dialog state
    confirm_delete_rule: bool,
    confirm_load_profile: bool,
    confirm_delete_profile: bool,
}

impl RulesTab {
    pub fn new() -> Self {
        let (file_tx, file_rx) = std::sync::mpsc::channel();
        Self {
            selected_rule_id: None,
            edit_dialog: None,
            presets_dialog: None,
            status: String::new(),
            profile_name: String::new(),
            selected_profile: String::new(),
            test_input: String::new(),
            file_rx,
            file_tx,
            confirm_delete_rule: false,
            confirm_load_profile: false,
            confirm_delete_profile: false,
        }
    }

    pub fn open_add_dialog(&mut self, template: Option<Rule>) {
        self.edit_dialog = Some(RuleEditDialog::new(
            template.unwrap_or_else(Rule::new_empty),
        ));
    }

    /// Returns `true` if rule_profiles in config changed (needs save).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rule_engine: &Arc<Mutex<RuleEngine>>,
        on_rules_changed: &mut bool,
        opacity: f32,
        proc_names: &[String],
        rule_profiles: &mut std::collections::HashMap<String, Vec<crate::config::RuleConfig>>,
        on_profiles_changed: &mut bool,
    ) {
        // ── Drain background file-dialog results ───────────────────────────
        while let Ok(result) = self.file_rx.try_recv() {
            match result {
                FileDialogResult::ExportDone(msg) => {
                    self.status = msg;
                }
                FileDialogResult::ImportDone(Ok(configs)) => {
                    let count = configs.len();
                    if let Ok(mut re) = rule_engine.lock() {
                        for cfg in configs {
                            re.add_rule(Rule::from_config(&cfg));
                        }
                    }
                    *on_rules_changed = true;
                    self.status = format!("Imported {count} rules.");
                }
                FileDialogResult::ImportDone(Err(e)) => {
                    self.status = e;
                }
            }
        }

        let rules: Vec<Rule> = rule_engine
            .lock()
            .map(|re| re.get_rules().to_vec())
            .unwrap_or_default();

        let selected_id = self.selected_rule_id.clone();
        let mut new_sel: Option<String> = selected_id.clone();
        let mut open_edit: Option<Rule> = None;
        let mut delete_rule_id: Option<String> = None;
        let mut toggle_rule_id: Option<String> = None;

        // ── Toolbar ────────────────────────────────────────────────────────
        self.toolbar(ui, rule_engine, rule_profiles, on_profiles_changed, &rules);

        if !self.status.is_empty() {
            ui.label(
                RichText::new(&self.status)
                    .size(tokens::FONT_HELP)
                    .color(ui.visuals().weak_text_color()),
            );
        }

        ui.add_space(tokens::SPACE_XS);

        // ── Empty state with a call to action ──────────────────────────────
        // Mockup 4a: icon, title, one paragraph capped at ~420px, and two
        // actions — centred in the panel's remaining height rather than
        // stacked under the toolbar with dead space below.
        if rules.is_empty() {
            let s = theme::sem(ui);
            const BODY_W: f32 = 420.0;
            // Centre vertically in what is left of the panel. The block is
            // roughly 150px tall; half the slack above it puts it on the
            // optical centre without measuring twice.
            let slack = (ui.available_height() - 150.0).max(0.0);
            ui.add_space(slack * 0.5);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("☰")
                        .size(30.0)
                        .color(ui.visuals().weak_text_color().gamma_multiply(0.6)),
                );
                ui.add_space(tokens::SPACE_XS);
                ui.label(theme::bold(ui, "No rules yet", tokens::FONT_HEADING));
                ui.add_space(tokens::SPACE_XS);
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(BODY_W, 0.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(
                                "Rules set affinity, nice and I/O priority automatically on \
                                 processes matching a pattern. Right-click a process in the \
                                 Processes tab and choose \"Add rule\", or start from a template.",
                            )
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                        );
                    },
                );
                ui.add_space(tokens::SPACE_M);
                // vertical_centered centres each child *allocation*, so the
                // row has to be allocated at exactly its own width. A plain
                // horizontal row — or a Frame wrapping one — takes the full
                // width and lands hard left, and a guessed fixed width leaves
                // slack that shows up as an offset. So measure the two labels.
                const NEW: &str = "+ New rule";
                const BROWSE: &str = "Browse templates";
                let row_w = {
                    let text_w = |t: &str| {
                        let font = if t == NEW {
                            theme::bold_font(tokens::FONT_BODY)
                        } else {
                            egui::TextStyle::Button.resolve(ui.style())
                        };
                        ui.painter()
                            .layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    };
                    let pad = ui.spacing().button_padding.x * 2.0;
                    text_w(NEW) + text_w(BROWSE) + pad * 2.0 + ui.spacing().item_spacing.x
                };
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(row_w, 0.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let btn = egui::Button::new(
                            RichText::new(NEW)
                                .color(s.on_accent)
                                .font(theme::bold_font(tokens::FONT_BODY)),
                        )
                        .fill(s.accent);
                        if ui.add(btn).clicked() {
                            self.open_add_dialog(None);
                        }
                        if ui.button(BROWSE).clicked() {
                            self.presets_dialog = Some(RulePresetsDialog::new());
                        }
                    },
                );
            });
        } else {
            // ── Rule table ─────────────────────────────────────────────────
            let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
            let text_color = ui.visuals().text_color();
            let dim_color = ui.visuals().weak_text_color();

            // Column widths proportional to available width. id_salt includes
            // avail_w so egui_extras re-initialises columns on window resize.
            let avail_w = ui.available_width() - 2.0;
            let table_left = ui.min_rect().left();
            let table_right = table_left + avail_w;
            let col_pattern = (avail_w * 0.13).clamp(40.0, 200.0);
            let col_match = (avail_w * 0.08).clamp(40.0, 120.0);
            let col_aff = (avail_w * 0.11).clamp(40.0, 160.0);
            let col_nice = (avail_w * 0.05).clamp(30.0, 70.0);
            let col_io = (avail_w * 0.08).clamp(40.0, 120.0);

            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0_f32, border_color))
                .inner_margin(egui::Margin::same(1))
                .show(ui, |ui| {
                    TableBuilder::new(ui)
                        .id_salt(avail_w as i32) // reset stored widths when window resizes
                        .striped(true)
                        .resizable(true)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(Column::exact(38.0))
                        .column(Column::remainder())
                        .column(Column::initial(col_pattern).clip(true))
                        .column(Column::initial(col_match).clip(true))
                        .column(Column::initial(col_aff).clip(true))
                        .column(Column::initial(col_nice).clip(true))
                        .column(Column::initial(col_io).clip(true))
                        .column(Column::exact(96.0))
                        .min_scrolled_height(120.0)
                        .header(24.0, |mut hdr| {
                            for label in [
                                "ON", "NAME", "PATTERN", "MATCH", "AFFINITY", "NICE", "I/O", "",
                            ] {
                                hdr.col(|ui| {
                                    ui.label(theme::header_text(ui, label, false));
                                });
                            }
                        })
                        .body(|mut body| {
                            for rule in &rules {
                                let rule_id = rule.rule_id.clone();
                                let is_sel = selected_id.as_deref() == Some(&rule.rule_id);
                                let row_color = if rule.enabled { text_color } else { dim_color };
                                let mut row_hovered = false;

                                body.row(tokens::ROW_H, |mut row| {
                                    row.set_selected(is_sel);

                                    let (_, r0) = row.col(|ui| {
                                        // Hover is measured on the full row band so the
                                        // action buttons don't flicker when aimed at.
                                        let band = ui.max_rect();
                                        row_hovered = ui
                                            .ctx()
                                            .pointer_latest_pos()
                                            .map(|p| {
                                                p.y >= band.top()
                                                    && p.y <= band.bottom()
                                                    && p.x >= table_left
                                                    && p.x <= table_right
                                            })
                                            .unwrap_or(false);
                                        let mut on = rule.enabled;
                                        if theme::toggle(ui, &mut on) {
                                            toggle_rule_id = Some(rule_id.clone());
                                        }
                                    });
                                    let (_, r1) = row.col(|ui| {
                                        ui.label(RichText::new(&rule.name).color(row_color));
                                    });
                                    let (_, r2) = row.col(|ui| {
                                        ui.label(RichText::new(&rule.pattern).color(row_color));
                                    });
                                    let (_, r3) = row.col(|ui| {
                                        theme::badge_outline(ui, &rule.match_type);
                                    });
                                    let (_, r4) = row.col(|ui| {
                                        ui.label(
                                            RichText::new(rule.affinity.as_deref().unwrap_or("—"))
                                                .color(row_color),
                                        );
                                    });
                                    // §2: numeric cell — monospace, right-aligned.
                                    let (_, r5) = row.col(|ui| {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let txt = rule
                                                    .nice
                                                    .map(|n| n.to_string())
                                                    .unwrap_or_else(|| "—".into());
                                                ui.label(
                                                    RichText::new(txt)
                                                        .font(theme::num_font(tokens::FONT_BODY))
                                                        .color(row_color),
                                                );
                                            },
                                        );
                                    });
                                    let (_, r6) = row.col(|ui| {
                                        let txt = match (rule.ionice_class, rule.ionice_level) {
                                            (Some(c), Some(l)) => format!("cls {c} · {l}"),
                                            (Some(c), None) => format!("cls {c}"),
                                            (None, Some(l)) => format!("lvl {l}"),
                                            (None, None) => "—".into(),
                                        };
                                        ui.label(
                                            RichText::new(txt)
                                                .font(theme::num_font(tokens::FONT_SMALL))
                                                .color(row_color),
                                        );
                                    });
                                    // Row-level actions: only on hover/selection.
                                    let (_, r7) = row.col(|ui| {
                                        if row_hovered || is_sel {
                                            ui.spacing_mut().item_spacing.x = tokens::SPACE_XS;
                                            if ui
                                                .small_button("Edit")
                                                .on_hover_text("Edit rule")
                                                .clicked()
                                            {
                                                new_sel = Some(rule_id.clone());
                                                open_edit = Some(rule.clone());
                                            }
                                            if ui
                                                .small_button("Delete")
                                                .on_hover_text("Delete rule")
                                                .clicked()
                                            {
                                                delete_rule_id = Some(rule_id.clone());
                                            }
                                        }
                                    });

                                    let clicked = r0.clicked()
                                        || r1.clicked()
                                        || r2.clicked()
                                        || r3.clicked()
                                        || r4.clicked()
                                        || r5.clicked()
                                        || r6.clicked()
                                        || r7.clicked();
                                    let doubled = r0.double_clicked()
                                        || r1.double_clicked()
                                        || r2.double_clicked()
                                        || r3.double_clicked()
                                        || r4.double_clicked()
                                        || r5.double_clicked()
                                        || r6.double_clicked()
                                        || r7.double_clicked();

                                    if doubled {
                                        new_sel = Some(rule_id.clone());
                                        open_edit = Some(rule.clone());
                                    } else if clicked {
                                        new_sel = Some(rule_id.clone());
                                    }
                                });
                            }
                        });
                });
        }

        self.selected_rule_id = new_sel;
        if let Some(rule) = open_edit {
            self.edit_dialog = Some(RuleEditDialog::new(rule));
        }
        if let Some(id) = delete_rule_id {
            self.selected_rule_id = Some(id);
            self.confirm_delete_rule = true;
        }
        if let Some(id) = toggle_rule_id {
            if let Ok(mut re) = rule_engine.lock() {
                if let Some(r) = re.get_rules_mut().iter_mut().find(|r| r.rule_id == id) {
                    r.enabled = !r.enabled;
                    *on_rules_changed = true;
                }
            }
        }

        // ── Confirm dialogs ────────────────────────────────────────────────
        if self.confirm_delete_rule {
            let rule_name = self
                .selected_rule_id
                .as_ref()
                .and_then(|id| rules.iter().find(|r| &r.rule_id == id))
                .map(|r| r.name.as_str())
                .unwrap_or("this rule");
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new("Delete process rule")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Delete rule '{rule_name}'?"));
                    ui.add_space(tokens::SPACE_S);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            confirmed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });
            if confirmed {
                if let (Some(id), Ok(mut re)) = (self.selected_rule_id.clone(), rule_engine.lock())
                {
                    re.remove_rule(&id);
                    *on_rules_changed = true;
                    self.selected_rule_id = None;
                }
                self.confirm_delete_rule = false;
            } else if cancelled {
                self.confirm_delete_rule = false;
            }
        }

        if self.confirm_load_profile {
            let profile = self.selected_profile.clone();
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new("Load rule profile")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Load profile '{profile}'?\nThis replaces all current rules."
                    ));
                    ui.add_space(tokens::SPACE_S);
                    ui.horizontal(|ui| {
                        if ui.button("Load").clicked() {
                            confirmed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });
            if confirmed {
                if let Some(rules) = rule_profiles.get(&self.selected_profile) {
                    if let Ok(mut re) = rule_engine.lock() {
                        re.clear_rules();
                        for cfg in rules {
                            re.add_rule(crate::rules::Rule::from_config(cfg));
                        }
                    }
                    *on_rules_changed = true;
                    self.status = format!("Loaded profile '{}'.", self.selected_profile);
                }
                self.confirm_load_profile = false;
            } else if cancelled {
                self.confirm_load_profile = false;
            }
        }

        if self.confirm_delete_profile {
            let profile = self.selected_profile.clone();
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new("Delete rule profile")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Delete profile '{profile}'?"));
                    ui.add_space(tokens::SPACE_S);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            confirmed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });
            if confirmed {
                rule_profiles.remove(&self.selected_profile);
                self.selected_profile.clear();
                *on_profiles_changed = true;
                self.status = "Profile deleted.".into();
                self.confirm_delete_profile = false;
            } else if cancelled {
                self.confirm_delete_profile = false;
            }
        }

        // ── Dialogs ────────────────────────────────────────────────────────
        if let Some(ref mut dlg) = self.edit_dialog {
            if let Some(result) = dlg.show(ctx, opacity, proc_names) {
                self.edit_dialog = None;
                if let Some(rule) = result {
                    if let Ok(mut re) = rule_engine.lock() {
                        let exists = re.get_rules().iter().any(|r| r.rule_id == rule.rule_id);
                        if exists {
                            re.update_rule(rule);
                        } else {
                            re.add_rule(rule);
                        }
                        *on_rules_changed = true;
                    }
                }
            }
        }

        if let Some(ref mut dlg) = self.presets_dialog {
            if let Some(result) = dlg.show(ctx, opacity) {
                self.presets_dialog = None;
                if let Some(rule) = result {
                    self.edit_dialog = Some(RuleEditDialog::new(rule));
                }
            }
        }
    }

    /// Slim toolbar: primary action, templates, live pattern test, profile
    /// picker and an overflow menu for the rare file/profile operations.
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        rule_engine: &Arc<Mutex<RuleEngine>>,
        rule_profiles: &mut std::collections::HashMap<String, Vec<crate::config::RuleConfig>>,
        on_profiles_changed: &mut bool,
        rules: &[Rule],
    ) {
        let s = theme::sem(ui);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(0, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let new_btn = egui::Button::new(
                        RichText::new("+ New rule")
                            .color(s.on_accent)
                            .font(theme::bold_font(tokens::FONT_BODY)),
                    )
                    .fill(s.accent);
                    if ui.add(new_btn).clicked() {
                        self.open_add_dialog(None);
                    }
                    if ui.button("Templates ▾").clicked() {
                        self.presets_dialog = Some(RulePresetsDialog::new());
                    }

                    ui.add_space(tokens::SPACE_S);

                    ui.add(
                        egui::TextEdit::singleline(&mut self.test_input)
                            .hint_text("🔍  Test pattern")
                            .desired_width(150.0),
                    );
                    if !self.test_input.is_empty() {
                        let test_lower = self.test_input.to_lowercase();
                        let matches: Vec<String> = rules
                            .iter()
                            .filter(|r| r.enabled && r.matches(&self.test_input, &test_lower))
                            .map(|r| r.name.clone())
                            .collect();
                        if matches.is_empty() {
                            ui.label(
                                RichText::new("No rules match")
                                    .size(tokens::FONT_HELP)
                                    .color(ui.visuals().weak_text_color()),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!("✓ Matches «{}»", matches.join(", ")))
                                    .size(tokens::FONT_HELP)
                                    .color(s.ok),
                            );
                        }
                    }

                    // Spacer → profile picker + overflow menu on the right.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.overflow_menu(ui, rule_engine, rule_profiles, on_profiles_changed);
                        self.profile_picker(ui, rule_profiles);
                    });
                });
            });
    }

    fn profile_picker(
        &mut self,
        ui: &mut egui::Ui,
        rule_profiles: &std::collections::HashMap<String, Vec<crate::config::RuleConfig>>,
    ) {
        let mut profile_names: Vec<String> = rule_profiles.keys().cloned().collect();
        profile_names.sort();
        let current = if self.selected_profile.is_empty() {
            "Profile: —".to_string()
        } else {
            format!("Profile: {}", self.selected_profile)
        };
        egui::ComboBox::from_id_salt("profile_picker")
            .selected_text(current)
            .width(160.0)
            .show_ui(ui, |ui| {
                if profile_names.is_empty() {
                    ui.label(
                        RichText::new("No saved profiles")
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                for name in &profile_names {
                    let picked = self.selected_profile == *name;
                    if ui.selectable_label(picked, name.as_str()).clicked() && !picked {
                        self.selected_profile = name.clone();
                        self.confirm_load_profile = true;
                    }
                }
            });
    }

    fn overflow_menu(
        &mut self,
        ui: &mut egui::Ui,
        rule_engine: &Arc<Mutex<RuleEngine>>,
        rule_profiles: &mut std::collections::HashMap<String, Vec<crate::config::RuleConfig>>,
        on_profiles_changed: &mut bool,
    ) {
        ui.menu_button("Import / export ▾", |ui| {
            ui.set_min_width(210.0);
            if ui.button("Export rules…").clicked() {
                self.export_rules(rule_engine);
                ui.close();
            }
            if ui.button("Import rules…").clicked() {
                self.import_rules();
                ui.close();
            }
            ui.separator();
            ui.label(
                RichText::new("SAVE AS PROFILE")
                    .size(tokens::FONT_LABEL)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.profile_name)
                    .hint_text("Profile name…")
                    .desired_width(190.0),
            );
            let can_save = !self.profile_name.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("Save profile"))
                .clicked()
            {
                let name = self.profile_name.trim().to_string();
                let saved = rule_engine
                    .lock()
                    .map(|re| re.to_config_list())
                    .unwrap_or_default();
                rule_profiles.insert(name.clone(), saved);
                self.selected_profile = name.clone();
                self.profile_name.clear();
                *on_profiles_changed = true;
                self.status = format!("Saved as profile '{name}'.");
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(
                    !self.selected_profile.is_empty(),
                    egui::Button::new("Delete profile"),
                )
                .clicked()
            {
                self.confirm_delete_profile = true;
                ui.close();
            }
        });
    }

    fn import_rules(&mut self) {
        let tx = self.file_tx.clone();
        std::thread::spawn(move || {
            let path = match crate::file_dialog::open("*.json") {
                Some(p) => p,
                None => return,
            };
            let result = match std::fs::read_to_string(&path) {
                Err(e) => Err(format!("Read error: {e}")),
                Ok(s) => serde_json::from_str::<Vec<crate::config::RuleConfig>>(&s)
                    .map_err(|e| format!("Parse error: {e}")),
            };
            tx.send(FileDialogResult::ImportDone(result)).ok();
        });
    }

    fn export_rules(&mut self, rule_engine: &Arc<Mutex<RuleEngine>>) {
        let rules = rule_engine
            .lock()
            .map(|re| re.to_config_list())
            .unwrap_or_default();
        let tx = self.file_tx.clone();
        std::thread::spawn(move || {
            let path = match crate::file_dialog::save("argus_lasso_rules.json", "*.json") {
                Some(p) => p,
                None => return,
            };
            let msg = match serde_json::to_string_pretty(&rules) {
                Err(e) => format!("Serialise error: {e}"),
                Ok(text) => match std::fs::write(&path, &text) {
                    Ok(_) => format!("Exported {} rules.", rules.len()),
                    Err(e) => format!("Export failed: {e}"),
                },
            };
            tx.send(FileDialogResult::ExportDone(msg)).ok();
        });
    }
}
