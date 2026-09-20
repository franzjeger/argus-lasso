use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bump this on any field addition/removal/reorder in a type sent over the
/// wire (IpcMessage and anything it contains, e.g. OverlayConfig,
/// TelemetryFrame). bincode's struct decoding is purely positional — unlike
/// this crate's TOML config-file path, `#[serde(default)]` on a wire struct
/// cannot fill in a missing trailing field, because bincode has no per-field
/// wire tag to detect "missing" in the first place; the reader's own struct
/// definition dictates exactly how many bytes it consumes regardless of what
/// the writer actually sent. Some structs here carry `#[serde(default)]`
/// only because they're ALSO deserialized from user TOML on disk, where it
/// does work as intended — don't mistake that for wire-format safety.
pub const PROTOCOL_VERSION: u32 = 5;
pub const MAX_MESSAGE_SIZE: usize = 256 * 1024;
pub const BUILD_ID: &str = env!("ARGUS_BUILD_ID");

pub fn socket_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("ARGUS_LASSO_SOCKET") {
        return path.into();
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let uid = status
                .lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or("unknown");
            std::path::PathBuf::from(format!("/run/user/{uid}"))
        });
    runtime.join("argus-lasso/overlay-v5.sock")
}

/// Steam pressure-vessel does not expose arbitrary host XDG_RUNTIME_DIR
/// entries. HOME is shared, so publish the same stream on a private home
/// socket too. An explicit override remains exclusive for isolated tests.
pub fn socket_paths() -> Vec<std::path::PathBuf> {
    let mut paths = vec![socket_path()];
    if std::env::var_os("ARGUS_LASSO_SOCKET").is_none() {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(
                std::path::PathBuf::from(home).join(".local/share/argus-lasso/ipc/overlay-v5.sock"),
            );
        }
    }
    paths
}

