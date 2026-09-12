//! Root service with no input commands, network, user paths or child processes.
//! Reads package RAPL counters and SMBIOS configured memory speed; publishes
//! a coarse 1 Hz snapshot in a root-owned systemd RuntimeDirectory.
// The writer shares the schema, but does not read its own public cache.
#[allow(dead_code)]
#[path = "../sensor_data.rs"]
mod sensor_data;
use sensor_data::{now_ms, ExtendedSensors};
use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

fn u16_at(b: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
/// SMBIOS type 17 configured speed, not the DIMM's maximum supported speed.
fn ram_speed(table: &[u8]) -> Result<u32, String> {
    let mut at = 0;
    let mut speeds = Vec::new();
    while at + 4 <= table.len() {
        let kind = table[at];
        let len = table[at + 1] as usize;
        if len < 4 || at + len > table.len() {
            return Err("Malformed SMBIOS structure".into());
        }
        let b = &table[at..at + len];
        if kind == 17 && u16_at(b, 12).is_some_and(|size| size != 0) {
            let speed = u16_at(b, 0x20).ok_or("Configured RAM speed unsupported")?;
            let speed = if speed == 0xffff {
                u32::from_le_bytes(
                    b.get(0x58..0x5c)
                        .ok_or("Extended RAM speed unavailable")?
                        .try_into()
                        .unwrap(),
                )
            } else {
                speed as u32
            };
            if speed == 0 {
                return Err("Configured RAM speed unknown".into());
            }
            speeds.push(speed);
        }
        if kind == 127 {
            break;
        }
        let strings = at + len;
        let end = table[strings..]
            .windows(2)
            .position(|p| p == [0, 0])
            .ok_or("Truncated SMBIOS strings")?;
        at = strings + end + 2;
    }
    let first = *speeds
        .first()
        .ok_or("No populated memory devices reported")?;
    if speeds.iter().all(|v| *v == first) {
        Ok(first)
    } else {
        Err("Mixed configured RAM speeds".into())
    }
}
fn read_u64(path: PathBuf) -> io::Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(io::Error::other)
}
fn watts(prev: u64, now: u64, range: u64, seconds: f64) -> Option<f32> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > 5.0 {
        return None;
    }
    let delta = if now >= prev {
        now - prev
    } else if range > prev && range > now {
        range - prev + now
    } else {
        return None;
    };
    let w = delta as f64 / 1_000_000.0 / seconds;
    (w.is_finite() && w < 10_000.0).then_some(w as f32)
}
fn run() -> io::Result<()> {
    if unsafe { nix::libc::geteuid() } != 0 {
        return Err(io::Error::other(
            "Run only as the argus-sensors system service",
        ));
    }
    let ram = fs::read("/sys/firmware/dmi/tables/DMI")
        .map_err(|e| e.to_string())
        .and_then(|b| ram_speed(&b));
    let mut previous = BTreeMap::<PathBuf, (u64, Instant)>::new();
    loop {
        let now = Instant::now();
        let mut data = ExtendedSensors {
            schema: 1,
            sampled_unix_ms: now_ms(),
            interval_ms: 1000,
            ram_speed_mts: ram.as_ref().ok().copied(),
            ram_speed_status: ram
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "SMBIOS configured speed".into()),
            ..Default::default()
        };
        let result = (|| -> Result<f32, String> {
            let entries = fs::read_dir("/sys/class/powercap").map_err(|e| e.to_string())?;
            let mut total = 0.0;
            let mut packages = 0;
            for entry in entries.flatten() {
                let path = entry.path();
                let name = fs::read_to_string(path.join("name")).unwrap_or_default();
                // Exclude core/DRAM subzones to avoid counting energy twice.
                if !name.trim().starts_with("package-") {
                    continue;
                }
                let energy = read_u64(path.join("energy_uj")).map_err(|e| e.to_string())?;
                let range =
                    read_u64(path.join("max_energy_range_uj")).map_err(|e| e.to_string())?;
                let prev = previous.insert(path, (energy, now));
                let (old, then) = prev.ok_or("Waiting for second energy sample")?;
                total += watts(old, energy, range, now.duration_since(then).as_secs_f64())
                    .ok_or("Energy counter reset or sampling gap")?;
                packages += 1;
            }
            if packages == 0 {
                Err("Package RAPL counters unsupported".into())
            } else {
                Ok(total)
            }
        })();
        data.cpu_power_w = result.as_ref().ok().copied();
        data.cpu_power_status = result
            .err()
            .unwrap_or_else(|| "Measured package RAPL energy / elapsed time".into());
        let temp = "/run/argus-sensors/telemetry.json.tmp";
        fs::write(temp, serde_json::to_vec(&data)?)?;
        fs::rename(temp, sensor_data::CACHE)?;
        std::thread::sleep(Duration::from_secs(1).saturating_sub(now.elapsed()));
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("argus-sensors: {e}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counter_wrap_and_gaps() {
        assert_eq!(watts(9_000_000, 1_000_000, 10_000_000, 1.0), Some(2.0));
        assert_eq!(watts(100, 200, 1000, 0.0), None);
        assert_eq!(watts(100, 200, 1000, 6.0), None);
    }
    #[test]
    fn configured_speed_not_rated_speed() {
        let mut b = vec![0; 0x22];
        b[0] = 17;
        b[1] = 0x22;
        b[12..14].copy_from_slice(&24576u16.to_le_bytes());
        b[0x15..0x17].copy_from_slice(&9000u16.to_le_bytes());
        b[0x20..0x22].copy_from_slice(&8000u16.to_le_bytes());
        b.extend([0, 0, 127, 4, 0, 0, 0, 0]);
        assert_eq!(ram_speed(&b), Ok(8000));
        assert!(ram_speed(&[17, 2, 0, 0]).is_err());
    }
}
