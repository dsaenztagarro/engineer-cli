//! The fold: cached server snapshot ⊕ pending intents → the *effective* local
//! timer every offline read renders (offline-write.brief.md §3 — the queue is
//! not a second ledger; the fold of it over the last server truth is what the
//! user sees).
//!
//! Pure over its inputs and computed fresh on every read — callers re-read
//! the queue each time, so a drained, dropped, or newly-enqueued intent shows
//! up on the very next read with nothing to invalidate. Nothing here writes:
//! the effective timer is never persisted, and `timer-cache.json` stays
//! server-truth-only.

use crate::api::Timer;
use crate::timer_cache::StaleTimer;
use crate::timer_clock;

use super::intent::{Intent, IntentKind};

/// Where the effective timer came from — the honesty fields a caller renders
/// next to the folded clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provenance {
    /// Seconds since the last server truth: the cached snapshot's age, or —
    /// when the whole session is local (a start with nothing cached) — the
    /// age of the oldest folded intent.
    pub stale_age_s: i64,
    /// How many pending timer intents were folded into the result.
    pub queue_depth: usize,
}

/// Replay the pending timer intents over the last-known server snapshot,
/// producing the effective local timer with `elapsed_seconds` materialized at
/// `now`. The replays go through the same `timer_clock` transitions the write
/// path synthesizes with, so a read after an offline write shows exactly what
/// the write returned, advanced to now — an offline pause freezes the clock,
/// an offline resume advances it, an offline stop reads as nothing running.
///
/// Ordering mirrors the drain: queue order, and nothing folds past the first
/// diverged intent — an open choice gates the picture exactly as it gates the
/// replay. Returns `None` when there is nothing to compose: no cached
/// snapshot, and no queued start to seed a purely-local session from.
pub fn fold_timer(
    cached: Option<&StaleTimer>,
    intents: &[Intent],
    now: jiff::Timestamp,
) -> Option<(Timer, Provenance)> {
    let mut timer = cached.map(|s| anchored(s.timer.clone(), snapshot_moment(s, now)));
    let mut queue_depth = 0usize;
    let mut oldest_folded: Option<jiff::Timestamp> = None;

    let mut ordered: Vec<&Intent> = intents.iter().filter(|i| i.stream == "timer").collect();
    ordered.sort_by_key(|i| i.id);
    for intent in ordered {
        if intent.is_diverged() {
            break; // nothing folds past an open choice — the drain's own rule
        }
        if !intent.is_pending() {
            continue;
        }
        timer = Some(match (&intent.kind, timer.take()) {
            (
                IntentKind::TimerStart {
                    activity_id, at, ..
                },
                _,
            ) => {
                // A queued start supersedes whatever ran before (the enqueue
                // path settles the switch question). The intent carries no
                // label — the reconcile re-read names the activity.
                timer_clock::apply_start(*activity_id, None, *at)
            }
            // Without a base there is nothing the other verbs can honestly
            // act on. The write path never enqueues them blind
            // (`QueuedClient::defer` requires a snapshot), so refuse rather
            // than invent one.
            (_, None) => return None,
            (IntentKind::TimerPause { at }, Some(t)) => timer_clock::apply_pause(t, *at),
            (IntentKind::TimerResume { at }, Some(t)) => timer_clock::apply_resume(t, *at),
            (IntentKind::TimerStop { at, .. }, Some(t)) => timer_clock::apply_stop(t, *at).0,
            (IntentKind::TimerBind { activity_id, title }, Some(mut t)) => {
                t.bound = true;
                t.activity_id = *activity_id;
                if title.is_some() {
                    t.label = title.clone();
                }
                t
            }
            (IntentKind::TimerDiscard, Some(_)) => Timer::default(),
        });
        queue_depth += 1;
        oldest_folded.get_or_insert(intent.queued_at);
    }

    let mut effective = timer?;
    if effective.running {
        // Materialize the clock so `elapsed_seconds` *is* the folded local
        // clock at `now` (frozen when the fold left it paused, advancing when
        // running) — the headless renderers read this field verbatim.
        effective.elapsed_seconds = Some(timer_clock::elapsed(&effective, now));
    }
    let stale_age_s = cached
        .map(|s| s.age_secs)
        .or_else(|| oldest_folded.map(|at| (now.as_second() - at.as_second()).max(0)))
        .unwrap_or(0);
    Some((
        effective,
        Provenance {
            stale_age_s,
            queue_depth,
        },
    ))
}

