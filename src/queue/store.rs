//! The persisted queue document and its concurrency rules.
//!
//! One JSON document (`queue.json` in the XDG state dir, sibling to
//! `timer-cache.json`), rewritten whole through write-temp + fsync + atomic
//! rename — the queue is designed to stay small and short-lived, so a full
//! rewrite beats an append log that would need compaction and torn-record
//! handling.
//!
//! Concurrency: several processes share one queue (the TUI, a status-bar
//! poller, ad-hoc one-shots). Writers serialize on an exclusive advisory lock
//! held on a sidecar `queue.lock` — never on the data file itself, because the
//! atomic rename swaps the inode out from under any lock held on it. Readers
//! take no lock: the rename guarantees they always see one consistent
//! document. Replay is single-flight via a non-blocking `try_lock` on a second
//! sidecar, `replay.lock`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

use super::intent::{new_idempotency_key, Intent, IntentKind, IntentState};

/// Errors are loud by design: a silently dropped intent is the one failure the
/// write queue exists to prevent (unlike the best-effort read cache).
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("queue io: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue file is corrupt ({path}): {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("queue state dir unavailable: {0}")]
    NoStateDir(String),
}

const DOC_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct QueueDoc {
    version: u32,
    next_id: u64,
    intents: Vec<Intent>,
}

impl Default for QueueDoc {
    fn default() -> Self {
        Self {
            version: DOC_VERSION,
            next_id: 1,
            intents: Vec::new(),
        }
    }
}

/// What a glance needs to know about the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSummary {
    /// Every stored intent, parked included.
    pub depth: usize,
    pub oldest_age_s: Option<i64>,
    /// Intents waiting to replay.
    pub pending: usize,
    /// Intents waiting on a divergence choice.
    pub diverged: usize,
    /// Intents kept for review by a take-server resolution — never replayed,
    /// never counted as queued writes.
    pub parked: usize,
}

impl QueueSummary {
    /// Intents still in play — pending + diverged. Parked intents are kept for
    /// review only, so every "queued" surface (`↑N`, `queued=N`, drain skips)
    /// counts this, not `depth`.
    pub fn in_play(&self) -> usize {
        self.pending + self.diverged
    }
}

/// Held by the single replaying process; the advisory lock releases on drop.
pub struct ReplayGuard {
    _lock: File,
}

/// Handle on the queue document at one path.
pub struct QueueStore {
    path: PathBuf,
}

impl QueueStore {
    /// The shared queue in the XDG state dir.
    pub fn open_default() -> Result<Self, QueueError> {
        let dir = Config::log_dir().map_err(|e| QueueError::NoStateDir(e.to_string()))?;
        Ok(Self::at(dir.join("queue.json")))
    }

    /// A store at an explicit path (tests).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Append a fresh pending intent and persist it. Returns the stored record.
    pub fn enqueue(&self, kind: IntentKind) -> Result<Intent, QueueError> {
        self.mutate(|doc| {
            let intent = Intent {
                id: doc.allocate_id(),
                idempotency_key: new_idempotency_key(),
                stream: kind.stream(),
                queued_at: jiff::Timestamp::now(),
                kind,
                state: IntentState::Pending,
                attempts: 0,
                last_error: None,
            };
            doc.intents_mut().push(intent.clone());
            intent
        })
    }

    /// Every stored intent, in queue order. Lock-free (see module docs).
    pub fn intents(&self) -> Result<Vec<Intent>, QueueError> {
        Ok(self.load()?.intents)
    }

    /// The pending subset, in replay order.
    pub fn pending(&self) -> Result<Vec<Intent>, QueueError> {
        Ok(self
            .load()?
            .intents
            .into_iter()
            .filter(Intent::is_pending)
            .collect())
    }

    /// Depth, oldest age, and the per-state counts the status surfaces read.
    pub fn summary(&self) -> Result<QueueSummary, QueueError> {
        let intents = self.intents()?;
        let now = jiff::Timestamp::now().as_second();
        Ok(QueueSummary {
            depth: intents.len(),
            oldest_age_s: intents
                .first()
                .map(|i| (now - i.queued_at.as_second()).max(0)),
            pending: intents.iter().filter(|i| i.is_pending()).count(),
            diverged: intents.iter().filter(|i| i.is_diverged()).count(),
            parked: intents.iter().filter(|i| i.is_parked()).count(),
        })
    }

    /// Run a closure over the document under the writer lock, persisting the
    /// result atomically. All mutation goes through here.
    pub fn mutate<R>(&self, f: impl FnOnce(&mut QueueDocView) -> R) -> Result<R, QueueError> {
        let _lock = self.writer_lock()?;
        let mut doc = self.load()?;
        let mut view = QueueDocView { doc: &mut doc };
        let out = f(&mut view);
        self.persist(&doc)?;
        Ok(out)
    }

