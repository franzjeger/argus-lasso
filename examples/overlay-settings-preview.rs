//! Isolated visual QA: no daemon, no policy changes and no user config writes.
#[path = "../src/gui/overlay_settings.rs"]
mod overlay_settings;
// The preview uses only part of the shared application theme.
#[allow(dead_code)]
#[path = "../src/gui/theme.rs"]
mod theme;
struct Preview {
    config: argus_ipc::OverlayConfig,
    open: bool,
    frames: u32,
    started: std::time::Instant,
}
impl eframe::App for Preview {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
    fn ui(&mut self, root_ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();
        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            theme::page_intro(
                ui,
                "Overlay",
                "Live customization, shared form spacing and readable section boundaries.",
            );
            let mut section = 0;
            theme::section_nav(
                ui,
                &mut section,
                &[(0, "Appearance"), (1, "Visible readings"), (2, "Graph")],
            );
            theme::card(ui, "In-game overlay", |ui| {
                overlay_settings::show(ui, &mut self.config, &mut self.open, 32);
            });
            ui.add_space(theme::tokens::SPACE_M);
            theme::card_hinted(
                ui,
                "Appearance",
                "Changes are saved while the game runs.",
                |ui| {
                    theme::form_row_w(ui, 220.0, "Text size", |ui| {
                        ui.add(egui::Slider::new(&mut self.config.font_px, 10..=24).suffix(" px"));
                    });
                    theme::form_row_w(ui, 220.0, "Screen corner", |ui| {
                        ui.label("Top left");
                    });
                },
            );
        });
        overlay_settings::window(
            &ctx,
            &mut self.config,
            &mut self.open,
            32,
            if std::env::args().any(|a| a == "--transparent") {
                0.35
            } else {
                1.0
            },
        );
        self.frames += 1;
        let hold = std::env::args().any(|a| a == "--hold");
        if hold && self.started.elapsed().as_secs() >= 12 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !hold && self.frames == 20 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let screenshot = ctx.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Screenshot { image, .. } = e {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = screenshot {
            let file = std::fs::File::create(if std::env::args().any(|a| a == "--light") {
                "diagnostics/polish-components-light.png"
            } else {
                "diagnostics/polish-components-dark.png"
            })
            .unwrap();
            let mut encoder = png::Encoder::new(file, image.size[0] as u32, image.size[1] as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            writer.write_image_data(&bytes).unwrap();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !hold && self.frames > 400 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "Argus overlay settings preview",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 650.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx
                .set_embed_viewports(std::env::args().any(|a| a == "--embedded"));
            theme::apply_theme(
                &cc.egui_ctx,
                1.0,
                &if std::env::args().any(|a| a == "--light") {
                    theme::AppTheme::AdwaitaLight
                } else {
                    theme::AppTheme::AdwaitaDark
                },
            );
            Ok(Box::new(Preview {
                config: argus_ipc::OverlayConfig {
                    show_overlay: true,
                    ..Default::default()
                },
                open: true,
                frames: 0,
                started: std::time::Instant::now(),
            }))
        }),
    )
}
