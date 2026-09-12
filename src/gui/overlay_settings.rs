//! Gaming's overlay switch and customization submenu. No sensor work runs here.
use argus_ipc::{OverlayConfig, OverlayMetric};
use egui::Ui;
use std::collections::BTreeMap;

fn fields(
    ui: &mut Ui,
    colors: &mut BTreeMap<OverlayMetric, [u8; 3]>,
    entries: &mut [(&mut bool, &str, OverlayMetric)],
) -> bool {
    let mut changed = false;
    ui.columns(2, |columns| {
        for (index, (value, label, metric)) in entries.iter_mut().enumerate() {
            columns[index % 2].horizontal(|ui| {
                changed |= color_picker(ui, colors, *metric);
                changed |= ui.checkbox(value, *label).changed();
            });
        }
    });
    changed
}
fn color_picker(
    ui: &mut Ui,
    colors: &mut BTreeMap<OverlayMetric, [u8; 3]>,
    metric: OverlayMetric,
) -> bool {
    let mut changed = false;
    let mut color = colors
        .get(&metric)
        .copied()
        .unwrap_or_else(|| metric.default_color());
    if ui
        .color_edit_button_srgb(&mut color)
        .on_hover_text("Color for this value")
        .changed()
    {
        colors.insert(metric, color);
        changed = true;
    }
    if ui
        .add_enabled(colors.contains_key(&metric), egui::Button::new("↺").small())
        .on_hover_text("Reset this value to the component palette")
        .clicked()
    {
        colors.remove(&metric);
        changed = true;
    }
    changed
}

