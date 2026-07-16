//! Pure timer-state transitions — the local clock's arithmetic
//! (offline-write.brief.md §3 "one clock, offline too").
//!
//! These are the synthesizers the offline write path uses: given the last
//! known server snapshot and the wall-clock moment the user acted, produce the
//! `Timer` the server itself would return for that verb. Same fields, same
//! meaning (`started_at`, `elapsed_seconds`, `paused_seconds`, `paused_at`),
//! so a locally-run session reconciles to the second when the wire returns.
//! No I/O here — persistence is `crate::queue`, reads are `crate::timer_cache`.
#![allow(dead_code)]

use crate::api::Timer;

/// The server's pause: freeze the clock, stamp when.
pub fn apply_pause(mut t: Timer, at: jiff::Timestamp) -> Timer {
    if !t.paused {
        t.paused = true;
        t.paused_at = Some(at);
    }
    t
}

/// The server's resume: fold the paused span into `paused_seconds`, clear the
/// stamp, run on.
pub fn apply_resume(mut t: Timer, at: jiff::Timestamp) -> Timer {
    if t.paused {
        if let Some(paused_at) = t.paused_at {
            let span = (at.as_second() - paused_at.as_second()).max(0);
            t.paused_seconds = Some(t.paused_seconds.unwrap_or(0) + span);
        }
        t.paused = false;
        t.paused_at = None;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "activity_id": 9, "label": "systems",
            "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 1800,
            "paused_seconds": 60
        }))
        .unwrap()
    }

    #[test]
    fn pause_freezes_and_stamps() {
        let at: jiff::Timestamp = "2026-07-15T09:31:00Z".parse().unwrap();
        let t = apply_pause(running(), at);
        assert!(t.paused);
        assert_eq!(t.paused_at, Some(at));
        assert_eq!(t.paused_seconds, Some(60), "prior paused time untouched");
    }

    #[test]
    fn resume_folds_the_span_and_clears_the_stamp() {
        let paused_at: jiff::Timestamp = "2026-07-15T09:31:00Z".parse().unwrap();
        let resumed_at: jiff::Timestamp = "2026-07-15T09:41:30Z".parse().unwrap();
        let t = apply_resume(apply_pause(running(), paused_at), resumed_at);
        assert!(!t.paused);
        assert_eq!(t.paused_at, None);
        assert_eq!(t.paused_seconds, Some(60 + 630));
    }

    #[test]
    fn transitions_are_idempotent_on_wrong_state() {
        let at: jiff::Timestamp = "2026-07-15T09:31:00Z".parse().unwrap();
        let already_running = apply_resume(running(), at);
        assert_eq!(
            already_running.paused_seconds,
            Some(60),
            "resume on running is a no-op"
        );
        let paused_once = apply_pause(
            apply_pause(running(), at),
            "2026-07-15T09:59:00Z".parse().unwrap(),
        );
        assert_eq!(
            paused_once.paused_at,
            Some(at),
            "second pause keeps the first stamp"
        );
    }
}
