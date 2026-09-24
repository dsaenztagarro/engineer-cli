//! The intent record — one deferred write, exactly as the user gestured it.

use serde::{Deserialize, Serialize};

use crate::api::{ActivityCreate, BookUpdate, ConflictInfo, FieldError, NoteInput, TargetCreate};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub id: u64,
    pub idempotency_key: String,
    pub stream: String,
    pub queued_at: jiff::Timestamp,
    pub kind: IntentKind,
    pub state: IntentState,
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
        local_elapsed_s: i64,
    },
    TimerBind {
        activity_id: Option<i64>,
        title: Option<String>,
    },
    TimerDiscard,
    ActivityCreate {
        body: ActivityCreate,
    },
    ActivityUpdate {
        id: i64,
        title: String,
    },
    ActivityArchive {
        id: i64,
    },
    ActivityComplete {
        id: i64,
    },
    /// Replays plain: a lost ack can mint a second copy, accepted in ADR 0004.
    ActivityDuplicate {
        id: i64,
    },
    ActivityUnarchive {
        id: i64,
    },
    /// `activity_id` may be a still-queued create's [`provisional_id`]; the replay
    /// stitches the real id on before this posts.
    SegmentCreate {
        activity_id: i64,
        started_at: jiff::Timestamp,
        minutes: u32,
    },
    WeekNoteWrite {
        iso_week: String,
        body: String,
    },
    TargetCreate {
        body: TargetCreate,
    },
    TargetAdjust {
        id: i64,
        hours: f64,
    },
    TargetRetire {
        id: i64,
    },
    /// Replays plain (outside the server's `Idempotency-Key` set): a lost ack can
    /// shelve a second copy.
    NoteCreate {
        body: NoteInput,
    },
    NoteUpdate {
        id: i64,
        body: NoteInput,
    },
    NoteArchive {
        id: i64,
    },
    NoteUnarchive {
        id: i64,
    },
    NoteUnlink {
        id: i64,
    },
    BookUpdate {
        id: i64,
        body: BookUpdate,
    },
}

impl IntentKind {
    pub fn stream(&self) -> String {
        match self {
            Self::TimerStart { .. }
            | Self::TimerPause { .. }
            | Self::TimerResume { .. }
            | Self::TimerStop { .. }
            | Self::TimerBind { .. }
            | Self::TimerDiscard => "timer".into(),
            Self::ActivityCreate { .. } => "activity".into(),
            Self::ActivityUpdate { id, .. }
            | Self::ActivityArchive { id }
            | Self::ActivityComplete { id }
            | Self::ActivityDuplicate { id }
            | Self::ActivityUnarchive { id } => {
                format!("activity:{id}")
            }
            Self::SegmentCreate { activity_id, .. } => format!("activity:{activity_id}"),
            Self::WeekNoteWrite { iso_week, .. } => format!("week:{iso_week}"),
            Self::TargetCreate { .. } => "target".into(),
            Self::TargetAdjust { id, .. } | Self::TargetRetire { id } => {
                format!("target:{id}")
            }
            Self::NoteCreate { .. } => "note".into(),
            Self::NoteUpdate { id, .. }
            | Self::NoteArchive { id }
            | Self::NoteUnarchive { id }
            | Self::NoteUnlink { id } => format!("note:{id}"),
            Self::BookUpdate { id, .. } => format!("book:{id}"),
        }
    }

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
            Self::ActivityComplete { .. } => "complete",
            Self::ActivityDuplicate { .. } => "duplicate",
            Self::ActivityUnarchive { .. } => "unarchive",
            Self::SegmentCreate { .. } => "log",
            Self::WeekNoteWrite { .. } => "reflect",
            Self::TargetCreate { .. } => "declare",
            Self::TargetAdjust { .. } => "adjust",
            Self::TargetRetire { .. } => "retire",
            Self::NoteCreate { .. } => "capture",
            Self::NoteUpdate { .. } => "edit",
            Self::NoteArchive { .. } => "archive",
            Self::NoteUnarchive { .. } => "unarchive",
            Self::NoteUnlink { .. } => "unlink",
            Self::BookUpdate { .. } => "book",
        }
    }
}

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Boxed like [`ApiError::Problem`](crate::api::ApiError), keeping the
        /// resolve/replay `Result`s off clippy's `result_large_err`.
        #[serde(default, skip_serializing_if = "ConflictInfo::is_empty")]
        conflict: Box<ConflictInfo>,
    },
    Parked {
        reason: String,
    },
}

