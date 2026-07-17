//! The fold: server truth ⊕ pending intents → the *effective* picture every
//! offline read renders (offline-write.brief.md §3 — the queue is not a
//! second ledger; the fold of it over the last server truth is what the user
//! sees). [`fold_timer`] composes the effective local clock; its sibling
//! [`fold_activities`] mixes still-queued activity/segment writes into a
//! fetched list as `◔ … provisional · queued` rows (#109, §Segment audit ·
//! mixed).
//!
//! Pure over their inputs and computed fresh on every read — callers re-read
//! the queue each time, so a drained, dropped, or newly-enqueued intent shows
//! up on the very next read with nothing to invalidate. Nothing here writes:
//! the effective timer is never persisted, and `timer-cache.json` stays
//! server-truth-only.

use std::collections::HashSet;

use crate::api::{Activity, Timer};
use crate::timer_cache::StaleTimer;
use crate::timer_clock;

use super::intent::{provisional_id, Intent, IntentKind};

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
            // Non-timer intents never reach here — the `stream == "timer"` filter
            // above admits only the timer stream — but the match stays exhaustive
            // over `IntentKind`. The timer fold has nothing to say about a plan
            // write or a week note; a later slice may fold an effective week the
            // same way.
            (
                IntentKind::ActivityCreate { .. }
                | IntentKind::ActivityUpdate { .. }
                | IntentKind::ActivityArchive { .. }
                | IntentKind::ActivityComplete { .. }
                | IntentKind::ActivityDuplicate { .. }
                | IntentKind::ActivityUnarchive { .. }
                | IntentKind::SegmentCreate { .. }
                | IntentKind::WeekNoteWrite { .. }
                | IntentKind::TargetCreate { .. }
                | IntentKind::TargetAdjust { .. }
                | IntentKind::TargetRetire { .. }
                | IntentKind::NoteCreate { .. }
                | IntentKind::NoteUpdate { .. }
                | IntentKind::NoteArchive { .. }
                | IntentKind::NoteUnarchive { .. }
                | IntentKind::NoteUnlink { .. }
                | IntentKind::BookUpdate { .. },
                _,
            ) => unreachable!("only timer-stream intents reach the timer fold"),
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

/// One row of the folded activities read: the fetched (or synthesized) row
/// plus what the queue says about it. A provisional row — a still-queued
/// `ActivityCreate` — carries the negative `provisional_id` and renders
/// `◔ … provisional · queued` (§Segment audit · mixed); a confirmed row with
/// `queued_minutes > 0` has pending `SegmentCreate` minutes folded onto it,
/// rendered `◔ +Nm queued` next to the server-confirmed duration.
#[derive(Debug, Clone)]
pub struct FoldedActivity {
    pub activity: Activity,
    /// Minutes from pending `SegmentCreate` intents folded onto this row.
    pub queued_minutes: u32,
}

impl FoldedActivity {
    fn confirmed(activity: Activity) -> Self {
        Self {
            activity,
            queued_minutes: 0,
        }
    }

    /// Whether the row itself is a still-queued create — the negative
    /// provisional id is the marker, exactly as everywhere else (#108).
    pub fn is_provisional(&self) -> bool {
        self.activity.id < 0
    }
}