/// The wall-clock moment the snapshot was taken, recovered from its age.
fn snapshot_moment(s: &StaleTimer, now: jiff::Timestamp) -> jiff::Timestamp {
    jiff::Timestamp::from_second(now.as_second() - s.age_secs.max(0)).unwrap_or(now)
}

/// Give an older running payload (no `started_at`) the anchor the arithmetic
/// needs, derived from what the snapshot did say: at `snapshot_at` the clock
/// read `elapsed_seconds` with `paused_seconds` banked, so
/// `started_at = frozen_at − elapsed_seconds − paused_seconds`, where
/// `frozen_at` is the pause stamp (defaulted to the snapshot moment, the
/// closest local truth, when the payload omitted it too) or the snapshot
/// moment itself. Anchored once, the transitions and `timer_clock::elapsed`
/// are exact over it.
fn anchored(mut t: Timer, snapshot_at: jiff::Timestamp) -> Timer {
    if !t.running || t.started_at.is_some() {
        return t;
    }
    if t.paused && t.paused_at.is_none() {
        t.paused_at = Some(snapshot_at);
    }
    let frozen_at = t.paused_at.filter(|_| t.paused).unwrap_or(snapshot_at);
    let started =
        frozen_at.as_second() - t.elapsed_seconds.unwrap_or(0) - t.paused_seconds.unwrap_or(0);
    t.started_at = jiff::Timestamp::from_second(started).ok();
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::FieldError;
    use crate::queue::intent::IntentState;

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    /// `now` for every case; snapshots and intents are placed relative to it.
    fn now() -> jiff::Timestamp {
        ts("2026-07-15T10:00:00Z")
    }

    fn timer(v: serde_json::Value) -> Timer {
        serde_json::from_value(v).unwrap()
    }

    fn cached(v: serde_json::Value, age_secs: i64) -> StaleTimer {
        StaleTimer {
            timer: timer(v),
            age_secs,
        }
    }

    fn intent(id: u64, kind: IntentKind) -> Intent {
        let queued_at = match &kind {
            IntentKind::TimerStart { at, .. }
            | IntentKind::TimerPause { at }
            | IntentKind::TimerResume { at }
            | IntentKind::TimerStop { at, .. } => *at,
            _ => ts("2026-07-15T09:59:00Z"),
        };
        Intent {
            id,
            idempotency_key: format!("key-{id}"),
            stream: kind.stream(),
            queued_at,
            kind,
            state: IntentState::Pending,
            attempts: 0,
            last_error: None,
        }
    }

    fn diverged(mut i: Intent) -> Intent {
        i.state = IntentState::Diverged {
            status: 422,
            title: "Segment overlaps".into(),
            detail: String::new(),
            type_uri: None,
            errors: Vec::<FieldError>::new(),
            code: None,
            conflict: Default::default(),
        };
        i
    }

    /// A running snapshot anchored at 09:00, read by the server 5 minutes ago.
    fn running_cache() -> StaleTimer {
        cached(
            serde_json::json!({
                "running": true, "bound": true, "activity_id": 9, "label": "systems",
                "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 3300
            }),
            300,
        )
    }

    #[test]
    fn empty_queue_still_advances_the_running_clock() {
        let (t, prov) = fold_timer(Some(&running_cache()), &[], now()).unwrap();
        assert_eq!(t.elapsed_seconds, Some(3600), "arithmetic, not the 3300");
        assert_eq!(prov.stale_age_s, 300);
        assert_eq!(prov.queue_depth, 0);
    }

    #[test]
    fn pause_freezes_then_resume_advances() {
        let pause = intent(
            1,
            IntentKind::TimerPause {
                at: ts("2026-07-15T09:50:00Z"),
            },
        );
        let resume = intent(
            2,
            IntentKind::TimerResume {
                at: ts("2026-07-15T09:55:00Z"),
            },
        );

        // Pause alone: frozen at the pause moment (09:00 → 09:50 = 3000s).
        let (t, prov) =
            fold_timer(Some(&running_cache()), std::slice::from_ref(&pause), now()).unwrap();
        assert!(t.paused);
        assert_eq!(t.elapsed_seconds, Some(3000), "frozen, not still counting");
        assert_eq!(prov.queue_depth, 1);

        // Pause then resume: 5 paused minutes banked, advancing again.
        let (t, prov) = fold_timer(Some(&running_cache()), &[pause, resume], now()).unwrap();
        assert!(!t.paused);
        assert_eq!(t.paused_seconds, Some(300));
        assert_eq!(t.elapsed_seconds, Some(3300), "3600 wall − 300 paused");
        assert_eq!(prov.queue_depth, 2);
    }

    #[test]
    fn start_when_none_cached_seeds_a_purely_local_session() {
        let start = intent(
            1,
            IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:58:00Z"),
            },
        );
        let (t, prov) = fold_timer(None, &[start], now()).unwrap();
        assert!(t.running && t.bound);
        assert_eq!(t.activity_id, Some(9));
        assert_eq!(t.elapsed_seconds, Some(120));
        assert_eq!(prov.stale_age_s, 120, "age of the oldest folded intent");
        assert_eq!(prov.queue_depth, 1);
    }

    #[test]
    fn stop_and_discard_leave_nothing_running() {
        for kind in [
            IntentKind::TimerStop {
                at: ts("2026-07-15T09:59:00Z"),
                local_elapsed_s: 3540,
            },
            IntentKind::TimerDiscard,
        ] {
            let word = kind.word();
            let (t, prov) = fold_timer(Some(&running_cache()), &[intent(1, kind)], now()).unwrap();
            assert!(!t.running, "{word} ends the session");
            assert_eq!(prov.queue_depth, 1, "{word}");
        }
    }

    #[test]
    fn bind_marks_the_timer_bound() {
        let unbound = cached(
            serde_json::json!({
                "running": true, "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 3300
            }),
            300,
        );
        let bind = intent(
            1,
            IntentKind::TimerBind {
                activity_id: Some(42),
                title: Some("Implement Raft".into()),
            },
        );
        let (t, _) = fold_timer(Some(&unbound), &[bind], now()).unwrap();
        assert!(t.bound);
        assert_eq!(t.activity_id, Some(42));
        assert_eq!(t.label.as_deref(), Some("Implement Raft"));
    }

    #[test]
    fn a_diverged_intent_gates_the_fold_like_the_drain() {
        let gated = diverged(intent(
            1,
            IntentKind::TimerPause {
                at: ts("2026-07-15T09:50:00Z"),
            },
        ));
        let behind = intent(
            2,
            IntentKind::TimerResume {
                at: ts("2026-07-15T09:55:00Z"),
            },
        );
        let (t, prov) = fold_timer(Some(&running_cache()), &[gated, behind], now()).unwrap();
        assert!(!t.paused, "nothing folded past the open choice");
        assert_eq!(t.elapsed_seconds, Some(3600), "the plain advanced snapshot");
        assert_eq!(prov.queue_depth, 0);
    }

    #[test]
    fn a_parked_intent_never_folds() {
        let mut parked = intent(
            1,
            IntentKind::TimerPause {
                at: ts("2026-07-15T09:50:00Z"),
            },
        );
        parked.state = IntentState::Parked {
            reason: "took server · Conflict".into(),
        };
        let (t, prov) = fold_timer(Some(&running_cache()), &[parked], now()).unwrap();
        assert!(
            !t.paused,
            "an abandoned session's gesture stays out of the picture"
        );
        assert_eq!(t.elapsed_seconds, Some(3600), "the plain advanced snapshot");
        assert_eq!(prov.queue_depth, 0);
    }

    #[test]
    fn verbs_without_any_base_refuse() {
        let pause = intent(
            1,
            IntentKind::TimerPause {
                at: ts("2026-07-15T09:50:00Z"),
            },
        );
        assert!(fold_timer(None, &[pause], now()).is_none());
        assert!(fold_timer(None, &[], now()).is_none(), "nothing to compose");
    }

    #[test]
    fn older_payloads_without_started_at_are_anchored_from_the_snapshot_age() {
        // Snapshot read 60s ago said 1800s elapsed, no anchor fields at all.
        let old = cached(
            serde_json::json!({ "running": true, "elapsed_seconds": 1800 }),
            60,
        );

        let (t, _) = fold_timer(Some(&old), &[], now()).unwrap();
        assert_eq!(t.elapsed_seconds, Some(1860), "advanced by the age");

        let pause = intent(
            1,
            IntentKind::TimerPause {
                at: ts("2026-07-15T09:59:30Z"), // 30s after the snapshot
            },
        );
        let (t, _) = fold_timer(Some(&old), &[pause], now()).unwrap();
        assert_eq!(t.elapsed_seconds, Some(1830), "frozen at the pause moment");
    }
}
