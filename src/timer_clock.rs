//! The server's timer arithmetic and transitions, computed client-side (ADR 0004, "one clock, offline too").
#![allow(dead_code)]

use crate::api::Timer;

/// Without `started_at` this reads the snapshot figure verbatim — when its age
/// is known, use [`elapsed_with_snapshot_age`] so an advancing clock still moves.
pub fn elapsed(t: &Timer, now: jiff::Timestamp) -> i64 {
    elapsed_with_snapshot_age(t, now, 0)
}

/// `snapshot_age_s` is how old the snapshot's `elapsed_seconds` is; it only
/// counts when `started_at` is absent.
pub fn elapsed_with_snapshot_age(t: &Timer, now: jiff::Timestamp, snapshot_age_s: i64) -> i64 {
    if !t.running {
        return t.elapsed_seconds.unwrap_or(0);
    }
    let Some(started_at) = t.started_at else {
        let base = t.elapsed_seconds.unwrap_or(0);
        return if t.paused {
            base
        } else {
            base + snapshot_age_s.max(0)
        };
    };
    let open_pause = match (t.paused, t.paused_at) {
        (true, Some(paused_at)) => now.as_second() - paused_at.as_second(),
        (true, None) => return t.elapsed_seconds.unwrap_or(0),
        (false, _) => 0,
    };
    (now.as_second() - started_at.as_second() - t.paused_seconds.unwrap_or(0) - open_pause).max(0)
}

/// A mirror of the server's 4 AM study-day boundary, for the day an offline
/// stop's segment lands on.
pub fn study_day(ts: jiff::Timestamp) -> jiff::civil::Date {
    study_day_in(ts, jiff::tz::TimeZone::system())
}

pub fn study_day_in(ts: jiff::Timestamp, tz: jiff::tz::TimeZone) -> jiff::civil::Date {
    let zoned = ts.to_zoned(tz);
    let date = zoned.date();
    if zoned.time() < jiff::civil::time(4, 0, 0, 0) {
        date.yesterday().unwrap_or(date)
    } else {
        date
    }
}

pub fn apply_start(activity_id: Option<i64>, label: Option<String>, at: jiff::Timestamp) -> Timer {
    Timer {
        running: true,
        bound: activity_id.is_some(),
        activity_id,
        label,
        started_at: Some(at),
        elapsed_seconds: Some(0),
        paused_seconds: Some(0),
        ..Timer::default()
    }
}

pub fn apply_pause(mut t: Timer, at: jiff::Timestamp) -> Timer {
    if !t.paused {
        t.paused = true;
        t.paused_at = Some(at);
    }
    t
}

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

/// The `TimerStopped` shape minus the server-minted `segment_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStop {
    /// `None` is the unbound case the server refuses — callers guard before
    /// enqueueing a stop.
    pub activity_id: Option<i64>,
    /// What the reconcile pass compares against the server's written segment
    /// (`IntentKind::TimerStop::local_elapsed_s`).
    pub elapsed_seconds: i64,
    pub minutes: u32,
    pub day: jiff::civil::Date,
}

