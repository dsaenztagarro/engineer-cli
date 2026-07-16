//! The intent record — one deferred write, exactly as the user gestured it.

use serde::{Deserialize, Serialize};

use crate::api::FieldError;

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
        }
    }
}

/// Where an intent stands. `Diverged` keeps the whole RFC 7807 payload so the
/// reconcile surface can render the server's objection verbatim.
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
    fn diverged_state_keeps_the_problem_payload() {
        let json = r#"{
            "state": "diverged",
            "status": 422, "title": "Segment overlaps", "detail": "…",
            "errors": [{"field": "started_at", "detail": "overlaps an existing segment"}]
        }"#;
        let state: IntentState = serde_json::from_str(json).unwrap();
        match state {
            IntentState::Diverged { status, errors, .. } => {
                assert_eq!(status, 422);
                assert_eq!(errors.len(), 1);
            }
            IntentState::Pending => panic!("expected diverged"),
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
