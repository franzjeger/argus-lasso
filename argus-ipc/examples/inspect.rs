use argus_ipc::{read_message, IpcMessage};
fn main() -> std::io::Result<()> {
    let (mut stream, path) = argus_ipc::connect()?;
    println!("Connected: {}", path.display());
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut frames = 0;
    loop {
        let message = read_message(&mut stream)?;
        println!("{message:#?}");
        if matches!(message, IpcMessage::Telemetry(_)) {
            frames += 1;
            if frames >= 3 {
                break;
            }
        }
    }
    Ok(())
}
