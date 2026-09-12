import re

with open("src/gui/dialogs.rs", "r") as f:
    text = f.read()

# For Steam games
steam_list_old = """                        egui::ScrollArea::vertical()
                            .max_height(400.0)
                            .show(ui, |ui| {
                                for (orig_i, (appid, name)) in &filtered {
                                    let sel = *selected == Some(*orig_i);
                                    let row = format!("{appid:<10} {name}");
                                    let resp = ui.selectable_label(sel, &row);
                                    if resp.double_clicked() {
                                        *selected = Some(*orig_i);
                                        accepted = true;
                                    } else if resp.clicked() {
                                        *selected = Some(*orig_i);
                                    }
                                }
                            });"""

steam_list_new = """                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(egui_extras::Column::exact(80.0))
                            .column(egui_extras::Column::remainder())
                            .min_scrolled_height(0.0)
                            .max_scroll_height(400.0)
                            .body(|mut body| {
                                for (orig_i, (appid, name)) in &filtered {
                                    body.row(24.0, |mut row| {
                                        let sel = *selected == Some(*orig_i);
                                        row.set_selected(sel);
                                        
                                        row.col(|ui| {
                                            ui.label(*appid);
                                        });
                                        row.col(|ui| {
                                            ui.label(*name);
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

text = text.replace(steam_list_old, steam_list_new)

# For Lutris games
lutris_list_old = """                        egui::ScrollArea::vertical()
                            .max_height(400.0)
                            .show(ui, |ui| {
                                for (orig_i, (name, slug)) in &filtered {
                                    let sel = *selected == Some(*orig_i);
                                    let row = format!("{name} ({slug})");
                                    let resp = ui.selectable_label(sel, &row);
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
                            .column(egui_extras::Column::exact(120.0))
                            .min_scrolled_height(0.0)
                            .max_scroll_height(400.0)
                            .body(|mut body| {
                                for (orig_i, (name, slug)) in &filtered {
                                    body.row(24.0, |mut row| {
                                        let sel = *selected == Some(*orig_i);
                                        row.set_selected(sel);
                                        
                                        row.col(|ui| {
                                            ui.label(*name);
                                        });
                                        row.col(|ui| {
                                            ui.label(egui::RichText::new(*slug).color(ui.visuals().weak_text_color()));
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

