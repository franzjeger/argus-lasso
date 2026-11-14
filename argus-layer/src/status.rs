//! What became of the HUD in this game, told to the daemon.
//!
//! A game without a HUD used to say why on stderr only, which for a Steam
//! game ends up nowhere, or in a Proton log nobody reads. The daemon shows
//! the same line next to the game's name.
use argus_ipc::{IpcMessage, LayerStatus};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

struct State {
    status: LayerStatus,
    /// The daemon connection's write half, while there is one.
    stream: Option<UnixStream>,
}

lazy_static::lazy_static! {
    static ref STATE: Mutex<State> = Mutex::new(State {
        status: LayerStatus {
            drawing: false,
            detail: "The game has not created a swapchain yet".into(),
        },
        stream: None,
    });
}

fn send(state: &mut State) {
    let Some(stream) = state.stream.as_mut() else {
        return;
    };
    let message = IpcMessage::Status(state.status.clone());
    if argus_ipc::write_message(stream, &message).is_err() {
        // The reading side notices the lost connection and reconnects.
        state.stream = None;
    }
}

/// Record what the HUD is doing now, and tell the daemon if it changed.
/// Called from the game's threads, on changes only: never per frame.
pub fn report(drawing: bool, detail: impl Into<String>) {
    let status = LayerStatus {
        drawing,
        detail: detail.into(),
    };
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.status != status {
        state.status = status;
        send(&mut state);
    }
}

/// A connection to the daemon is up: tell it where things stand.
pub fn connected(stream: &UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    // A daemon that stopped reading must not hold up a game thread.
    let _ = writer.set_write_timeout(Some(Duration::from_millis(50)));
    if let Ok(mut state) = STATE.lock() {
        state.stream = Some(writer);
        send(&mut state);
    }
}

pub fn disconnected() {
    if let Ok(mut state) = STATE.lock() {
        state.stream = None;
    }
}
