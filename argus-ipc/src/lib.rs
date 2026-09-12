use serde::{Deserialize, Serialize};

pub const IPC_SOCKET_PATH: &str = "/tmp/argus_lasso_overlay.sock";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryFrame {
    pub cpu_usage_percent: u8,
    pub cpu_temp_c: u8,
    pub gpu_usage_percent: u8,
    pub gpu_temp_c: u8,
    pub active_profile: String,
    pub parked_cores: u32, // Count of parked cores
}
