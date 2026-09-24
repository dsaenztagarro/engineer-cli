//! Row cells shared by the `engineer queue` table and the queue screen.

use super::intent::{Intent, IntentState};

pub const HEADERS: [&str; 5] = ["#", "INTENT", "TARGET", "AGE", "STATE"];

pub struct Row {
    pub id: String,
    pub intent: String,
    pub target: String,
    pub age: String,
    pub state: &'static str,
}

pub fn row(intent: &Intent, now: i64) -> Row {
    Row {
        id: intent.id.to_string(),
        intent: intent.kind.word().to_string(),
        target: intent.stream.clone(),
        age: fmt_age(age_s(intent, now)),
        state: state_word(intent),
    }
}

pub fn age_s(intent: &Intent, now: i64) -> i64 {
    (now - intent.queued_at.as_second()).max(0)
}

pub fn fmt_age(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

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

    #[test]
    fn a_clock_skew_never_reads_as_a_negative_age() {
        let store = tmp_store("skew");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        let before_it_was_queued = pause.queued_at.as_second() - 300;
        assert_eq!(age_s(&pause, before_it_was_queued), 0);
        assert_eq!(row(&pause, before_it_was_queued).age, "0s");
    }

    #[test]
    fn every_stored_state_reads_as_its_one_word() {
        let store = tmp_store("states");
        let mut intent = store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        assert_eq!(state_word(&intent), "pending");
        intent.state = IntentState::Diverged {
            status: 409,
            title: "Conflict".into(),
            detail: String::new(),
            type_uri: None,
            errors: vec![],
            code: None,
            conflict: Default::default(),
        };
        assert_eq!(state_word(&intent), "diverged");
        intent.state = IntentState::Parked {
            reason: "skipped · Conflict".into(),
        };
        assert_eq!(state_word(&intent), "parked");
    }
}
