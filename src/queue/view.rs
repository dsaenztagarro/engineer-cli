//! Shared row-shaping for the intent-log surfaces — the `# INTENT TARGET AGE
//! STATE` columns rendered by both `engineer queue` (the headless table,
//! `queue_cli.rs`) and the Queue inspector screen (`app::screens::queue`). One
//! spelling of each cell so the two faces of the same `queue::pending()` read
//! can never drift (offline-write.dc.html §Queue inspector).
//!
//! This shapes *values*, not layout: the CLI pads them into fixed columns and
//! the TUI wraps them in ratatui cells, but both take the id, verb word,
//! target stream, age read, and state word from here.

use super::intent::{Intent, IntentState};

/// The intent-log column headers, in order — the one source of truth for both
/// the CLI table header and the TUI board header.
pub const HEADERS: [&str; 5] = ["#", "INTENT", "TARGET", "AGE", "STATE"];

/// One intent as its five display cells. `state` is the bare state word; each
/// surface paints it in its own idiom (the CLI's ANSI, the TUI's theme colour).
pub struct Row {
    pub id: String,
    pub intent: String,
    pub target: String,
    pub age: String,
    pub state: &'static str,
}

/// Shape an intent into its row cells against `now` (epoch seconds).
pub fn row(intent: &Intent, now: i64) -> Row {
    Row {
        id: intent.id.to_string(),
        intent: intent.kind.word().to_string(),
        target: intent.stream.clone(),
        age: fmt_age(age_s(intent, now)),
        state: state_word(intent),
    }
}

/// Seconds since the intent was queued, floored at zero (a clock skew never
/// reads as a negative age).
pub fn age_s(intent: &Intent, now: i64) -> i64 {
    (now - intent.queued_at.as_second()).max(0)
}

/// `42s` · `7m` · `3h` · `2d` — the queue's one-glance age read.
pub fn fmt_age(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The stored state as its one-word read — the shipped vocabulary
/// (pending / diverged / parked), keyed on the store's own states.
pub fn state_word(intent: &Intent) -> &'static str {
    match intent.state {
        IntentState::Pending => "pending",
        IntentState::Diverged { .. } => "diverged",
        IntentState::Parked { .. } => "parked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{IntentKind, QueueStore};

    fn tmp_store(tag: &str) -> QueueStore {
        let dir = std::env::temp_dir().join(format!("engineer-view-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        QueueStore::at(dir.join("queue.json"))
    }

    #[test]
    fn ages_read_at_a_glance() {
        assert_eq!(fmt_age(42), "42s");
        assert_eq!(fmt_age(420), "7m");
        assert_eq!(fmt_age(7200), "2h");
        assert_eq!(fmt_age(200_000), "2d");
    }

    #[test]
    fn row_shapes_the_five_cells_and_the_state_word() {
        let store = tmp_store("row");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        let now = pause.queued_at.as_second() + 90; // 1m30s later
        let r = row(&pause, now);
        assert_eq!(r.id, pause.id.to_string());
        assert_eq!(r.intent, "pause");
        assert_eq!(r.target, "timer");
        assert_eq!(r.age, "1m");
        assert_eq!(r.state, "pending");
    }
}
