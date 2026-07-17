//! The intent record — one deferred write, exactly as the user gestured it.

use serde::{Deserialize, Serialize};

use crate::api::{ActivityCreate, ConflictInfo, FieldError, NoteInput, TargetCreate};

/// A single deferred write: a mutation the user performed while the wire was
/// down, persisted until it replays. Stored only until it syncs — the queue is
/// never a second ledger (offline-write.brief.md §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    /// Monotonic queue sequence — the replay order.
    pub id: u64,
    /// Sent as the `Idempotency-Key` header on replay, so a re-sent intent
    /// whose ack was lost cannot double-write.
    pub idempotency_key: String,
    /// Ordering domain (`timer`, `activity:42`, …). Replay is global FIFO; a
    /// later relaxation to per-stream FIFO keys on this field.
    pub stream: String,
    /// Wall-clock when the user acted — the true intent time.
    pub queued_at: jiff::Timestamp,
    pub kind: IntentKind,
    pub state: IntentState,
    /// Replay attempts so far (transport failures leave the intent pending).
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Intent {
    pub fn is_pending(&self) -> bool {
        matches!(self.state, IntentState::Pending)
    }

    pub fn is_diverged(&self) -> bool {
        matches!(self.state, IntentState::Diverged { .. })
    }

    pub fn is_parked(&self) -> bool {
        matches!(self.state, IntentState::Parked { .. })
    }
}

/// The typed verb + payload. Verbs that act on the clock carry the wall-clock
/// moment the user acted (`at`) — a pause replayed ten minutes later must not
/// mean "paused at replay time".
// Variants are named `<module><verb>` — the queue spans every module's writes,
// so the shared prefix is the namespace, not noise.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum IntentKind {
    TimerStart {
        activity_id: Option<i64>,
        switch: bool,
        at: jiff::Timestamp,
    },
    TimerPause {
        at: jiff::Timestamp,
    },
    TimerResume {
        at: jiff::Timestamp,
    },
    TimerStop {
        at: jiff::Timestamp,
        /// The local clock's elapsed seconds at the stop — the reconcile pass
        /// compares the server's written segment against this.
        local_elapsed_s: i64,
    },
    TimerBind {
        activity_id: Option<i64>,
        title: Option<String>,
    },
    TimerDiscard,
    /// Declare a plan item — a `planned` activity with `planned_on` set (the
    /// board's `a`, deferred while offline). Carries the whole create body so
    /// the replay re-sends it verbatim; the queued `Idempotency-Key` makes the
    /// re-send safe. Stream `"activity"`: there is no server id yet to order a
    /// per-activity stream on.
    ActivityCreate {
        body: ActivityCreate,
    },
    /// Adjust a plan item's title in place — a deferred `PATCH
    /// /api/v1/activities/:id` (the board's `e`).
    ActivityUpdate {
        id: i64,
        title: String,
    },
    /// Drop a plan item — a deferred archive (the board's `d`, second press).
    ActivityArchive {
        id: i64,
    },
    /// Write the week's retro reflection — a deferred `PATCH
    /// /api/v1/weeks/:iso_week/note` (the board's `i`, deferred while offline).
    /// Carries the whole body so the replay re-sends it verbatim; the route
    /// upserts the single note row, so it is naturally idempotent (a re-send
    /// overwrites with the same body) and replays as a plain call. Stream
    /// `"week:<iso_week>"`: one note per week, ordered on its own.
    WeekNoteWrite {
        iso_week: String,
        body: String,
    },
    /// Declare a weekly target — the Progress `n` flow, deferred while offline.
    /// Carries the whole create body so the replay re-sends it verbatim; the
    /// queued `Idempotency-Key` makes that re-send safe (a lost ack cannot mint
    /// the target twice). Stream `"target"`: there is no server id yet to order a
    /// per-target stream on, so a fresh declare joins the shared target stream.
    TargetCreate {
        body: TargetCreate,
    },
    /// Adjust a target's weekly hours — a deferred `PATCH /api/v1/targets/:id`
    /// (Progress `e`). Replays plain; a closed-version rejection re-addresses the
    /// same hours to the lineage's live row (engineer ADR 0026) inside the replay
    /// rather than diverging. Stream `"target:<id>"`: keyed on the row it edits.
    TargetAdjust {
        id: i64,
        hours: f64,
    },
    /// Retire a target — a deferred `PATCH /api/v1/targets/:id/retire` (Progress
    /// `x`). Closes the lineage while keeping its history (retire ≠ delete);
    /// replays plain, since a second retire is naturally idempotent server-side.
    TargetRetire {
        id: i64,
    },
    /// Capture a study note — a deferred `POST /api/v1/notes` (the quick-capture
    /// overlay's save, and `engineer note capture`). Carries the whole create
    /// body so the replay re-sends it verbatim. Stream `"note"`: there is no
    /// server id yet to order a per-note stream on, so a fresh capture joins the
    /// shared note stream. Unlike the timer / activity creates, notes-create is
    /// NOT in the server's `Idempotency-Key` opt-in set (ADR 0036 — timer
    /// start/stop/pause/resume, segment create, activity create), so it replays
    /// plain: a duplicate on a lost ack is benign (a study note is shelved and
    /// archivable, never double-counted like a logged segment).
    NoteCreate {
        body: NoteInput,
    },
}

