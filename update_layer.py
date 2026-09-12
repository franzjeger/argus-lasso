import re

with open("argus-layer/src/lib.rs", "r") as f:
    text = f.read()

header = """use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use std::sync::RwLock;
use std::collections::HashMap;
use std::thread;
use std::os::unix::net::UnixStream;
use std::io::Read;
use argus_ipc::TelemetryFrame;

lazy_static::lazy_static! {
    static ref TELEMETRY: RwLock<TelemetryFrame> = RwLock::new(TelemetryFrame::default());
}

fn start_ipc_thread() {
    thread::spawn(|| {
        loop {
            if let Ok(mut stream) = UnixStream::connect(argus_ipc::IPC_SOCKET_PATH) {
                let mut len_buf = [0u8; 4];
                while stream.read_exact(&mut len_buf).is_ok() {
                    let len = u32::from_le_bytes(len_buf) as usize;
                    let mut data = vec![0u8; len];
                    if stream.read_exact(&mut data).is_ok() {
                        if let Ok(frame) = bincode::deserialize::<TelemetryFrame>(&data) {
                            *TELEMETRY.write().unwrap() = frame;
                        }
                    } else {
                        break;
                    }
                }
            }
            thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}
"""

text = text.replace("use std::collections::HashMap;\n", header)

# We want to call start_ipc_thread() during vkNegotiateLoaderLayerInterfaceVersion
negotiate = """#[no_mangle]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    p_version_struct: *mut VkLayerNegotiateStruct,
) -> ash::vk::Result {"""

negotiate_new = """#[no_mangle]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    p_version_struct: *mut VkLayerNegotiateStruct,
) -> ash::vk::Result {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        start_ipc_thread();
    });
"""

text = text.replace(negotiate, negotiate_new)

# Update argus_vkQueuePresentKHR to print telemetry
present = """        if count % 60 == 0 {
            let fps = 1.0 / delta.as_secs_f64();
            println!("[Argus-Layer] Hooked vkQueuePresentKHR! FPS: {:.1} ({} ms)", fps, delta.as_millis());
        }"""

present_new = """        if count % 60 == 0 {
            let fps = 1.0 / delta.as_secs_f64();
            let tel = TELEMETRY.read().unwrap();
            println!("[Argus-Layer] FPS: {:.1} | CPU: {}% ({}°C) | GPU: {}% ({}°C) | Cores Parked: {}", 
                fps, tel.cpu_usage_percent, tel.cpu_temp_c, tel.gpu_usage_percent, tel.gpu_temp_c, tel.parked_cores);
        }"""

text = text.replace(present, present_new)

with open("argus-layer/src/lib.rs", "w") as f:
    f.write(text)