/// Fold the pending activity-write intents into a fetched activities list —
/// the read-time composition the Activities table (and any segment-audit
/// surface) renders, `fold_timer`'s sibling (§Segment audit · mixed). Pure
/// over its inputs, computed fresh on every read, never written into any
/// cache: a drained or dropped intent disappears from the picture on the
/// very next fetch.
///
/// - A pending `ActivityCreate` appends a provisional row: negative
///   `provisional_id`, the body's own title/kind/minutes, anchored at the
///   moment the user gestured (`queued_at`).
/// - A pending `SegmentCreate` folds its minutes onto the row it belongs to —
///   a real fetched row, or a provisional one from earlier in the queue (the
///   provisional parent id matches the synthesized row's id). Minutes on an
///   activity that isn't in `rows` (another page, another filter) are
///   dropped from *this* view only — the queue still holds them.
/// - Ordering mirrors the drain's per-stream contract: nothing folds past the
///   first diverged intent in its stream (an open choice gates the picture as
///   it gates the replay), and parked intents never fold.
pub fn fold_activities(rows: Vec<Activity>, intents: &[Intent]) -> Vec<FoldedActivity> {
    let mut folded: Vec<FoldedActivity> = rows.into_iter().map(FoldedActivity::confirmed).collect();

    let mut ordered: Vec<&Intent> = intents.iter().collect();
    ordered.sort_by_key(|i| i.id);
    let mut blocked: HashSet<&str> = HashSet::new();
    for intent in ordered {
        if intent.is_diverged() {
            blocked.insert(intent.stream.as_str());
            continue; // a diverged write is a loud choice, never a calm ◔ row
        }
        if !intent.is_pending() || blocked.contains(intent.stream.as_str()) {
            continue;
        }
        match &intent.kind {
            IntentKind::ActivityCreate { body } => {
                folded.push(FoldedActivity::confirmed(Activity {
                    id: provisional_id(intent.id),
                    title: body.title.clone(),
                    kind: body.kind.clone(),
                    status: Some("planned".into()),
                    duration_minutes: body.duration_minutes,
                    started_at: Some(intent.queued_at),
                    ..Default::default()
                }));
            }
            IntentKind::SegmentCreate {
                activity_id,
                minutes,
                ..
            } => {
                // A segment whose parent create is diverged references a
                // provisional id with no folded row — it falls through this
                // find and stays out of the picture, like the drain holds it.
                if let Some(row) = folded.iter_mut().find(|f| f.activity.id == *activity_id) {
                    row.queued_minutes += *minutes;
                }
            }
            _ => {}
        }
    }
    folded
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

    // --- fold_activities (#109, §Segment audit · mixed) ---------------------

    fn parked(mut i: Intent) -> Intent {
        i.state = IntentState::Parked {
            reason: "skipped · Segment overlaps".into(),
        };
        i
    }

    fn create_intent(id: u64, title: &str, minutes: Option<u32>) -> Intent {
        use crate::api::ActivityCreate;
        intent(
            id,
            IntentKind::ActivityCreate {
                body: ActivityCreate {
                    title: title.into(),
                    duration_minutes: minutes,
                    kind: Some("build".into()),
                    ..Default::default()
                },
            },
        )
    }

    fn segment_intent(id: u64, activity_id: i64, minutes: u32) -> Intent {
        intent(
            id,
            IntentKind::SegmentCreate {
                activity_id,
                started_at: ts("2026-07-15T14:02:00Z"),
                minutes,
            },
        )
    }

    fn fetched(id: i64, title: &str, minutes: u32) -> Activity {
        Activity {
            id,
            title: title.into(),
            duration_minutes: Some(minutes),
            status: Some("completed".into()),
            ..Default::default()
        }
    }

    #[test]
    fn queued_creates_render_as_provisional_rows_mixed_after_the_confirmed() {
        let rows = vec![fetched(9, "Raft leader election", 52)];
        let intents = [create_intent(3, "Paxos made live", Some(20))];

        let folded = fold_activities(rows, &intents);
        assert_eq!(folded.len(), 2, "confirmed + provisional, one list");
        assert!(!folded[0].is_provisional());
        let row = &folded[1];
        assert!(row.is_provisional());
        assert_eq!(row.activity.id, -3, "the provisional id is -(intent.id)");
        assert_eq!(row.activity.title, "Paxos made live");
        assert_eq!(row.activity.duration_minutes, Some(20));
        assert_eq!(
            row.activity.started_at,
            Some(ts("2026-07-15T09:59:00Z")),
            "anchored at the gesture, so WHEN renders"
        );
    }

    #[test]
    fn queued_segment_minutes_fold_onto_the_row_they_belong_to() {
        let rows = vec![
            fetched(9, "Raft leader election", 52),
            fetched(12, "DDIA", 30),
        ];
        let intents = [segment_intent(4, 9, 20), segment_intent(5, 9, 14)];

        let folded = fold_activities(rows, &intents);
        assert_eq!(folded.len(), 2, "no new rows — the minutes ride the parent");
        assert_eq!(folded[0].queued_minutes, 34, "20m + 14m queued on #9");
        assert_eq!(
            folded[0].activity.duration_minutes,
            Some(52),
            "the confirmed duration is never rewritten — the queued minutes ride beside it"
        );
        assert_eq!(folded[1].queued_minutes, 0);
    }

    #[test]
    fn a_queued_segment_on_a_queued_create_folds_onto_the_provisional_row() {
        let create = create_intent(3, "Paxos made live", Some(20));
        let segment = segment_intent(4, -3, 15); // references the create's provisional id
        let folded = fold_activities(vec![], &[create, segment]);
        assert_eq!(folded.len(), 1);
        assert!(folded[0].is_provisional());
        assert_eq!(folded[0].queued_minutes, 15);
    }

    #[test]
    fn an_empty_queue_leaves_the_fetched_rows_untouched() {
        // The after-drain read: intents left the queue, the fold is identity.
        let folded = fold_activities(vec![fetched(9, "Raft", 52)], &[]);
        assert_eq!(folded.len(), 1);
        assert!(!folded[0].is_provisional());
        assert_eq!(folded[0].queued_minutes, 0);
    }

    #[test]
    fn a_diverged_create_never_folds_and_holds_its_dependent_segment() {
        // The rejected create is a loud choice, not a calm ◔ row; the segment
        // referencing its provisional id has no row to land on — out of the
        // picture exactly as the drain holds it.
        let gated = diverged(create_intent(3, "Paxos made live", Some(20)));
        let dependent = segment_intent(4, -3, 15);
        let folded = fold_activities(vec![fetched(9, "Raft", 52)], &[gated, dependent]);
        assert_eq!(folded.len(), 1, "only the confirmed row");
        assert_eq!(folded[0].queued_minutes, 0);
    }

    #[test]
    fn a_diverged_intent_gates_only_its_stream_in_the_fold() {
        // Streams mirror the drain: a diverged segment on activity:9 gates
        // later intents on activity:9, while the shared activity stream's
        // create still folds.
        let gated = diverged(segment_intent(3, 9, 45));
        let behind = segment_intent(4, 9, 10); // same stream — held
        let other = create_intent(5, "Paxos made live", None); // stream "activity" — flows
        let folded = fold_activities(vec![fetched(9, "Raft", 52)], &[gated, behind, other]);
        assert_eq!(folded[0].queued_minutes, 0, "nothing folds past the choice");
        assert_eq!(folded.len(), 2, "the unrelated declare still folds");
        assert!(folded[1].is_provisional());
    }

    #[test]
    fn a_parked_intent_never_folds_into_the_activities_read() {
        let skipped = parked(segment_intent(3, 9, 45));
        let folded = fold_activities(vec![fetched(9, "Raft", 52)], &[skipped]);
        assert_eq!(folded[0].queued_minutes, 0, "kept for review, not a row");
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
