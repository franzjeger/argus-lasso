//! Isolated GUI / portal QA. --shortcut requests normal desktop permission.
#[path = "../src/game_benchmark.rs"]
mod game_benchmark;
#[path = "../src/sensor_access.rs"]
mod sensor_access;
#[path = "../src/sensor_data.rs"]
mod sensor_data;
struct Preview {
    bench: game_benchmark::GameBenchmark,
    sensors: sensor_access::SensorAccess,
    last: String,
    started: std::time::Instant,
}
impl eframe::App for Preview {
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.bench.show(ui);
                ui.separator();
                self.sensors.show(ui);
            });
        });
        if self.last != self.bench.status() {
            self.last = self.bench.status().to_owned();
            println!("{}", self.last);
        }
        if self.started.elapsed().as_secs() > 180 {
            root.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        root.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}
fn main() -> eframe::Result {
    let mut bench = game_benchmark::GameBenchmark::default();
    if std::env::args().any(|a| a == "--shortcut") {
        bench.register();
    }
    eframe::run_native(
        "Argus recording and sensors QA",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default()
                .with_app_id("io.github.franzjeger.ArgusLasso")
                .with_inner_size([850.0, 650.0]),
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Preview {
                bench,
                sensors: Default::default(),
                last: String::new(),
                started: std::time::Instant::now(),
            }))
        }),
    )
}
