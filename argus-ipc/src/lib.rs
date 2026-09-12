use serde::{Deserialize, Serialize};

pub const IPC_SOCKET_PATH: &str = "/tmp/argus_lasso_overlay.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcMessage {
    Telemetry(TelemetryFrame),
    Config(OverlayConfig),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TelemetryFrame {
    pub cpu_name: String,
    pub cpu_usage_percent: u8,
    pub cpu_temp_c: u8,
    pub cpu_power_w: f32,
    pub cpu_freq_mhz: Option<u32>,
    pub parked_cores: u32,
    pub core_usages: Vec<u8>,
    pub core_freqs: Vec<u32>,
    
    pub gpu_name: String,
    pub gpu_usage_percent: u8,
    pub gpu_temp_c: u8,
    pub gpu_power_w: f32,
    pub gpu_core_clock_mhz: Option<u32>,
    pub gpu_mem_clock_mhz: Option<u32>,
    pub gpu_fan_speed_percent: Option<u8>,
    
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_speed_mts: Option<u32>,
    
    pub vram_used_gb: f32,
    pub vram_total_gb: f32,
    pub active_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayConfig {
    pub show_overlay: bool,
    pub scale: u32,
    pub position: (i32, i32),
    pub offset_x: i32,
    pub offset_y: i32,
    pub text_color: (u8, u8, u8, u8), // RGBA
    pub bg_color: (u8, u8, u8, u8),   // RGBA
    pub show_cores: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            show_overlay: true,
            scale: 2,
            position: (10, 10),
            offset_x: 4,
            offset_y: 4,
            text_color: (0, 0xFF, 0x66, 0xFF), // Greenish
            bg_color: (0, 0, 0, 0),         // Semi-transparent black
            show_cores: false,
        }
    }
}
