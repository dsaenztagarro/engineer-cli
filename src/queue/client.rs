//! The queue-aware write seam: live when the wire is up, a persisted intent
//! when it is not.
//!
//! `QueuedClient` wraps the typed `ApiClient` verbs one by one. Each wrapped
//! verb tries the live call first; on `ApiError::Transport` — the same seam
//! the read cache falls back on — it enqueues the intent (never losing the
//! gesture) and returns a synthesized response computed by the pure
//! transitions in `crate::timer_clock`, seeded from the last known server
//! snapshot. Callers match on [`WriteOutcome`] to render confirmed vs
//! provisional. Every other error keeps live semantics and propagates.

use std::path::PathBuf;

use crate::api::{ApiClient, ApiError, Timer, TimerStopped};
use crate::timer_cache;
use crate::timer_clock;

use super::fold::{self, Provenance};
use super::intent::IntentKind;
use super::replay::{self, ReplayError, ReplayReport};
use super::store::{QueueStore, QueueSummary};

/// A stand-in `segment_id` for a stop that landed in the queue: the real id is
/// server-minted on replay, so the provisional confirmation carries a negative
/// sentinel and the caller renders "queued" instead of `segment #N`.
pub const PROVISIONAL_SEGMENT_ID: i64 = -1;

/// How a write landed: on the server, or into the queue with a locally
/// synthesized stand-in the caller renders as provisional.
#[derive(Debug)]
pub enum WriteOutcome<T> {
    Confirmed(T),
    Provisional(T),
}

impl<T> WriteOutcome<T> {
    pub fn value(&self) -> &T {
        match self {
            Self::Confirmed(v) | Self::Provisional(v) => v,
        }
    }

    /// Consume the outcome for the wrapped value, dropping the confirmed/queued
    /// distinction — callers that carried it in a side channel (a negative
    /// segment id, the screen's provisional flag) reach for this.
    pub fn into_value(self) -> T {
        match self {
            Self::Confirmed(v) | Self::Provisional(v) => v,
        }
    }

    pub fn is_provisional(&self) -> bool {
        matches!(self, Self::Provisional(_))
    }
}

/// Owns a cloned `ApiClient` (it derives `Clone`) rather than borrowing one, so
/// a `QueuedClient` is `'static` and can be built fresh inside each spawned TUI
/// task from that task's own api clone — no lifetime to thread through the event
/// loop. `ApiClient` is a thin `reqwest::Client` handle, so the clone is cheap.
pub struct QueuedClient {
    api: ApiClient,
    store: QueueStore,
    /// Read-cache override for tests; `None` reads the shared XDG location.
    cache_path: Option<PathBuf>,
}

impl QueuedClient {
    /// The shared queue + read cache in the XDG state dir.
    pub fn new(api: &ApiClient) -> Result<Self, super::QueueError> {
        Ok(Self {
            api: api.clone(),
            store: QueueStore::open_default()?,
            cache_path: None,
        })
    }

    /// Explicit store + cache paths (tests).
    pub fn with_paths(api: &ApiClient, store: QueueStore, cache_path: PathBuf) -> Self {
        Self {
            api: api.clone(),
            store,
            cache_path: Some(cache_path),
        }
    }

    /// Depth / age / diverged for the status surfaces. Best-effort on the read
    /// side: an unreadable queue reads as empty here (enqueue stays loud).
    pub fn queue_summary(&self) -> QueueSummary {
        self.store.summary().unwrap_or_else(|e| {
            tracing::warn!(target: "engineer_cli::queue", error = %e, "queue summary unavailable");
            QueueSummary {
                depth: 0,
                oldest_age_s: None,
                diverged: 0,
            }
        })
    }

    /// The effective local timer: the cached server snapshot with the pending
    /// queue folded over it (`fold_timer`), composed fresh on every call — the
    /// queue and the cache are both re-read, so a drained or dropped intent
    /// disappears from the picture on the very next read. `None` when there is
    /// nothing to compose (no snapshot, nothing queued that starts one).
    /// Read-only: the fold is never written back into the cache.
    pub fn effective_timer(&self, now: jiff::Timestamp) -> Option<(Timer, Provenance)> {
        let cached = self.cached_timer();
        // Best-effort like `queue_summary`: an unreadable queue folds as empty
        // (enqueue stays loud).
        let intents = self.store.intents().unwrap_or_else(|e| {
            tracing::warn!(target: "engineer_cli::queue", error = %e, "queue unreadable for the fold");
            Vec::new()
        });
        fold::fold_timer(cached.as_ref(), &intents, now)
    }