pub fn connect() -> io::Result<(std::os::unix::net::UnixStream, std::path::PathBuf)> {
    let mut failures = Vec::new();
    for path in socket_paths() {
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(stream) => return Ok((stream, path)),
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        failures.join("; "),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// IPC messages are transient, off the render thread; keep the wire model inline.
#[allow(clippy::large_enum_variant)]
pub enum IpcMessage {
    Hello {
        build: String,
        protocol: u32,
        host_pid: u32,
    },
    Telemetry(TelemetryFrame),
    Config(OverlayConfig),
}

pub fn encode(msg: &IpcMessage) -> io::Result<Vec<u8>> {
    let data = bincode::serialize(msg).map_err(io::Error::other)?;
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(io::Error::other("IPC packet too large"));
    }
    let mut packet = Vec::with_capacity(12 + data.len());
    packet.extend_from_slice(b"ARGL");
    packet.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    packet.extend_from_slice(&(data.len() as u32).to_le_bytes());
    packet.extend(data);
    Ok(packet)
}
pub fn write_message(writer: &mut impl Write, msg: &IpcMessage) -> io::Result<()> {
    writer.write_all(&encode(msg)?)
}
pub fn read_message(reader: &mut impl Read) -> io::Result<IpcMessage> {
    use bincode::Options;
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    if &header[..4] != b"ARGL"
        || u32::from_le_bytes(header[4..8].try_into().unwrap()) != PROTOCOL_VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incompatible Argus IPC protocol; restart daemon and game",
        ));
    }
    let len = u32::from_le_bytes(header[8..].try_into().unwrap()) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized IPC packet",
        ));
    }
    let mut data = vec![0; len];
    reader.read_exact(&mut data)?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_MESSAGE_SIZE as u64)
        .reject_trailing_bytes()
        .deserialize(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LogicalCpu {
    pub id: u32,
    pub package_id: Option<u32>,
    pub core_id: Option<u32>,
    pub online: bool,
    pub usage: Option<u8>,
    pub frequency_mhz: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LaunchProfile {
    pub pid: u32,
    pub start_ticks: u64,
    pub profile: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GameTelemetry {
    pub pid: u32,
    pub name: String,
    pub main_thread_cpus: Option<String>,
    pub nice: Option<i32>,
    pub launcher_profile: Option<String>,
    pub probalance_active: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TelemetryFrame {
    pub sample_unix_ms: u64,
    pub sample_interval_ms: u64,
    pub cpu_name: String,
    pub cpu_usage_percent: u8,
    pub cpu_temp_c: Option<u8>,
    pub cpu_power_w: Option<f32>,
    pub cpu_power_status: String,
    pub cpu_freq_mhz: Option<u32>,
    pub parked_cores: u32,
    pub cpus: Vec<LogicalCpu>,
    pub gpu_name: String,
    pub gpu_usage_percent: Option<u8>,
    pub gpu_temp_c: Option<u8>,
    pub gpu_power_w: Option<f32>,
    pub gpu_core_clock_mhz: Option<u32>,
    pub gpu_mem_clock_mhz: Option<u32>,
    pub gpu_fan_speed_percent: Option<u8>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_speed_mts: Option<u32>,
    pub ram_speed_status: String,
    pub vram_used_gb: Option<f32>,
    pub vram_total_gb: Option<f32>,
    pub active_profile: String,
    pub game: Option<GameTelemetry>,
    pub launch_profiles: Vec<LaunchProfile>,
    pub probalance_pids: Vec<u32>,
}

/// Stable metric keys shared by configuration, UI and rasterization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMetric {
    GpuName,
    GpuUsage,
    GpuTemp,
    GpuPower,
    GpuCoreClock,
    GpuMemClock,
    GpuFan,
    Vram,
    CpuName,
    CpuUsage,
    CpuTemp,
    CpuPower,
    CpuFrequency,
    ThreadUsage,
    ThreadFrequency,
    PhysicalCoreId,
    RamUsage,
    RamSpeed,
    Fps,
    Frametime,
    AverageFps,
    Low1,
    Parked,
    ArgusMode,
    UnavailableReason,
    GameName,
    GameProfile,
    GameAffinity,
    GamePriority,
    Probalance,
    ThreadId,
    Graph,
}
impl OverlayMetric {
    pub fn default_color(self) -> [u8; 3] {
        use OverlayMetric::*;
        match self {
            GpuName | GpuUsage | GpuTemp | GpuPower | GpuCoreClock | GpuMemClock | GpuFan => {
                [108, 221, 166]
            }
            CpuName | CpuUsage | CpuTemp | CpuPower | CpuFrequency | ThreadId | ThreadUsage
            | ThreadFrequency | PhysicalCoreId => [125, 190, 255],
            Vram | RamUsage | RamSpeed => [195, 166, 255],
            Fps | Frametime | AverageFps | Low1 | Graph => [255, 211, 128],
            GameName | GameProfile | GameAffinity | GamePriority | Probalance | Parked
            | ArgusMode => [210, 220, 232],
            UnavailableReason => [255, 183, 132],
        }
    }
}

/// Individual visibility flags, grouped by the existing section switches.
///
/// Also sent over the bincode wire as part of OverlayConfig: adding a field
/// here needs a PROTOCOL_VERSION bump too, `#[serde(default)]` below only
/// covers the TOML-on-disk path (see PROTOCOL_VERSION's doc comment).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayFields {
    pub gpu_name: bool,
    pub gpu_usage: bool,
    pub gpu_temp: bool,
    pub gpu_power: bool,
    pub gpu_core_clock: bool,
    pub gpu_mem_clock: bool,
    pub gpu_fan: bool,
    pub vram: bool,
    pub cpu_name: bool,
    pub cpu_usage: bool,
    pub cpu_temp: bool,
    pub cpu_power: bool,
    pub cpu_frequency: bool,
    pub thread_usage: bool,
    pub thread_frequency: bool,
    pub physical_core_id: bool,
    pub ram_usage: bool,
    pub ram_speed: bool,
    pub fps: bool,
    pub frametime: bool,
    pub average_fps: bool,
    pub low_1: bool,
    pub parked: bool,
    pub argus_mode: bool,
    pub unavailable_reason: bool,
    pub game_name: bool,
    pub game_profile: bool,
    pub game_affinity: bool,
    pub game_priority: bool,
    pub probalance: bool,
}
impl Default for OverlayFields {
    fn default() -> Self {
        Self {
            gpu_name: true,
            gpu_usage: true,
            gpu_temp: true,
            gpu_power: true,
            gpu_core_clock: true,
            gpu_mem_clock: true,
            gpu_fan: true,
            vram: true,
            cpu_name: true,
            cpu_usage: true,
            cpu_temp: true,
            cpu_power: true,
            cpu_frequency: true,
            thread_usage: true,
            thread_frequency: true,
            physical_core_id: false,
            ram_usage: true,
            ram_speed: true,
            fps: true,
            frametime: true,
            average_fps: true,
            low_1: true,
            parked: true,
            argus_mode: true,
            unavailable_reason: true,
            game_name: true,
            game_profile: true,
            game_affinity: true,
            game_priority: false,
            probalance: true,
        }
    }
}

/// Sent over the bincode wire as `IpcMessage::Config`, and also stored in the
/// user's TOML config file. Adding a field needs a PROTOCOL_VERSION bump —
/// `#[serde(default)]` below only covers the TOML path, see its doc comment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    pub show_overlay: bool,
    // Retained only to read older user configurations; rendering uses physical pixels.
    pub scale: u32,
    pub font_px: u32,
    pub position: (i32, i32),
    pub offset_x: i32,
    pub offset_y: i32,
    pub margin: u32,
    pub text_color: (u8, u8, u8, u8),
    pub bg_color: (u8, u8, u8, u8),
    pub show_cores: bool,
    pub show_gpu: bool,
    pub show_cpu: bool,
    pub show_ram: bool,
    pub show_fps: bool,
    pub show_graph: bool,
    pub fields: OverlayFields,
    pub hidden_cpu_ids: Vec<u32>,
    /// 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
    pub anchor: u8,
    /// Unset entries inherit the component palette. RGB never changes text alpha.
    pub value_colors: std::collections::BTreeMap<OverlayMetric, [u8; 3]>,
    pub section_dividers: bool,
    pub graph_hz: u32,
    pub graph_max_ms: u32,
}
impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            show_overlay: true,
            scale: 1,
            font_px: 14,
            position: (0, 0),
            offset_x: 8,
            offset_y: 8,
            margin: 4,
            text_color: (218, 225, 235, 255),
            bg_color: (0, 0, 0, 0),
            show_cores: true,
            show_gpu: true,
            show_cpu: true,
            show_ram: true,
            show_fps: true,
            show_graph: false,
            fields: OverlayFields::default(),
            hidden_cpu_ids: Vec::new(),
            anchor: 0,
            value_colors: Default::default(),
            section_dividers: true,
            graph_hz: 60,
            graph_max_ms: 33,
        }
    }
}

impl OverlayConfig {
    pub fn metric_color(&self, metric: OverlayMetric) -> [u8; 3] {
        self.value_colors
            .get(&metric)
            .copied()
            .unwrap_or_else(|| metric.default_color())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_roundtrip_and_reject_wrong_version() {
        let mut bytes = encode(&IpcMessage::Telemetry(TelemetryFrame {
            gpu_usage_percent: Some(0),
            ..Default::default()
        }))
        .unwrap();
        assert!(matches!(
            read_message(&mut bytes.as_slice()).unwrap(),
            IpcMessage::Telemetry(TelemetryFrame {
                gpu_usage_percent: Some(0),
                ..
            })
        ));
        bytes[4] = 99;
        assert!(read_message(&mut bytes.as_slice()).is_err());
    }
    #[test]
    fn reject_unbounded_packet() {
        let mut bytes = b"ARGL".to_vec();
        bytes.extend(PROTOCOL_VERSION.to_le_bytes());
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(read_message(&mut bytes.as_slice()).is_err());
    }
}

pub mod capture;
