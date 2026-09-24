//! The last-known timer cache — the offline read fallback (ADR 0004, the read half).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::Timer;
use crate::config::Config;

#[derive(Serialize, Deserialize)]
struct Cached {
    /// Unix seconds when the snapshot was taken.
    cached_at: i64,
    timer: Timer,
}

pub struct StaleTimer {
    pub timer: Timer,
    pub age_secs: i64,
}

fn path() -> Option<PathBuf> {
    Config::log_dir()
        .ok()
        .map(|dir| dir.join("timer-cache.json"))
}

pub fn store(timer: &Timer) {
    if let Some(path) = path() {
        store_at(&path, timer);
    }
}

pub fn load() -> Option<StaleTimer> {
    load_at(&path()?)
}

pub(crate) fn store_at(path: &Path, timer: &Timer) {
    let cached = Cached {
        cached_at: jiff::Timestamp::now().as_second(),
        timer: timer.clone(),
    };
    let Ok(json) = serde_json::to_string(&cached) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, json);
}

pub(crate) fn load_at(path: &Path) -> Option<StaleTimer> {
    let json = std::fs::read_to_string(path).ok()?;
    let cached: Cached = serde_json::from_str(&json).ok()?;
    let age = (jiff::Timestamp::now().as_second() - cached.cached_at).max(0);
    Some(StaleTimer {
        timer: cached.timer,
        age_secs: age,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_timer() -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "id": 9, "activity_id": 3, "label": "systems",
            "elapsed_seconds": 1453, "mode": "stopwatch"
        }))
        .unwrap()
    }

    #[test]
    fn store_then_load_roundtrips_the_timer() {
        let path =
            std::env::temp_dir().join(format!("engineer-timer-cache-{}.json", std::process::id()));
        store_at(&path, &running_timer());
        let loaded = load_at(&path).expect("a cached timer");
        let _ = std::fs::remove_file(&path);
        assert!(loaded.timer.running);
        assert_eq!(loaded.timer.label.as_deref(), Some("systems"));
        assert_eq!(loaded.timer.elapsed_seconds, Some(1453));
        assert!(loaded.age_secs >= 0);
    }

    #[test]
    fn a_failed_cache_write_is_silent() {
        let blocker =
            std::env::temp_dir().join(format!("engineer-timer-cache-file-{}", std::process::id()));
        std::fs::write(&blocker, "a file, not a directory").unwrap();
        store_at(&blocker.join("timer-cache.json"), &running_timer());
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn load_missing_file_is_none() {
        let path = std::env::temp_dir().join("engineer-timer-cache-does-not-exist.json");
        let _ = std::fs::remove_file(&path);
        assert!(load_at(&path).is_none());
    }
}
