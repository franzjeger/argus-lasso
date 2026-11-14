//! Frame-rate cap: hold each present back until its turn.
//!
//! The wait sits just before the real `vkQueuePresentKHR`, so frames reach
//! the screen evenly spaced. The game has already sampled input and rendered
//! by then, which a game's own limiter avoids; that one is the better choice
//! where it exists.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sleeping wakes late by the timer slack and whatever the scheduler adds;
/// the last stretch is spun instead, to land on the deadline.
const SPIN: Duration = Duration::from_micros(250);

lazy_static::lazy_static! {
    /// `ARGUS_LASSO_FPS_LIMIT` in a game's launch options: this game's own
    /// cap, whatever Argus is set to. 0 means never cap this game.
    static ref FROM_ENV: Option<u32> = std::env::var("ARGUS_LASSO_FPS_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse().ok());
    // One for the process: two swapchains presenting at once share the cap.
    static ref STATE: Mutex<State> = Mutex::new(State::default());
}

/// The cap in force, for the HUD to show.
static CURRENT: AtomicU32 = AtomicU32::new(0);

/// The frame-rate cap presents are held to now; 0 is none.
pub fn current() -> u32 {
    CURRENT.load(Ordering::Relaxed)
}

#[derive(Default)]
struct State {
    /// The limit the deadline belongs to, and the last one logged.
    limit: u32,
    deadline: Option<Instant>,
}

/// The cap in force: the game's own from its launch options if it has one,
/// otherwise the configured one. 0 is no cap.
fn effective(configured: u32, from_env: Option<u32>) -> u32 {
    argus_ipc::normalized_fps_limit(from_env.unwrap_or(configured))
}

/// When a present arriving at `now` may go ahead, and the deadline for the
/// one after it.
fn schedule(now: Instant, deadline: Option<Instant>, interval: Duration) -> (Instant, Instant) {
    match deadline {
        // Early: wait for the slot.
        Some(slot) if slot > now => (slot, slot + interval),
        // Late by less than a frame: go now and keep the rhythm, so the
        // average rate holds through ordinary jitter.
        Some(slot) if now - slot < interval => (now, slot + interval),
        // The first frame, or the game is slower than the cap: start a new
        // rhythm here rather than hurrying frames to catch up.
        _ => (now, now + interval),
    }
}

/// Block until this present's turn under `configured` frames per second.
pub fn wait(configured: u32) {
    let limit = effective(configured, *FROM_ENV);
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.limit != limit {
        state.limit = limit;
        state.deadline = None;
        CURRENT.store(limit, Ordering::Relaxed);
        if limit == 0 {
            eprintln!("[Argus-Layer] Frame rate limit off");
        } else {
            eprintln!("[Argus-Layer] Frame rate limit {limit} FPS");
        }
    }
    if limit == 0 {
        return;
    }
    let interval = Duration::from_secs(1) / limit;
    let (release, next) = schedule(Instant::now(), state.deadline, interval);
    state.deadline = Some(next);
    // The lock is kept through the wait: a second presenting thread queues
    // behind this one instead of taking the same slot.
    if let Some(sleep) = release
        .checked_duration_since(Instant::now())
        .and_then(|left| left.checked_sub(SPIN))
    {
        std::thread::sleep(sleep);
    }
    while Instant::now() < release {
        std::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(10);

    #[test]
    fn the_first_present_goes_at_once_and_starts_the_rhythm() {
        let now = Instant::now();
        assert_eq!(schedule(now, None, FRAME), (now, now + FRAME));
    }

    #[test]
    fn an_early_present_waits_for_its_slot() {
        let now = Instant::now();
        let slot = now + Duration::from_millis(4);
        assert_eq!(schedule(now, Some(slot), FRAME), (slot, slot + FRAME));
    }

    #[test]
    fn a_slightly_late_present_keeps_the_rhythm() {
        let slot = Instant::now();
        let now = slot + Duration::from_millis(3);
        // Released at once; the next slot is still a frame after the missed
        // one, so 100 frames take one second on average.
        assert_eq!(schedule(now, Some(slot), FRAME), (now, slot + FRAME));
    }

    #[test]
    fn a_game_slower_than_the_cap_is_never_hurried() {
        let slot = Instant::now();
        let now = slot + Duration::from_millis(25);
        // No burst of frames to make up for the lost time.
        assert_eq!(schedule(now, Some(slot), FRAME), (now, now + FRAME));
    }

    #[test]
    fn the_launch_option_wins_over_the_configured_cap() {
        assert_eq!(effective(60, None), 60);
        assert_eq!(effective(60, Some(120)), 120);
        // 0 in the launch options exempts the game.
        assert_eq!(effective(60, Some(0)), 0);
        assert_eq!(effective(0, Some(90)), 90);
        // Out-of-range values are brought into range, never ignored into
        // an uncapped or frozen game.
        assert_eq!(effective(3, None), argus_ipc::FPS_LIMIT_MIN);
        assert_eq!(effective(0, Some(100_000)), argus_ipc::FPS_LIMIT_MAX);
    }

    #[test]
    fn waiting_holds_presents_to_the_cap() {
        // 200 FPS for 40 frames is 195 ms after the first, immediate one.
        let start = Instant::now();
        for _ in 0..40 {
            wait(200);
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(195), "{took:?}");
        // Generous: a loaded CI machine wakes sleepers late.
        assert!(took < Duration::from_secs(2), "{took:?}");
        wait(0);
    }
}