    /// Run a full replay pass now; the caller renders the report.
    pub async fn drain(&self) -> Result<ReplayReport, ReplayError> {
        replay::drain(&self.api, &self.store).await
    }

    /// The cheap drain the automatic triggers fire — before a live write and
    /// after a successful one-shot read. Skips instantly when the queue is
    /// empty (a summary depth check before taking any lock) and swallows
    /// failures with a log line: the caller's own call carries the
    /// user-facing error, and a divergence keeps surfacing through the
    /// `queued`/`diverged` read fields until `engineer queue` resolves it.
    pub async fn drain_best_effort(&self) {
        if self.queue_summary().depth == 0 {
            return;
        }
        if let Err(e) = self.drain().await {
            tracing::warn!(target: "engineer_cli::queue", error = %e, "queue drain failed");
        }
    }

    pub async fn pause_timer(&self) -> Result<WriteOutcome<Timer>, ApiError> {
        // Drain-before-live-write: a live write never jumps the queue. If the
        // drain hits Transport, this verb's own live attempt fails the same
        // way and the fresh intent enqueues *behind* the replaying ones.
        self.drain_best_effort().await;
        match self.api.pause_timer().await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(msg)) => {
                let at = jiff::Timestamp::now();
                self.defer(IntentKind::TimerPause { at }, msg, |snap| {
                    timer_clock::apply_pause(snap, at)
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn resume_timer(&self) -> Result<WriteOutcome<Timer>, ApiError> {
        // Same drain-before-live-write contract as `pause_timer`.
        self.drain_best_effort().await;
        match self.api.resume_timer().await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(msg)) => {
                let at = jiff::Timestamp::now();
                self.defer(IntentKind::TimerResume { at }, msg, |snap| {
                    timer_clock::apply_resume(snap, at)
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Start a clock. Unlike every other verb, an offline start with *no* cached
    /// timer is legitimate — nothing is running, so there is nothing missing to
    /// act on. Rather than propagating the transport error like `defer` does, it
    /// synthesizes a fresh anchored clock (`apply_start`) and enqueues the intent
    /// (decision on #103). `switch` rides the intent so the replay stops & saves
    /// whatever the server has running first, exactly as a live start would.
    pub async fn start_timer(
        &self,
        activity_id: Option<i64>,
        switch: bool,
    ) -> Result<WriteOutcome<Timer>, ApiError> {
        self.drain_best_effort().await;
        match self.api.start_timer(activity_id, switch).await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(msg)) => {
                let at = jiff::Timestamp::now();
                self.store
                    .enqueue(IntentKind::TimerStart {
                        activity_id,
                        switch,
                        at,
                    })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                let _ = msg;
                Ok(WriteOutcome::Provisional(timer_clock::apply_start(
                    activity_id,
                    None,
                    at,
                )))
            }
            Err(e) => Err(e),
        }
    }

    /// Stop & save. Returns the same `TimerStopped` shape a live stop does; the
    /// offline stand-in freezes the local clock (`apply_stop` → `LocalStop`),
    /// enqueues the intent carrying the local elapsed the reconcile compares
    /// against, and reports a [`PROVISIONAL_SEGMENT_ID`] the caller renders as
    /// "queued". No snapshot → propagate, like every non-start verb.
    pub async fn stop_timer(&self) -> Result<WriteOutcome<TimerStopped>, ApiError> {
        self.drain_best_effort().await;
        match self.api.stop_timer().await {
            Ok(stopped) => Ok(WriteOutcome::Confirmed(stopped)),
            Err(ApiError::Transport(msg)) => {
                let Some(snapshot) = self.load_snapshot() else {
                    return Err(ApiError::Transport(msg));
                };
                let at = jiff::Timestamp::now();
                let (_, local) = timer_clock::apply_stop(snapshot, at);
                self.store
                    .enqueue(IntentKind::TimerStop {
                        at,
                        local_elapsed_s: local.elapsed_seconds,
                    })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                Ok(WriteOutcome::Provisional(TimerStopped {
                    stopped: true,
                    activity_id: local.activity_id.unwrap_or(0),
                    segment_id: PROVISIONAL_SEGMENT_ID,
                    minutes: local.minutes,
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Name the running timer. Offline, field-flips the snapshot bound (the same
    /// transition the fold applies), so the provisional face reads as bound.
    pub async fn bind_timer(
        &self,
        activity_id: Option<i64>,
        title: Option<String>,
    ) -> Result<WriteOutcome<Timer>, ApiError> {
        self.drain_best_effort().await;
        match self.api.bind_timer(activity_id, title.clone()).await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(msg)) => {
                let flip_title = title.clone();
                self.defer(
                    IntentKind::TimerBind { activity_id, title },
                    msg,
                    move |mut t| {
                        t.bound = true;
                        t.activity_id = activity_id;
                        if flip_title.is_some() {
                            t.label = flip_title;
                        }
                        t
                    },
                )
            }
            Err(e) => Err(e),
        }
    }

    /// Throw the timer away, writing nothing. Offline, the stand-in is the blank
    /// "nothing running" clock — what a discard leaves behind.
    pub async fn discard_timer(&self) -> Result<WriteOutcome<Timer>, ApiError> {
        self.drain_best_effort().await;
        match self.api.discard_timer().await {
            Ok(()) => Ok(WriteOutcome::Confirmed(Timer::default())),
            Err(ApiError::Transport(msg)) => {
                self.defer(IntentKind::TimerDiscard, msg, |_| Timer::default())
            }
            Err(e) => Err(e),
        }
    }

    /// The offline arm shared by every wrapped verb: enqueue first (the
    /// gesture must never be lost), then synthesize from the last known
    /// snapshot. With no snapshot there is nothing locally known to act on —
    /// the transport error propagates, exactly like the read path.
    fn defer(
        &self,
        kind: IntentKind,
        transport_msg: String,
        synthesize: impl FnOnce(Timer) -> Timer,
    ) -> Result<WriteOutcome<Timer>, ApiError> {
        let Some(snapshot) = self.load_snapshot() else {
            return Err(ApiError::Transport(transport_msg));
        };
        self.store.enqueue(kind).map_err(|e| {
            ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
        })?;
        Ok(WriteOutcome::Provisional(synthesize(snapshot)))
    }

    fn load_snapshot(&self) -> Option<Timer> {
        Some(self.cached_timer()?.timer)
    }

    /// The last-known server snapshot and its age, from the read cache.
    fn cached_timer(&self) -> Option<timer_cache::StaleTimer> {
        match &self.cache_path {
            None => timer_cache::load(),
            Some(path) => timer_cache::load_at(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("engineer-qc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seeded_cache(dir: &std::path::Path) -> PathBuf {
        let cache = dir.join("timer-cache.json");
        let timer: Timer = serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "activity_id": 9, "label": "systems",
            "elapsed_seconds": 1800, "paused_seconds": 0
        }))
        .unwrap();
        timer_cache::store_at(&cache, &timer);
        cache
    }

    /// A base URL nothing listens on — reqwest fails before any response,
    /// which is exactly `ApiError::Transport`.
    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    #[tokio::test]
    async fn live_pause_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": true, "elapsed_seconds": 1801
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-pause");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            seeded_cache(&dir),
        );

        let out = queued.pause_timer().await.unwrap();
        assert!(!out.is_provisional());
        assert!(out.value().paused);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_pause_enqueues_and_synthesizes() {
        let api = dead_api();
        let dir = tmp_dir("offline-pause");
        let store = QueueStore::at(dir.join("queue.json"));
        let queued = QueuedClient::with_paths(&api, store, seeded_cache(&dir));

        let out = queued.pause_timer().await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().paused, "synthesized timer is paused");
        assert!(out.value().paused_at.is_some());

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "pause");
    }

    #[tokio::test]
    async fn offline_resume_folds_the_paused_span() {
        let api = dead_api();
        let dir = tmp_dir("offline-resume");
        let cache = dir.join("timer-cache.json");
        let paused: Timer = serde_json::from_value(serde_json::json!({
            "running": true, "paused": true, "elapsed_seconds": 900,
            "paused_seconds": 30, "paused_at": jiff::Timestamp::now().to_string()
        }))
        .unwrap();
        timer_cache::store_at(&cache, &paused);
        let queued = QueuedClient::with_paths(&api, QueueStore::at(dir.join("queue.json")), cache);

        let out = queued.resume_timer().await.unwrap();
        assert!(out.is_provisional());
        assert!(!out.value().paused);
        assert!(out.value().paused_seconds.unwrap() >= 30);
    }

    #[tokio::test]
    async fn offline_with_no_snapshot_propagates_transport() {
        let api = dead_api();
        let dir = tmp_dir("no-snapshot");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        assert!(matches!(
            queued.pause_timer().await,
            Err(ApiError::Transport(_))
        ));
        assert_eq!(queued.queue_summary().depth, 0, "nothing enqueued blind");
    }

    #[tokio::test]
    async fn a_live_write_drains_the_queue_first() {
        let server = MockServer::start().await;
        // One replayed pause (with the stored key) + the live pause = 2 hits.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": true, "elapsed_seconds": 1801
            })))
            .expect(2)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("drain-first");
        let store = QueueStore::at(dir.join("queue.json"));
        store
            .enqueue(crate::queue::IntentKind::TimerPause {
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        let queued = QueuedClient::with_paths(&api, store, seeded_cache(&dir));

        let out = queued.pause_timer().await.unwrap();
        assert!(!out.is_provisional(), "the live write went live");
        assert_eq!(queued.queue_summary().depth, 0, "the backlog drained first");
    }

    #[tokio::test]
    async fn offline_drain_leaves_the_fresh_write_queued_behind() {
        let api = dead_api();
        let dir = tmp_dir("enqueue-behind");
        let store = QueueStore::at(dir.join("queue.json"));
        let first = store
            .enqueue(crate::queue::IntentKind::TimerPause {
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        let queued = QueuedClient::with_paths(&api, store, seeded_cache(&dir));

        let out = queued.resume_timer().await.unwrap();
        assert!(out.is_provisional());

        let intents = QueueStore::at(dir.join("queue.json")).intents().unwrap();
        assert_eq!(intents.len(), 2, "the fresh write joined the tail");
        assert_eq!(intents[0].id, first.id, "order preserved");
        assert_eq!(intents[0].attempts, 1, "the drain tried the head first");
        assert_eq!(intents[1].kind.word(), "resume");
        assert_eq!(intents[1].attempts, 0);
    }

    #[tokio::test]
    async fn auth_errors_keep_live_semantics() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("auth");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            seeded_cache(&dir),
        );

        assert!(matches!(
            queued.pause_timer().await,
            Err(ApiError::Unauthorized)
        ));
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_start_with_no_snapshot_still_enqueues() {
        // The start exception: nothing cached is legitimate — nothing was
        // running — so an offline start synthesizes a fresh clock and queues,
        // rather than propagating the transport error the other verbs would.
        let api = dead_api();
        let dir = tmp_dir("offline-start");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        let out = queued.start_timer(Some(9), false).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().running, "synthesized clock is running");
        assert_eq!(out.value().activity_id, Some(9));
        assert!(out.value().bound, "a bound start is bound");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "start");
    }

    #[tokio::test]
    async fn offline_stop_synthesizes_the_segment_confirmation() {
        let api = dead_api();
        let dir = tmp_dir("offline-stop");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            seeded_cache(&dir),
        );

        let out = queued.stop_timer().await.unwrap();
        assert!(out.is_provisional());
        let stopped = out.value();
        assert!(stopped.stopped);
        assert_eq!(stopped.activity_id, 9, "from the cached bound timer");
        assert!(
            stopped.segment_id < 0,
            "a queued stop has no server segment id yet"
        );

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "stop");
    }

    #[tokio::test]
    async fn offline_stop_with_no_snapshot_propagates_transport() {
        let api = dead_api();
        let dir = tmp_dir("stop-no-snapshot");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );
        assert!(matches!(
            queued.stop_timer().await,
            Err(ApiError::Transport(_))
        ));
        assert_eq!(queued.queue_summary().depth, 0, "nothing enqueued blind");
    }

    #[tokio::test]
    async fn offline_bind_flips_the_snapshot_bound() {
        let api = dead_api();
        let dir = tmp_dir("offline-bind");
        // A running *unbound* cache so the flip is observable.
        let cache = dir.join("timer-cache.json");
        let unbound: Timer = serde_json::from_value(serde_json::json!({
            "running": true, "bound": false, "elapsed_seconds": 300
        }))
        .unwrap();
        timer_cache::store_at(&cache, &unbound);
        let queued = QueuedClient::with_paths(&api, QueueStore::at(dir.join("queue.json")), cache);

        let out = queued
            .bind_timer(None, Some("Implement Raft".into()))
            .await
            .unwrap();
        assert!(out.is_provisional());
        assert!(out.value().bound);
        assert_eq!(out.value().label.as_deref(), Some("Implement Raft"));

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "bind");
    }

    #[tokio::test]
    async fn offline_discard_leaves_nothing_running() {
        let api = dead_api();
        let dir = tmp_dir("offline-discard");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            seeded_cache(&dir),
        );

        let out = queued.discard_timer().await.unwrap();
        assert!(out.is_provisional());
        assert!(!out.value().running, "a discard leaves nothing running");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "discard");
    }
}
