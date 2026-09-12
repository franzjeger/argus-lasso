//! Feed captured real telemetry plus config transitions to an isolated test HUD.
//! Never binds the production socket or changes the daemon's configuration.
use argus_ipc::{read_message, write_message, IpcMessage, OverlayConfig};
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
fn main() -> io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("isolated fixture socket required");
    assert_ne!(std::path::Path::new(&path), argus_ipc::socket_path());
    let mut source = UnixStream::connect(argus_ipc::socket_path())?;
    source.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut frame = loop {
        if let IpcMessage::Telemetry(t) = read_message(&mut source)? {
            break t;
        }
    };
    let listener = UnixListener::bind(&path)?;
    println!("READY");
    io::stdout().flush()?;
    let (mut stream, _) = listener.accept()?;
    write_message(
        &mut stream,
        &IpcMessage::Hello {
            build: argus_ipc::BUILD_ID.into(),
            protocol: argus_ipc::PROTOCOL_VERSION,
            host_pid: std::process::id(),
        },
    )?;
    let mut config = OverlayConfig::default();
    for phase in [
        "default",
        "colors",
        "large",
        "background",
        "hidden",
        "restored",
    ] {
        match phase {
            "colors" => {
                config
                    .value_colors
                    .insert(argus_ipc::OverlayMetric::GpuTemp, [255, 90, 170]);
                config
                    .value_colors
                    .insert(argus_ipc::OverlayMetric::ThreadFrequency, [230, 235, 245]);
                config.fields.physical_core_id = true;
                config.section_dividers = false;
            }
            "large" => {
                config.font_px = 24;
                config.offset_x = 50;
                config.margin = 8;
                config.text_color = (255, 255, 255, 255);
                config.show_graph = true;
            }
            "background" => {
                config.bg_color = (10, 20, 60, 128);
                config.text_color.3 = 128;
            }
            "hidden" => config.show_overlay = false,
            "restored" => config = OverlayConfig::default(),
            _ => {}
        }
        write_message(&mut stream, &IpcMessage::Config(config.clone()))?;
        frame.sample_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        write_message(&mut stream, &IpcMessage::Telemetry(frame.clone()))?;
        println!("{phase}");
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(3));
    }
    // No new sample, but socket remains connected: must display stale status.
    std::thread::sleep(Duration::from_secs(4));
    println!("stale");
    io::stdout().flush()?;
    std::thread::sleep(Duration::from_secs(3));
    drop(stream);
    println!("disconnected");
    io::stdout().flush()?;
    std::thread::sleep(Duration::from_secs(3));
    std::fs::remove_file(path)?;
    Ok(())
}
