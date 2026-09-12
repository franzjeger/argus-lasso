//! Narrow, versioned output from the optional system sensor service.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtendedSensors {
    pub schema: u32,
    pub sampled_unix_ms: u64,
    pub interval_ms: u64,
    pub cpu_power_w: Option<f32>,
    pub cpu_power_status: String,
    pub ram_speed_mts: Option<u32>,
    pub ram_speed_status: String,
}
pub const CACHE: &str = "/run/argus-sensors/telemetry.json";
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn read() -> Result<ExtendedSensors, String> {
    let data = std::fs::read(CACHE).map_err(|e| format!("Sensor service: {e}"))?;
    if data.len() > 16_384 {
        return Err("Invalid sensor cache size".into());
    }
    let snapshot: ExtendedSensors = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if snapshot.schema != 1 || now_ms().saturating_sub(snapshot.sampled_unix_ms) > 3500 {
        return Err("Extended sensor data is stale".into());
    }
    Ok(snapshot)
}