impl IntentKind {
    /// The stream this verb orders within.
    pub fn stream(&self) -> String {
        match self {
            Self::TimerStart { .. }
            | Self::TimerPause { .. }
            | Self::TimerResume { .. }
            | Self::TimerStop { .. }
            | Self::TimerBind { .. }
            | Self::TimerDiscard => "timer".into(),
            // A declare has no server id yet — it orders in the shared activity
            // stream; adjust/drop key on the row they act on.
            Self::ActivityCreate { .. } => "activity".into(),
            Self::ActivityUpdate { id, .. } | Self::ActivityArchive { id } => {
                format!("activity:{id}")
            }
            // One note per week, ordered on its own stream — a later reflection
            // for the same week supersedes the earlier queued one in FIFO order.
            Self::WeekNoteWrite { iso_week, .. } => format!("week:{iso_week}"),
            // A declare has no server id yet — it orders in the shared target
            // stream; adjust/retire key on the lineage row they act on.
            Self::TargetCreate { .. } => "target".into(),
            Self::TargetAdjust { id, .. } | Self::TargetRetire { id } => {
                format!("target:{id}")
            }
            // A fresh capture has no server id yet — it orders in the shared
            // note stream, like a plan or target declare.
            Self::NoteCreate { .. } => "note".into(),
        }
    }

    /// The short human word the queue table and status lines print.
    pub fn word(&self) -> &'static str {
        match self {
            Self::TimerStart { .. } => "start",
            Self::TimerPause { .. } => "pause",
            Self::TimerResume { .. } => "resume",
            Self::TimerStop { .. } => "stop",
            Self::TimerBind { .. } => "bind",
            Self::TimerDiscard => "discard",
            Self::ActivityCreate { .. } => "plan",
            Self::ActivityUpdate { .. } => "adjust",
            Self::ActivityArchive { .. } => "drop",
            Self::WeekNoteWrite { .. } => "reflect",
            Self::TargetCreate { .. } => "declare",
            Self::TargetAdjust { .. } => "adjust",
            Self::TargetRetire { .. } => "retire",
            Self::NoteCreate { .. } => "capture",
        }
    }
}

/// Where an intent stands. `Diverged` keeps the whole RFC 7807 payload so the
/// reconcile surface can render the server's objection verbatim. `Parked` is
/// the take-server resolution's kept-for-review state: the intent stays in
/// `queue.json` (never deleted), reads as `parked` in `engineer queue`, is
/// excluded from replay, and leaves only by an explicit gesture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IntentState {
    Pending,
    Diverged {
        status: u16,
        title: String,
        detail: String,
        #[serde(default)]
        type_uri: Option<String>,
        #[serde(default)]
        errors: Vec<FieldError>,
        /// The stable conflict code (engineer#806, ADR 0036), when the server
        /// sent one. Additive: pre-#107 queue documents load as `None` and the
        /// reconcile surfaces fall back to the generic title/detail rendering.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// The coded conflict's extension members (`current`, `resolutions`,
        /// …), kept verbatim so the panel can render the server's side without
        /// a second read. Empty on legacy and code-less problems. Boxed to
        /// match [`ApiError::Problem`](crate::api::ApiError) — the payload rides
        /// the error path verbatim into this state, and boxing keeps the
        /// resolve/replay `Result`s off clippy's `result_large_err`.
        #[serde(default, skip_serializing_if = "ConflictInfo::is_empty")]
        conflict: Box<ConflictInfo>,
    },
    Parked {
        /// Why it was parked — the resolution that put it here, carrying the
        /// server objection's title so the review can still say what happened.
        reason: String,
    },
}