pub fn show(ui: &mut Ui, config: &mut OverlayConfig, open: &mut bool, _cpu_count: u32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .toggle_value(&mut config.show_overlay, "Enable in-game overlay")
            .changed()
        {
            changed = true;
            *open = config.show_overlay;
        }
        if ui
            .add_enabled(config.show_overlay, egui::Button::new("Customize overlay…"))
            .clicked()
        {
            *open = true;
        }
    });
    if !config.show_overlay {
        *open = false;
    }
    ui.small("Choose which readings appear and how they look in Customize overlay.");
    ui.small("Steam launch option: ARGUS_LASSO_HUD=1 %command%");
    changed
}
pub fn window(
    ctx: &egui::Context,
    config: &mut OverlayConfig,
    open: &mut bool,
    cpu_count: u32,
    window_opacity: f32,
) -> bool {
    if !*open || !config.show_overlay {
        return false;
    }
    let mut changed = false;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("gaming_overlay_settings"),
        egui::ViewportBuilder::default().with_title("Argus — Overlay settings").with_transparent(true).with_app_id("argus-lasso")
            .with_inner_size([660.0, 730.0]).with_min_inner_size([560.0, 400.0]),
        |root_ui, class| {
            super::theme::apply_viewport_opacity(root_ui, window_opacity);
            if root_ui.input(|i| i.viewport().close_requested()) { *open = false; }
            let mut contents = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
            ui.label("Choose the values you want. Changes are saved and applied while the game runs.");
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Appearance & position").default_open(true).show(ui, |ui| {
                egui::Grid::new("overlay_appearance").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Text size (px)");
                    changed |= ui.add(egui::Slider::new(&mut config.font_px, 10..=24)).changed();
                    ui.end_row();
                    ui.label("Screen corner");
                    let names = ["Top left", "Top right", "Bottom left", "Bottom right"];
                    egui::ComboBox::from_id_salt("overlay_anchor").selected_text(names[config.anchor.min(3) as usize]).show_ui(ui, |ui| {
                        for (i,name) in names.iter().enumerate() {
                            changed |= ui.selectable_value(&mut config.anchor, i as u8, *name).changed();
                        }
                    });
                    ui.end_row();
                    ui.label("Horizontal / vertical offset (px)");
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(&mut config.offset_x).range(0..=8192).prefix("X ")).changed();
                        changed |= ui.add(egui::DragValue::new(&mut config.offset_y).range(0..=8192).prefix("Y ")).changed();
                    });
                    ui.end_row();
                    ui.label("Inner padding (px)");
                    changed |= ui.add(egui::Slider::new(&mut config.margin, 0..=32)).changed();
                    ui.end_row();
                    ui.label("Label color");
                    let mut text = [config.text_color.0, config.text_color.1, config.text_color.2];
                    if ui.color_edit_button_srgb(&mut text).changed() {
                        (config.text_color.0,config.text_color.1,config.text_color.2)=(text[0],text[1],text[2]); changed=true;
                    }
                    ui.end_row();
                    ui.label("Text opacity (%)");
                    changed |= opacity(ui, &mut config.text_color.3);
                    ui.end_row();
                    ui.label("Background color");
                    let mut bg = [config.bg_color.0, config.bg_color.1, config.bg_color.2];
                    if ui.color_edit_button_srgb(&mut bg).changed() {
                        (config.bg_color.0,config.bg_color.1,config.bg_color.2)=(bg[0],bg[1],bg[2]); changed=true;
                    }
                    ui.end_row();
                    ui.label("Background opacity (%)");
                    changed |= opacity(ui, &mut config.bg_color.3);
                    ui.end_row();
                });
                changed |= ui.checkbox(&mut config.section_dividers, "Subtle section dividers").changed();
                if ui.button("Restore recommended colors").on_hover_text("Restores label and value colors. Keeps size, position and visibility choices.").clicked() {
                    config.value_colors.clear();
                    let base = OverlayConfig::default().text_color;
                    config.text_color.0 = base.0; config.text_color.1 = base.1; config.text_color.2 = base.2;
                    changed = true;
                }
                ui.small("Pick a value's color below. ↺ restores its default. Background remains fully transparent at 0%.");
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("GPU & VRAM").default_open(true).show(ui, |ui| {
                changed |= ui.checkbox(&mut config.show_gpu,"Show GPU readings").changed();
                ui.add_enabled_ui(config.show_gpu, |ui| {
                    let f=&mut config.fields;
                    changed |= fields(ui, &mut config.value_colors, &mut [(&mut f.gpu_name,"Model name",OverlayMetric::GpuName),(&mut f.gpu_usage,"Load (%)",OverlayMetric::GpuUsage),(&mut f.gpu_temp,"Temperature (°C)",OverlayMetric::GpuTemp),(&mut f.gpu_power,"Power (W)",OverlayMetric::GpuPower),(&mut f.gpu_core_clock,"Core clock (MHz)",OverlayMetric::GpuCoreClock),(&mut f.gpu_mem_clock,"Memory clock (MHz)",OverlayMetric::GpuMemClock),(&mut f.gpu_fan,"Fan (%)",OverlayMetric::GpuFan),(&mut f.vram,"VRAM used / total",OverlayMetric::Vram)]);
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("CPU").default_open(true).show(ui, |ui| {
                changed |= ui.checkbox(&mut config.show_cpu,"Show CPU readings").changed();
                ui.add_enabled_ui(config.show_cpu, |ui| {
                    let f=&mut config.fields;
                    changed |= fields(ui, &mut config.value_colors, &mut [(&mut f.cpu_name,"Model name",OverlayMetric::CpuName),(&mut f.cpu_usage,"Total load (%)",OverlayMetric::CpuUsage),(&mut f.cpu_temp,"Temperature (°C)",OverlayMetric::CpuTemp),(&mut f.cpu_power,"Power (W)",OverlayMetric::CpuPower),(&mut f.cpu_frequency,"Frequency (MHz)",OverlayMetric::CpuFrequency)]);
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Individual CPU threads").show(ui, |ui| {
                changed |= ui.checkbox(&mut config.show_cores,"Show logical CPUs").changed();
                ui.add_enabled_ui(config.show_cores, |ui| {
                    let f=&mut config.fields;
                    changed |= fields(ui, &mut config.value_colors, &mut [(&mut f.thread_usage,"Load (%)",OverlayMetric::ThreadUsage),(&mut f.thread_frequency,"Frequency (MHz)",OverlayMetric::ThreadFrequency),(&mut f.physical_core_id,"Physical core ID",OverlayMetric::PhysicalCoreId)]);
                    ui.small("CPU 04 is Linux logical CPU 4. Physical core IDs are optional: two CPU threads can share one core.");
                    ui.horizontal(|ui| { changed |= color_picker(ui, &mut config.value_colors, OverlayMetric::ThreadId); ui.label("CPU label color"); });
                    ui.horizontal(|ui| {
                        if ui.button("All CPUs").clicked() { config.hidden_cpu_ids.clear(); changed=true; }
                        if ui.button("No CPUs").clicked() { config.hidden_cpu_ids=(0..cpu_count).collect(); changed=true; }
                    });
                    egui::Grid::new("overlay_thread_selection").num_columns(4).show(ui, |ui| {
                        for id in 0..cpu_count {
                            let mut visible=!config.hidden_cpu_ids.contains(&id);
                            if ui.checkbox(&mut visible,format!("CPU {id:02}")).changed() {
                                if visible { config.hidden_cpu_ids.retain(|c| *c!=id); }
                                else { config.hidden_cpu_ids.push(id); }
                                changed=true;
                            }
                            if id%4==3 {ui.end_row();}
                        }
                    });
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("RAM").show(ui, |ui| {
                changed |= ui.checkbox(&mut config.show_ram,"Show RAM readings").changed();
                ui.add_enabled_ui(config.show_ram, |ui| {
                    changed |= fields(ui, &mut config.value_colors, &mut [(&mut config.fields.ram_usage,"Used / total (GiB)",OverlayMetric::RamUsage),(&mut config.fields.ram_speed,"Configured speed (MT/s)",OverlayMetric::RamSpeed)]);
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("FPS & frametimes").show(ui, |ui| {
                changed |= ui.checkbox(&mut config.show_fps,"Show frame statistics").changed();
                ui.add_enabled_ui(config.show_fps, |ui| {
                    let f=&mut config.fields;
                    changed |= fields(ui, &mut config.value_colors, &mut [(&mut f.fps,"FPS",OverlayMetric::Fps),(&mut f.frametime,"Frametime (ms)",OverlayMetric::Frametime),(&mut f.average_fps,"Average FPS",OverlayMetric::AverageFps),(&mut f.low_1,"1% low",OverlayMetric::Low1)]);
                });
                ui.horizontal(|ui| { changed |= color_picker(ui, &mut config.value_colors, OverlayMetric::Graph); changed |= ui.checkbox(&mut config.show_graph,"Frametime graph").changed(); });
                ui.add_enabled_ui(config.show_graph, |ui| {
            ui.horizontal(|ui| {
                ui.label("Graph refresh (Hz)");
                changed |= ui.add(egui::Slider::new(&mut config.graph_hz, 30..=120)).changed();
                ui.label("Range (ms)");
                changed |= ui.add(egui::Slider::new(&mut config.graph_max_ms, 5..=100)).changed();
            });
            ui.small("Graph: last 5 seconds; each bin preserves its slowest frame. Text refreshes at 4 Hz.");
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Game & Argus status").show(ui, |ui| {
                let f=&mut config.fields;
                changed |= fields(ui, &mut config.value_colors, &mut [(&mut f.parked,"Parked thread count",OverlayMetric::Parked),(&mut f.argus_mode,"Active Argus mode",OverlayMetric::ArgusMode),(&mut f.unavailable_reason,"Explain unavailable sensors",OverlayMetric::UnavailableReason)]);
                changed |= fields(ui, &mut config.value_colors, &mut [
                    (&mut f.game_name,"Application / PID",OverlayMetric::GameName),
                    (&mut f.game_profile,"Launcher profile",OverlayMetric::GameProfile),
                    (&mut f.game_affinity,"Main thread CPU affinity",OverlayMetric::GameAffinity),
                    (&mut f.game_priority,"Process nice level",OverlayMetric::GamePriority),
                    (&mut f.probalance,"ProBalance intervention",OverlayMetric::Probalance)]);
                ui.small("Selecting a reading does not grant sensor access. Unsupported or inaccessible readings display —.");
            });
            };
            if class == egui::ViewportClass::EmbeddedWindow {
                egui::Window::new("Overlay settings").open(open).vscroll(true).show(root_ui.ctx(), contents);
            } else {
                egui::CentralPanel::default().show_inside(root_ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, &mut contents);
                });
            }
        });
    changed
}
fn opacity(ui: &mut Ui, alpha: &mut u8) -> bool {
    let mut percent = *alpha as f32 * 100.0 / 255.0;
    if ui
        .add(egui::Slider::new(&mut percent, 0.0..=100.0))
        .changed()
    {
        *alpha = (percent * 255.0 / 100.0).round() as u8;
        true
    } else {
        false
    }
}