/// Unique per intent, unlike the flat `-1` note/target sentinels: the replay
/// id-map keys on it to rewrite references to this create.
pub fn provisional_id(intent_id: u64) -> i64 {
    -(intent_id as i64)
}

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
    fn activity_lifecycle_verbs_roundtrip_and_key_their_row_stream() {
        for (kind, word) in [
            (IntentKind::ActivityComplete { id: 42 }, "complete"),
            (IntentKind::ActivityDuplicate { id: 42 }, "duplicate"),
            (IntentKind::ActivityUnarchive { id: 42 }, "unarchive"),
        ] {
            assert_eq!(kind.word(), word);
            assert_eq!(kind.stream(), "activity:42", "keyed on the row it acts on");
            let json = serde_json::to_string(&kind).unwrap();
            assert!(
                json.contains(&format!(r#""verb":"activity_{word}""#)),
                "{json}"
            );
            assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), kind);
        }
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
    fn note_write_intents_roundtrip_and_key_their_note_stream() {
        use crate::api::{Anchor, NoteInput};

        let update = IntentKind::NoteUpdate {
            id: 7,
            body: NoteInput {
                title: "MVCC".into(),
                anchors: Some(vec![Anchor {
                    chapter_id: Some(3),
                    section_id: Some(32),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        };
        assert_eq!(update.word(), "edit");
        assert_eq!(update.stream(), "note:7", "keyed on the note it edits");
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains(r#""verb":"note_update""#), "{json}");
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), update);

        for (kind, word) in [
            (IntentKind::NoteArchive { id: 7 }, "archive"),
            (IntentKind::NoteUnarchive { id: 7 }, "unarchive"),
            (IntentKind::NoteUnlink { id: 7 }, "unlink"),
        ] {
            assert_eq!(kind.word(), word);
            assert_eq!(kind.stream(), "note:7", "keyed on the note it acts on");
            let json = serde_json::to_string(&kind).unwrap();
            assert!(json.contains(&format!(r#""verb":"note_{word}""#)), "{json}");
            assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), kind);
        }
    }

    #[test]
    fn note_update_carries_the_omit_vs_replace_anchors_contract_through_serde() {
        use crate::api::NoteInput;

        let omit = IntentKind::NoteUpdate {
            id: 9,
            body: NoteInput {
                title: "kept".into(),
                anchors: None,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&omit).unwrap();
        assert!(
            !json.contains("anchors"),
            "an omitted anchor stays omitted: {json}"
        );
        match serde_json::from_str::<IntentKind>(&json).unwrap() {
            IntentKind::NoteUpdate { body, .. } => assert!(body.anchors.is_none()),
            other => panic!("expected NoteUpdate, got {other:?}"),
        }

        let replace = IntentKind::NoteUpdate {
            id: 9,
            body: NoteInput {
                title: "cleared".into(),
                anchors: Some(vec![]),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&replace).unwrap();
        assert!(
            json.contains(r#""anchors":[]"#),
            "a replace sends the field: {json}"
        );
        match serde_json::from_str::<IntentKind>(&json).unwrap() {
            IntentKind::NoteUpdate { body, .. } => {
                assert_eq!(body.anchors, Some(vec![]), "Some(empty) is not None")
            }
            other => panic!("expected NoteUpdate, got {other:?}"),
        }
    }

    #[test]
    fn book_update_intent_roundtrips_and_keys_its_book_stream() {
        use crate::api::{BookStatus, BookUpdate};

        let status = IntentKind::BookUpdate {
            id: 7,
            body: BookUpdate {
                status: Some(BookStatus::Completed),
                ..Default::default()
            },
        };
        assert_eq!(status.word(), "book");
        assert_eq!(status.stream(), "book:7", "keyed on the book it edits");
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains(r#""verb":"book_update""#), "{json}");
        assert!(json.contains(r#""status":"completed""#), "{json}");
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), status);

        let page = IntentKind::BookUpdate {
            id: 7,
            body: BookUpdate {
                current_page: Some(142),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&page).unwrap();
        assert!(
            !json.contains("status"),
            "an unset field stays omitted: {json}"
        );
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), page);
    }

    #[test]
    fn segment_create_intent_roundtrips_and_streams_on_its_activity() {
        let append = IntentKind::SegmentCreate {
            activity_id: 9,
            started_at: "2026-07-15T13:00:00Z".parse().unwrap(),
            minutes: 20,
        };
        assert_eq!(append.word(), "log");
        assert_eq!(append.stream(), "activity:9", "orders behind its activity");
        let json = serde_json::to_string(&append).unwrap();
        assert!(json.contains(r#""verb":"segment_create""#), "{json}");
        assert_eq!(serde_json::from_str::<IntentKind>(&json).unwrap(), append);

        let provisional = IntentKind::SegmentCreate {
            activity_id: -7,
            started_at: "2026-07-15T13:00:00Z".parse().unwrap(),
            minutes: 20,
        };
        assert_eq!(provisional.stream(), "activity:-7");
    }

    #[test]
    fn a_diverged_state_written_before_coded_conflicts_still_loads() {
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