/// A fresh v4-format idempotency key.
pub fn new_idempotency_key() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    let h = |r: std::ops::Range<usize>| {
        bytes[r].iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    };
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_kind_roundtrips_through_json() {
        let kind = IntentKind::TimerStop {
            at: "2026-07-15T09:30:00Z".parse().unwrap(),
            local_elapsed_s: 2832,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains(r#""verb":"timer_stop""#),
            "tagged on verb: {json}"
        );
        let back: IntentKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn target_intents_roundtrip_and_key_their_streams() {
        use crate::api::{TargetCreate, TargetScope};

        let declare = IntentKind::TargetCreate {
            body: TargetCreate {
                scope: TargetScope::Domain(7),
                hours_per_week: 6.0,
            },
        };
        assert_eq!(declare.word(), "declare");
        assert_eq!(declare.stream(), "target", "a fresh declare has no id yet");
        let json = serde_json::to_string(&declare).unwrap();
        assert!(json.contains(r#""verb":"target_create""#), "{json}");
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), declare);

        let adjust = IntentKind::TargetAdjust { id: 42, hours: 8.0 };
        assert_eq!(adjust.word(), "adjust");
        assert_eq!(adjust.stream(), "target:42", "keyed on the row it edits");

        let retire = IntentKind::TargetRetire { id: 42 };
        assert_eq!(retire.word(), "retire");
        assert_eq!(retire.stream(), "target:42");
    }

    #[test]
    fn note_create_intent_roundtrips_and_streams_on_note() {
        use crate::api::{Anchor, NoteInput};

        let capture = IntentKind::NoteCreate {
            body: NoteInput {
                title: "MVCC keeps one version per read-tx".into(),
                content: Some("MVCC keeps one version per read-tx".into()),
                book_id: Some(3),
                anchors: Some(vec![Anchor {
                    page: Some(142),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        };
        assert_eq!(capture.word(), "capture");
        assert_eq!(capture.stream(), "note", "a fresh capture has no id yet");
        let json = serde_json::to_string(&capture).unwrap();
        assert!(json.contains(r#""verb":"note_create""#), "{json}");
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), capture);
    }

    #[test]
    fn diverged_state_keeps_the_problem_payload() {
        // A pre-#107 diverged shape: no `code`, no `conflict` — the additive
        // fields must default so existing queue documents keep loading.
        let json = r#"{
            "state": "diverged",
            "status": 422, "title": "Segment overlaps", "detail": "…",
            "errors": [{"field": "started_at", "detail": "overlaps an existing segment"}]
        }"#;
        let state: IntentState = serde_json::from_str(json).unwrap();
        match state {
            IntentState::Diverged {
                status,
                errors,
                code,
                conflict,
                ..
            } => {
                assert_eq!(status, 422);
                assert_eq!(errors.len(), 1);
                assert!(code.is_none(), "pre-coded documents read as code-less");
                assert!(conflict.is_empty());
            }
            other => panic!("expected diverged, got {other:?}"),
        }
    }

    #[test]
    fn diverged_state_roundtrips_the_coded_conflict() {
        let state = IntentState::Diverged {
            status: 409,
            title: "Timer already running".into(),
            detail: "Stop the running timer first, or pass switch=true.".into(),
            type_uri: Some("https://engineer.example/problems/timer-already-running".into()),
            errors: vec![],
            code: Some("timer-already-running".into()),
            conflict: serde_json::from_value(serde_json::json!({
                "current": {
                    "id": 114, "activity_id": 9, "label": "Ruby OOP Study",
                    "started_at": "2026-07-16T08:59:03Z", "paused": false
                },
                "resolutions": ["switch", "keep-remote"]
            }))
            .unwrap(),
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""code":"timer-already-running""#), "{json}");
        let back: IntentState = serde_json::from_str(&json).unwrap();
        match back {
            IntentState::Diverged { code, conflict, .. } => {
                assert_eq!(code.as_deref(), Some("timer-already-running"));
                let current = conflict.current.expect("the snapshot rides along");
                assert_eq!(current.activity_id, Some(9));
                assert_eq!(conflict.resolutions, vec!["switch", "keep-remote"]);
            }
            other => panic!("expected diverged, got {other:?}"),
        }
    }

    #[test]
    fn parked_state_roundtrips_with_its_reason() {
        let state = IntentState::Parked {
            reason: "took server · Conflict".into(),
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""state":"parked""#), "{json}");
        let back: IntentState = serde_json::from_str(&json).unwrap();
        match back {
            IntentState::Parked { reason } => assert_eq!(reason, "took server · Conflict"),
            other => panic!("expected parked, got {other:?}"),
        }
    }

    #[test]
    fn idempotency_keys_are_v4_shaped_and_unique() {
        let a = new_idempotency_key();
        let b = new_idempotency_key();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.chars().nth(14), Some('4'), "version nibble: {a}");
    }
}