pub fn apply_stop(t: Timer, at: jiff::Timestamp) -> (Timer, LocalStop) {
    let elapsed_seconds = elapsed(&t, at);
    let stop = LocalStop {
        activity_id: t.activity_id,
        elapsed_seconds,
        minutes: ((elapsed_seconds + 30) / 60) as u32,
        day: study_day(at),
    };
    (Timer::default(), stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(v: serde_json::Value) -> Timer {
        serde_json::from_value(v).unwrap()
    }

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn running() -> Timer {
        timer(serde_json::json!({
            "running": true, "bound": true, "activity_id": 9, "label": "systems",
            "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 1800,
            "paused_seconds": 60
        }))
    }

    #[test]
    fn elapsed_covers_every_shape() {
        let now = ts("2026-07-15T10:00:00Z");
        let cases: &[(&str, serde_json::Value, i64)] = &[
            (
                "running from started_at, no pauses",
                serde_json::json!({"running": true, "started_at": "2026-07-15T09:00:00Z"}),
                3600,
            ),
            (
                "banked pauses subtract",
                serde_json::json!({"running": true, "started_at": "2026-07-15T09:00:00Z",
                                   "paused_seconds": 600}),
                3000,
            ),
            (
                "an open pause freezes the clock exactly",
                serde_json::json!({"running": true, "paused": true,
                                   "started_at": "2026-07-15T09:00:00Z",
                                   "paused_at": "2026-07-15T09:30:00Z", "paused_seconds": 60}),
                1740, // (09:30 − 09:00) − 60, however far `now` runs on
            ),
            (
                "paused with no stamp reads the snapshot figure",
                serde_json::json!({"running": true, "paused": true,
                                   "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 1500}),
                1500,
            ),
            (
                "not running reads the snapshot figure",
                serde_json::json!({"running": false, "elapsed_seconds": 90}),
                90,
            ),
            (
                "no started_at falls back to the snapshot figure",
                serde_json::json!({"running": true, "elapsed_seconds": 1800}),
                1800,
            ),
            (
                "a started_at ahead of now floors at zero",
                serde_json::json!({"running": true, "started_at": "2026-07-15T10:05:00Z"}),
                0,
            ),
        ];
        for (name, v, want) in cases {
            assert_eq!(elapsed(&timer(v.clone()), now), *want, "{name}");
        }
    }

    #[test]
    fn snapshot_age_extrapolates_only_without_an_anchor() {
        let now = ts("2026-07-15T10:00:00Z");
        let cases: &[(&str, serde_json::Value, i64, i64)] = &[
            (
                "advancing without an anchor: snapshot + age",
                serde_json::json!({"running": true, "elapsed_seconds": 1800}),
                45,
                1845,
            ),
            (
                "paused without an anchor stays frozen despite the age",
                serde_json::json!({"running": true, "paused": true, "elapsed_seconds": 1800}),
                45,
                1800,
            ),
            (
                "with an anchor the age is ignored — arithmetic wins",
                serde_json::json!({"running": true, "started_at": "2026-07-15T09:00:00Z",
                                   "elapsed_seconds": 1800}),
                45,
                3600,
            ),
        ];
        for (name, v, age, want) in cases {
            assert_eq!(
                elapsed_with_snapshot_age(&timer(v.clone()), now, *age),
                *want,
                "{name}"
            );
        }
    }

    #[test]
    fn study_day_boundary_at_4am_both_sides_of_midnight() {
        let tz = jiff::tz::TimeZone::UTC;
        let cases: &[(&str, &str)] = &[
            ("2026-07-15T03:59:59Z", "2026-07-14"), // 3:59 → still yesterday
            ("2026-07-15T04:00:00Z", "2026-07-15"), // 4:00 sharp → today
            ("2026-07-15T04:01:00Z", "2026-07-15"),
            ("2026-07-15T23:59:00Z", "2026-07-15"), // before midnight → today
            ("2026-07-16T00:30:00Z", "2026-07-15"), // after midnight → still the 15th
            ("2026-07-01T01:00:00Z", "2026-06-30"), // the boundary crosses months too
        ];
        for (moment, want) in cases {
            assert_eq!(
                study_day_in(ts(moment), tz.clone()).to_string(),
                *want,
                "{moment}"
            );
        }
    }

    #[test]
    fn start_synthesizes_a_fresh_anchored_clock() {
        let at = ts("2026-07-15T09:00:00Z");
        let bound = apply_start(Some(9), Some("systems".into()), at);
        assert!(bound.running && bound.bound && !bound.paused);
        assert_eq!(bound.activity_id, Some(9));
        assert_eq!(bound.label.as_deref(), Some("systems"));
        assert_eq!(bound.started_at, Some(at));
        assert_eq!(bound.elapsed_seconds, Some(0));
        assert_eq!(bound.paused_seconds, Some(0));

        let unnamed = apply_start(None, None, at);
        assert!(unnamed.running && !unnamed.bound, "no activity → unbound");
    }

    #[test]
    fn a_synthesized_start_leaves_server_minted_fields_for_the_reconcile_read() {
        let t = apply_start(Some(9), None, ts("2026-07-15T09:00:00Z"));
        assert_eq!(t.id, None);
        assert_eq!(t.mode, None);
    }

    #[test]
    fn pause_freezes_and_stamps() {
        let at = ts("2026-07-15T09:31:00Z");
        let t = apply_pause(running(), at);
        assert!(t.paused);
        assert_eq!(t.paused_at, Some(at));
        assert_eq!(t.paused_seconds, Some(60), "prior paused time untouched");
    }

    #[test]
    fn resume_folds_the_span_and_clears_the_stamp() {
        let paused_at = ts("2026-07-15T09:31:00Z");
        let resumed_at = ts("2026-07-15T09:41:30Z");
        let t = apply_resume(apply_pause(running(), paused_at), resumed_at);
        assert!(!t.paused);
        assert_eq!(t.paused_at, None);
        assert_eq!(t.paused_seconds, Some(60 + 630));
    }

    #[test]
    fn transitions_are_idempotent_on_wrong_state() {
        let at = ts("2026-07-15T09:31:00Z");
        let already_running = apply_resume(running(), at);
        assert_eq!(
            already_running.paused_seconds,
            Some(60),
            "resume on running is a no-op"
        );
        let paused_once = apply_pause(apply_pause(running(), at), ts("2026-07-15T09:59:00Z"));
        assert_eq!(
            paused_once.paused_at,
            Some(at),
            "second pause keeps the first stamp"
        );
    }

    #[test]
    fn stop_freezes_the_segment_and_clears_the_clock() {
        // 09:00 → 09:47:10 minus 60s banked pause = 2770s ≈ 46 minutes.
        let (after, stop) = apply_stop(running(), ts("2026-07-15T09:47:10Z"));
        assert!(!after.running, "nothing runs after a stop");
        assert_eq!(stop.activity_id, Some(9));
        assert_eq!(stop.elapsed_seconds, 2770);
        assert_eq!(stop.minutes, 46, "rounded to the nearest minute");
        assert_eq!(stop.day, study_day(ts("2026-07-15T09:47:10Z")));
    }

    #[test]
    fn stop_of_a_paused_timer_uses_the_frozen_clock() {
        let paused = apply_pause(running(), ts("2026-07-15T09:31:00Z"));
        let (_, stop) = apply_stop(paused, ts("2026-07-15T09:59:00Z"));
        // (09:31 − 09:00) − 60 banked = 1800s; the 28 paused minutes don't count.
        assert_eq!(stop.elapsed_seconds, 1800);
        assert_eq!(stop.minutes, 30);
    }
}
