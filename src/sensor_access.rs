//! Systemd owns service activation and uses the desktop's normal polkit agent.
//! GUI never executes as root; no shell command or sensor path comes from input.

use crate::gui::theme::{self, tokens};
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
        theme::help_text(
            ui,
            "Read CPU package power and RAM speed when supported by your hardware.",
        );
        ui.add_space(tokens::SPACE_S);
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
                let output = std::process::Command::new("systemctl")
                    .args(service_command(enabled))
                    .output();
                let _ = tx.send(service_outcome(output));
            });
        }
        if self.pending.is_some() {
            ui.label("Waiting for system authentication / service…");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(s) = &self.latest {
            ui.label(readings_line(s));
        }
        if enabled && self.latest.is_none() {
            ui.colored_label(theme::sem(ui).warning, "Sensor data is stale or invalid. Disable and re-enable access to restart the reader.");
        }
        if !self.status.is_empty() {
            ui.colored_label(theme::sem(ui).negative, &self.status);
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        theme::help_text(ui, "Sensor access remains enabled until you turn it off or restart the system. Unavailable readings may not be supported by your hardware.");
    }
}

/// The fixed systemctl arguments that start or stop the sensor reader.
fn service_command(enable: bool) -> [&'static str; 3] {
    [
        "--system",
        if enable { "start" } else { "stop" },
        "argus-sensors.service",
    ]
}

/// What systemctl's run amounts to: done, or the reason to show.
fn service_outcome(output: std::io::Result<std::process::Output>) -> Result<(), String> {
    let output = output.map_err(|e| format!("systemctl could not be run: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    // A cancelled authentication can fail without a word; an empty reason
    // would show as no error at all.
    Err(if reason.is_empty() {
        format!("systemctl failed ({})", output.status)
    } else {
        reason
    })
}

/// Each reading, or why there is none.
fn readings_line(sensors: &crate::sensor_data::ExtendedSensors) -> String {
    format!(
        "CPU power: {} · RAM: {}",
        sensors
            .cpu_power_w
            .map(|w| format!("{w:.1} W"))
            .unwrap_or_else(|| sensors.cpu_power_status.clone()),
        sensors
            .ram_speed_mts
            .map(|v| format!("{v} MT/s"))
            .unwrap_or_else(|| sensors.ram_speed_status.clone())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(code: i32, stderr: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            // A wait status: the exit code is the second byte.
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    #[test]
    fn the_service_is_started_and_stopped_by_fixed_commands() {
        assert_eq!(
            service_command(true),
            ["--system", "start", "argus-sensors.service"]
        );
        assert_eq!(
            service_command(false),
            ["--system", "stop", "argus-sensors.service"]
        );
    }

    #[test]
    fn a_failed_service_change_always_has_a_reason_to_show() {
        assert_eq!(service_outcome(output(0, "ignored")), Ok(()));
        assert_eq!(
            service_outcome(output(1, "  Access denied\n")),
            Err("Access denied".into())
        );
        let silent = service_outcome(output(1, "")).unwrap_err();
        assert!(silent.starts_with("systemctl failed"), "{silent}");
        let missing = service_outcome(Err(std::io::ErrorKind::NotFound.into())).unwrap_err();
        assert!(
            missing.starts_with("systemctl could not be run"),
            "{missing}"
        );
    }

    #[test]
    fn a_missing_reading_shows_why_it_is_missing() {
        let sensors = |power, speed| crate::sensor_data::ExtendedSensors {
            schema: 1,
            sampled_unix_ms: 0,
            interval_ms: 1000,
            cpu_power_w: power,
            cpu_power_status: "RAPL unavailable".into(),
            ram_speed_mts: speed,
            ram_speed_status: "no DMI table".into(),
        };
        assert_eq!(
            readings_line(&sensors(Some(41.26), Some(6000))),
            "CPU power: 41.3 W · RAM: 6000 MT/s"
        );
        assert_eq!(
            readings_line(&sensors(None, None)),
            "CPU power: RAPL unavailable · RAM: no DMI table"
        );
    }
}
