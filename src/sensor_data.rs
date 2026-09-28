//! Narrow, versioned output from the optional system sensor service.
use serde::{Deserialize, Serialize};
use std::io::Read;

const MAX_CACHE_BYTES: u64 = 16_384;
const MAX_AGE_MS: u64 = 3500;
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
    let file = std::fs::File::open(CACHE).map_err(|e| format!("Sensor service: {e}"))?;
    read_snapshot(file, now_ms())
}

fn read_snapshot(reader: impl Read, now: u64) -> Result<ExtendedSensors, String> {
    // Bound the read itself, not just the eventual JSON parse/allocation.
    let mut data = Vec::new();
    reader
        .take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("Sensor service: {e}"))?;
    if data.len() as u64 > MAX_CACHE_BYTES {
        return Err("Invalid sensor cache size".into());
    }
    let snapshot: ExtendedSensors = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if snapshot.schema != 1 {
        return Err("Unsupported sensor cache schema".into());
    }
    let age = now
        .checked_sub(snapshot.sampled_unix_ms)
        .ok_or("Extended sensor timestamp is in the future")?;
    if age > MAX_AGE_MS {
        return Err("Extended sensor data is stale".into());
    }
    if snapshot
        .cpu_power_w
        .is_some_and(|w| !w.is_finite() || w < 0.0)
        || snapshot.ram_speed_mts == Some(0)
    {
        return Err("Invalid extended sensor measurement".into());
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> ExtendedSensors {
        ExtendedSensors {
            schema: 1,
            sampled_unix_ms: 10_000,
            interval_ms: 1000,
            cpu_power_w: Some(0.0),
            ram_speed_mts: Some(8000),
            ..Default::default()
        }
    }

    fn parse(s: &ExtendedSensors, now: u64) -> Result<ExtendedSensors, String> {
        read_snapshot(serde_json::to_vec(s).unwrap().as_slice(), now)
    }

    #[test]
    fn freshness_boundaries_and_schema_are_distinct() {
        let mut s = snapshot();
        assert!(parse(&s, 10_000).is_ok());
        assert!(parse(&s, 13_500).is_ok());
        assert!(parse(&s, 13_501).unwrap_err().contains("stale"));
        assert!(parse(&s, 9_999).unwrap_err().contains("future"));
        s.schema = 2;
        assert!(parse(&s, 10_000).unwrap_err().contains("schema"));
    }

    #[test]
    fn unavailable_and_measured_zero_remain_distinct() {
        let mut s = snapshot();
        assert_eq!(parse(&s, 10_000).unwrap().cpu_power_w, Some(0.0));
        s.cpu_power_w = None;
        s.ram_speed_mts = None;
        let parsed = parse(&s, 10_000).unwrap();
        assert_eq!(parsed.cpu_power_w, None);
        assert_eq!(parsed.ram_speed_mts, None);
        s.cpu_power_w = Some(-1.0);
        assert!(parse(&s, 10_000).is_err());
        s.cpu_power_w = Some(42.0);
        s.ram_speed_mts = Some(0);
        assert!(parse(&s, 10_000).is_err());
    }

    #[test]
    fn oversized_input_is_bounded_before_parsing() {
        let mut input = std::io::repeat(b' ');
        assert!(read_snapshot(&mut input, 10_000)
            .unwrap_err()
            .contains("size"));
        assert!(read_snapshot(&b"{"[..], 10_000).is_err());
    }
}