    /// Claim the single-replayer slot. `None` means another process is already
    /// draining — callers just skip; enqueues during a drain join the tail.
    pub fn try_replay_lock(&self) -> Result<Option<ReplayGuard>, QueueError> {
        let lock = self.open_lock_file(&self.sibling("replay.lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(ReplayGuard { _lock: lock })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    fn load(&self) -> Result<QueueDoc, QueueError> {
        let json = match std::fs::read_to_string(&self.path) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(QueueDoc::default()),
            Err(e) => return Err(e.into()),
        };
        serde_json::from_str(&json).map_err(|source| QueueError::Corrupt {
            path: self.path.clone(),
            source,
        })
    }

    fn persist(&self, doc: &QueueDoc) -> Result<(), QueueError> {
        let json = serde_json::to_string(doc)
            .expect("queue document serialization is infallible for owned data");
        let tmp = self.sibling("queue.json.tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Blocking exclusive lock on the writer sidecar; released on drop.
    fn writer_lock(&self) -> Result<File, QueueError> {
        let lock = self.open_lock_file(&self.sibling("queue.lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    fn open_lock_file(&self, path: &Path) -> Result<File, QueueError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?)
    }

    fn sibling(&self, name: &str) -> PathBuf {
        self.path.with_file_name(name)
    }
}

/// The mutable view `mutate` closures work against — the document internals
/// stay private to this module.
pub struct QueueDocView<'a> {
    doc: &'a mut QueueDoc,
}

impl QueueDocView<'_> {
    /// Claim the next queue sequence number.
    pub fn allocate_id(&mut self) -> u64 {
        let id = self.doc.next_id;
        self.doc.next_id += 1;
        id
    }

    pub fn intents(&self) -> &[Intent] {
        &self.doc.intents
    }

    pub fn intents_mut(&mut self) -> &mut Vec<Intent> {
        &mut self.doc.intents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> QueueStore {
        let dir = std::env::temp_dir().join(format!("engineer-queue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        QueueStore::at(dir.join("queue.json"))
    }

    fn pause_kind() -> IntentKind {
        IntentKind::TimerPause {
            at: "2026-07-15T09:30:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn enqueue_then_read_roundtrips_in_order() {
        let store = tmp_store("roundtrip");
        let a = store.enqueue(pause_kind()).unwrap();
        let b = store
            .enqueue(IntentKind::TimerResume {
                at: "2026-07-15T09:41:00Z".parse().unwrap(),
            })
            .unwrap();
        assert!(a.id < b.id, "ids are monotonic");
        assert_ne!(a.idempotency_key, b.idempotency_key);

        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, a.id, "replay order is enqueue order");
        assert_eq!(pending[0].stream, "timer");
        assert_eq!(pending[1].kind.word(), "resume");
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let store = tmp_store("missing");
        assert!(store.pending().unwrap().is_empty());
        let summary = store.summary().unwrap();
        assert_eq!(summary.depth, 0);
        assert_eq!(summary.oldest_age_s, None);
    }

    #[test]
    fn corrupt_file_is_a_loud_error_not_a_silent_reset() {
        let store = tmp_store("corrupt");
        store.enqueue(pause_kind()).unwrap();
        std::fs::write(store.path.clone(), "{not json").unwrap();

        assert!(matches!(store.pending(), Err(QueueError::Corrupt { .. })));
        // A write over a corrupt document must refuse too — never clobber
        // intents we can no longer read.
        assert!(store.enqueue(pause_kind()).is_err());
    }

    #[test]
    fn summary_counts_depth_age_and_every_state() {
        let store = tmp_store("summary");
        store.enqueue(pause_kind()).unwrap();
        store.enqueue(pause_kind()).unwrap();
        store.enqueue(pause_kind()).unwrap();
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 422,
                    title: "Segment overlaps".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: vec![],
                };
                doc.intents_mut()[1].state = IntentState::Parked {
                    reason: "took server · Segment overlaps".into(),
                };
            })
            .unwrap();

        let summary = store.summary().unwrap();
        assert_eq!(summary.depth, 3, "depth counts parked too");
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.diverged, 1);
        assert_eq!(summary.parked, 1);
        assert_eq!(summary.in_play(), 2, "parked is out of play");
        assert!(summary.oldest_age_s.unwrap() >= 0);
        assert_eq!(store.pending().unwrap().len(), 1, "parked never replays");
    }

    #[test]
    fn concurrent_writers_never_lose_an_intent() {
        let store = tmp_store("concurrent");
        store.enqueue(pause_kind()).unwrap(); // create the file first
        let path = store.path.clone();

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = QueueStore::at(path);
                    for _ in 0..10 {
                        store
                            .enqueue(IntentKind::TimerPause {
                                at: jiff::Timestamp::now(),
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 81, "1 seed + 8 threads × 10 enqueues");
        let mut ids: Vec<u64> = intents.iter().map(|i| i.id).collect();
        let unique_before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), unique_before, "no id was assigned twice");
    }

    #[test]
    fn rename_leaves_no_temp_file_behind() {
        let store = tmp_store("tmpfile");
        store.enqueue(pause_kind()).unwrap();
        assert!(!store.sibling("queue.json.tmp").exists());
        assert!(store.path.exists());
    }

    #[test]
    fn replay_lock_is_single_flight() {
        let store = tmp_store("replaylock");
        let first = store.try_replay_lock().unwrap();
        assert!(first.is_some(), "free lock is claimed");
        assert!(
            store.try_replay_lock().unwrap().is_none(),
            "held lock is skipped, not waited on"
        );
        drop(first);
        assert!(
            store.try_replay_lock().unwrap().is_some(),
            "released lock is claimable again"
        );
    }
}
