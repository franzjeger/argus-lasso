//! Systemd owns service activation and uses the desktop's normal polkit agent.
//! GUI never executes as root; no shell command or sensor path comes from input.
#[derive(Default)]
pub struct SensorAccess {
    pending: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    status: String,
    latest: Option<crate::sensor_data::ExtendedSensors>,
    checked: Option<std::time::Instant>,
}
impl SensorAccess {
    pub fn show(&mut self, ui: &mut egui::Ui) {
        if let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.status = result.err().unwrap_or_default();
            self.pending = None;
            self.checked = None;
        }
        if self.checked.is_none_or(|t| t.elapsed().as_secs() >= 1) {
            self.latest = crate::sensor_data::read().ok();
            self.checked = Some(std::time::Instant::now());
        }
        let mut enabled = std::path::Path::new(crate::sensor_data::CACHE).exists();
        ui.heading("Extended sensor access");
        ui.label("Allows measured CPU package power and configured RAM speed when the hardware exposes them.");
        ui.add_space(12.0);
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Checkbox::new(&mut enabled, "Enable extended sensor access"),
            )
            .changed()
        {
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending = Some(rx);
            std::thread::spawn(move || {
                let result = std::process::Command::new("systemctl")
                    .args([
                        "--system",
                        if enabled { "start" } else { "stop" },
                        "argus-sensors.service",
                    ])
                    .output()
                    .map_err(|e| e.to_string())
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&o.stderr).trim().into())
                        }
                    });
                let _ = tx.send(result);
            });
        }
        if self.pending.is_some() {
            ui.label("Waiting for system authentication / service…");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(s) = &self.latest {
            ui.label(format!(
                "CPU power: {} · RAM: {}",
                s.cpu_power_w
                    .map(|w| format!("{w:.1} W"))
                    .unwrap_or_else(|| s.cpu_power_status.clone()),
                s.ram_speed_mts
                    .map(|v| format!("{v} MT/s"))
                    .unwrap_or_else(|| s.ram_speed_status.clone())
            ));
        }
        if enabled && self.latest.is_none() {
            ui.colored_label(egui::Color32::YELLOW, "Sensor data is stale or invalid. Disable and re-enable access to restart the reader.");
        }
        if !self.status.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.status);
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.small("A restricted system service reads sensors once per second. GUI and game remain unprivileged. Enabled until stopped or the system restarts; root access cannot add unsupported sensors.");
    }
}
