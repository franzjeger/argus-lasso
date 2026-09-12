import re

with open("src/gui/dialogs.rs", "r") as f:
    text = f.read()

lutris_list_old = """                        egui::ScrollArea::vertical()
                            .max_height(400.0)
                            .show(ui, |ui| {
                                for (orig_i, (_, label)) in &filtered {
                                    let sel = *selected == Some(*orig_i);
                                    let resp = ui.selectable_label(sel, label.as_str());
                                    if resp.double_clicked() {
                                        *selected = Some(*orig_i);
                                        accepted = true;
                                    } else if resp.clicked() {
                                        *selected = Some(*orig_i);
                                    }
                                }
                            });"""

lutris_list_new = """                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(egui_extras::Column::remainder())
                            .min_scrolled_height(0.0)
                            .max_scroll_height(400.0)
                            .body(|mut body| {
                                for (orig_i, (_, label)) in &filtered {
                                    body.row(24.0, |mut row| {
                                        let sel = *selected == Some(*orig_i);
                                        row.set_selected(sel);
                                        
                                        row.col(|ui| {
                                            ui.label(label.as_str());
                                        });
                                        
                                        let resp = row.response();
                                        if resp.double_clicked() {
                                            *selected = Some(*orig_i);
                                            accepted = true;
                                        } else if resp.clicked() {
                                            *selected = Some(*orig_i);
                                        }
                                    });
                                }
                            });"""

text = text.replace(lutris_list_old, lutris_list_new)

with open("src/gui/dialogs.rs", "w") as f:
    f.write(text)

