import re

with open("src/monitor.rs", "r") as f:
    text = f.read()

ipc_import = """use argus_ipc::TelemetryFrame;
use std::os::unix::net::UnixListener;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::thread;

lazy_static::lazy_static! {
    static ref IPC_CLIENTS: Arc<Mutex<Vec<std::os::unix::net::UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
}

fn start_ipc_server() {
    thread::spawn(|| {
        let _ = std::fs::remove_file(argus_ipc::IPC_SOCKET_PATH);
        if let Ok(listener) = UnixListener::bind(argus_ipc::IPC_SOCKET_PATH) {
            for stream in listener.incoming() {
                if let Ok(stream) = stream {
                    IPC_CLIENTS.lock().unwrap().push(stream);
                }
            }
        }
    });
}
"""

text = text.replace("use std::collections::HashSet;", "use std::collections::HashSet;\n" + ipc_import)

broadcast = """    pub fn tick(&mut self, ui_sender: Sender<AppState>) {
        static INIT_IPC: std::sync::Once = std::sync::Once::new();
        INIT_IPC.call_once(|| {
            start_ipc_server();
        });
        
"""

text = text.replace("    pub fn tick(&mut self, ui_sender: Sender<AppState>) {", broadcast)

send_ipc = """        if let Ok(mut clients) = IPC_CLIENTS.lock() {
            if !clients.is_empty() {
                let frame = TelemetryFrame {
                    cpu_usage_percent: new_state.cpu_utilization.round() as u8,
                    cpu_temp_c: new_state.cpu_temp.round() as u8,
                    gpu_usage_percent: new_state.gpu_usage.round() as u8,
                    gpu_temp_c: new_state.gpu_temp.round() as u8,
                    active_profile: new_state.power_profile.clone(),
                    parked_cores: new_state.parked_cores.len() as u32,
                };
                if let Ok(encoded) = bincode::serialize(&frame) {
                    let len = (encoded.len() as u32).to_le_bytes();
                    clients.retain_mut(|client| {
                        client.write_all(&len).is_ok() && client.write_all(&encoded).is_ok()
                    });
                }
            }
        }
        
        let _ = ui_sender.send(new_state);
"""

text = text.replace("        let _ = ui_sender.send(new_state);", send_ipc)

with open("src/monitor.rs", "w") as f:
    f.write(text)

