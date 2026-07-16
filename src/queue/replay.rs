//! The replay pass — pending intents re-send in order when the wire returns
//! (offline-write.brief.md §8 Foundation, "a replay-on-reconnect pass").
//!
//! Strict FIFO by intent `id`, one intent at a time, single-flight across
//! processes via the `replay.lock` sidecar. The server stays authoritative:
//! a replayed intent leaves the queue the moment the server acknowledges it,
//! and the pass halts the instant the server *disagrees* — a divergence needs
//! a human choice before anything later replays, or ordering would lie.

use crate::api::{ActivityUpdate, ApiClient, ApiError};

use super::intent::{Intent, IntentKind, IntentState};
use super::store::{QueueError, QueueStore};

/// What one drain accomplished — the callers render this, so both the
/// `engineer queue sync` line and the TUI tile speak from the same numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayReport {
    /// Intents the server acknowledged; they have left the queue.
    pub replayed: usize,
    /// Of the replayed, how many were answered from the server's idempotency
    /// store (`Idempotency-Replayed: true`) — the first attempt landed and
    /// only the ack was lost. Counted in `replayed` too: a deduped intent is
    /// consumed as confirmed, silently, exactly like a first execution.
    pub deduped: usize,
    /// Intents still in play after the pass (pending + diverged). Parked
    /// intents are kept for review, not waiting to sync, so they never count.
    pub remaining: usize,
    /// A divergence is waiting on a human choice.
    pub diverged: bool,
}

/// A drain that could not even run its per-intent protocol. `Transport` and
/// `Problem` are *handled* inside the pass (halt / diverge); only the
/// unclassifiable rest surfaces here — queue io, auth, decode.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Api(#[from] ApiError),
}

/// Drain the queue: replay pending intents oldest-first until they are gone,
/// the wire drops, or the server diverges.
///
/// Single-flight: if another process holds the replay lock this returns
/// immediately with a report of the queue as it stands — callers skip
/// silently, they never wait. An already-diverged intent gates the whole
/// pass the same way a fresh divergence halts it: everything queued behind
/// the choice stays queued.
pub async fn drain(api: &ApiClient, store: &QueueStore) -> Result<ReplayReport, ReplayError> {
    drain_reporting(api, store, |_| {}).await
}

/// [`drain`], but reporting each acknowledged intent to `on_replay` as it lands
/// — the TUI streams these into its reconnect transcript (`back online ·
/// replaying the queue…`). The callback fires only for intents the server
/// *acknowledges*, so a pass that replays nothing (still offline, a held replay
/// lock, an already-parked divergence) never calls it: the transcript can't lie.
pub async fn drain_reporting(
    api: &ApiClient,
    store: &QueueStore,
    mut on_replay: impl FnMut(&Intent),
) -> Result<ReplayReport, ReplayError> {
    let Some(_guard) = store.try_replay_lock()? else {
        return report(store, 0, 0);
    };

    if store.summary()?.diverged > 0 {
        return report(store, 0, 0);
    }

    let mut pending = store.pending()?;
    pending.sort_by_key(|i| i.id);
    let mut replayed = 0usize;
    let mut deduped = 0usize;

    for intent in pending {
        match send_intent(api, &intent).await {
            Ok(from_store) => {
                // Acknowledged — the server is authoritative now; the intent
                // leaves the queue (under the writer lock, like all mutation).
                // A stored replay (the first attempt landed, the ack was lost)
                // is consumed the same way, silently: the server already
                // deduped it, so there is nothing to ask the user (engineer#806).
                if from_store {
                    deduped += 1;
                    tracing::info!(
                        target: "engineer_cli::queue",
                        intent = intent.id,
                        verb = intent.kind.word(),
                        "stored response replayed — the first attempt had landed; consumed as confirmed"
                    );
                }
                store.mutate(|doc| doc.intents_mut().retain(|i| i.id != intent.id))?;
                replayed += 1;
                on_replay(&intent);
            }
            Err(ApiError::Transport(msg)) => {
                // The wire dropped again. Everything stays pending; only the
                // intent that hit the wall records the attempt.
                store.mutate(|doc| {
                    if let Some(i) = doc.intents_mut().iter_mut().find(|i| i.id == intent.id) {
                        i.attempts += 1;
                        i.last_error = Some(msg);
                    }
                })?;
                break;
            }
            Err(ApiError::Problem {
                status,
                title,
                detail,
                type_uri,
                errors,
                code,
                conflict,
            }) => {
                // The server moved on — persist its objection verbatim (the
                // coded conflict's `code` + extensions included, so the
                // reconcile surfaces can render the server's side) and halt:
                // nothing later replays past an unresolved divergence.
                store.mutate(|doc| {
                    if let Some(i) = doc.intents_mut().iter_mut().find(|i| i.id == intent.id) {
                        i.attempts += 1;
                        i.state = IntentState::Diverged {
                            status,
                            title,
                            detail,
                            type_uri,
                            errors,
                            code,
                            conflict,
                        };
                    }
                })?;
                break;
            }
            // Auth / decode — neither "offline" nor "the server said no";
            // the caller decides (exit 5 in `engineer queue`).
            Err(e) => return Err(e.into()),
        }
    }

    report(store, replayed, deduped)
}

/// Re-send one intent through the typed call for its kind, carrying the
/// stored `Idempotency-Key` so a lost ack can never double-write. `Ok(true)`
/// means the answer was a stored replay (`Idempotency-Replayed: true`) — the
/// first attempt had landed.
async fn send_intent(api: &ApiClient, intent: &Intent) -> Result<bool, ApiError> {
    let key = &intent.idempotency_key;
    match &intent.kind {
        IntentKind::TimerStart {
            activity_id,
            switch,
            ..
        } => api
            .start_timer_idempotent(*activity_id, *switch, key)
            .await
            .map(|k| k.replayed),
        IntentKind::TimerPause { .. } => api.pause_timer_idempotent(key).await.map(|k| k.replayed),
        IntentKind::TimerResume { .. } => {
            api.resume_timer_idempotent(key).await.map(|k| k.replayed)
        }
        IntentKind::TimerStop { .. } => api.stop_timer_idempotent(key).await.map(|k| k.replayed),
        IntentKind::TimerBind { activity_id, title } => api
            .bind_timer_idempotent(*activity_id, title.clone(), key)
            .await
            .map(|k| k.replayed),
        // DELETE needs no idempotent variant: deleting the singleton twice is
        // naturally idempotent — the second delete finds nothing to write.
        IntentKind::TimerDiscard => api.discard_timer().await.map(|()| false),
        // A declare re-sends the whole create body under the stored key, so a
        // lost ack can never mint the plan item twice (the server's
        // Idempotency-Key contract, like the timer starts).
        IntentKind::ActivityCreate { body } => api
            .create_activity_idempotent(body, key)
            .await
            .map(|k| k.replayed),
        // Adjust/drop replay as plain calls: re-sending the same title or a
        // second archive is naturally idempotent server-side, so they need no
        // key. A stored replay is indistinguishable from a first ack here, so
        // report `false` (never a dedupe).
        IntentKind::ActivityUpdate { id, title } => api
            .update_activity(
                *id,
                &ActivityUpdate {
                    title: Some(title.clone()),
                },
            )
            .await
            .map(|_| false),
        IntentKind::ActivityArchive { id } => api.archive_activity(*id).await.map(|_| false),
    }
}

fn report(
    store: &QueueStore,
    replayed: usize,
    deduped: usize,
) -> Result<ReplayReport, ReplayError> {
    let summary = store.summary()?;
    Ok(ReplayReport {
        replayed,
        deduped,
        remaining: summary.in_play(),
        diverged: summary.diverged > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use url::Url;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn tmp_store(tag: &str) -> QueueStore {
        let dir =
            std::env::temp_dir().join(format!("engineer-replay-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        QueueStore::at(dir.join("queue.json"))
    }

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    fn at() -> jiff::Timestamp {
        "2026-07-15T09:30:00Z".parse().unwrap()
    }

    /// Records `(path, Idempotency-Key)` per request so the tests can assert
    /// FIFO order *on the wire*, not just in the store.
    struct Recorder {
        log: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Respond for Recorder {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let key = req
                .headers
                .get("Idempotency-Key")
                .map(|v| v.to_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            self.log
                .lock()
                .unwrap()
                .push((req.url.path().to_string(), key));
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true }))
        }
    }

    #[tokio::test]
    async fn drains_fifo_with_the_stored_idempotency_keys_on_the_wire() {
        let server = MockServer::start().await;
        let log = Arc::new(Mutex::new(Vec::new()));
        Mock::given(method("POST"))
            .respond_with(Recorder { log: log.clone() })
            .expect(3)
            .mount(&server)
            .await;

        let store = tmp_store("fifo");
        let a = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        let b = store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();
        let c = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 3);
        assert_eq!(report.remaining, 0);
        assert!(!report.diverged);
        assert!(store.intents().unwrap().is_empty(), "synced intents leave");

        let calls = log.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                ("/api/v1/timer/pause".into(), a.idempotency_key),
                ("/api/v1/timer/resume".into(), b.idempotency_key),
                ("/api/v1/timer/pause".into(), c.idempotency_key),
            ],
            "wire order is queue order, each carrying its own stored key"
        );
    }

    #[tokio::test]
    async fn drain_reporting_streams_each_verb_word_in_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(3)
            .mount(&server)
            .await;

        let store = tmp_store("reporting");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();

        let mut words = Vec::new();
        let report = drain_reporting(&client(&server), &store, |i| words.push(i.kind.word()))
            .await
            .unwrap();
        assert_eq!(report.replayed, 3);
        assert_eq!(
            words,
            vec!["pause", "resume", "pause"],
            "one word per ack, in FIFO order"
        );
    }

    #[tokio::test]
    async fn drain_reporting_never_fires_when_the_lock_is_held() {
        let store = tmp_store("reporting-held");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        let _guard = store.try_replay_lock().unwrap().expect("free lock");

        let mut fired = false;
        let report = drain_reporting(&dead_api(), &store, |_| fired = true)
            .await
            .unwrap();
        assert_eq!(report.replayed, 0);
        assert!(!fired, "a skipped pass streams no transcript");
    }

    #[tokio::test]
    async fn every_kind_maps_to_its_endpoint() {
        let server = MockServer::start().await;
        let timer_body = ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({ "running": true, "bound": true }));
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(
                serde_json::json!({ "activity_id": 9, "switch": true }),
            ))
            .respond_with(timer_body.clone())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/bind"))
            .and(body_json(serde_json::json!({ "activity_id": 9 })))
            .respond_with(timer_body)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/stop"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "stopped": true, "activity_id": 9, "segment_id": 41, "minutes": 25
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("kinds");
        store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: true,
                at: at(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerBind {
                activity_id: Some(9),
                title: None,
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerStop {
                at: at(),
                local_elapsed_s: 1500,
            })
            .unwrap();
        store.enqueue(IntentKind::TimerDiscard).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 4);
        assert_eq!(report.remaining, 0);
    }

    #[tokio::test]
    async fn replayed_writes_carry_an_idempotency_key_header() {
        let server = MockServer::start().await;
        let store = tmp_store("header");
        let intent = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .and(header("Idempotency-Key", intent.idempotency_key.as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
    }

    #[tokio::test]
    async fn problem_halts_the_drain_and_persists_diverged() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/overlap",
                "title": "Segment overlaps",
                "status": 422,
                "detail": "another session already covers 09:00–09:30",
                "errors": [{ "field": "started_at", "detail": "overlaps an existing segment" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Nothing after the divergence may replay.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/resume"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("problem");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(report.remaining, 2);
        assert!(report.diverged);

        let intents = store.intents().unwrap();
        match &intents[0].state {
            IntentState::Diverged {
                status,
                title,
                detail,
                type_uri,
                errors,
                code,
                conflict,
            } => {
                assert_eq!(*status, 422);
                assert_eq!(title, "Segment overlaps");
                assert!(detail.contains("09:00"));
                assert!(type_uri.as_deref().unwrap().contains("overlap"));
                assert_eq!(errors.len(), 1, "the full RFC 7807 payload is kept");
                assert!(code.is_none(), "a code-less problem stays code-less");
                assert!(conflict.is_empty());
            }
            other => panic!("expected the rejected intent to be diverged, got {other:?}"),
        }
        assert!(intents[1].is_pending(), "the rest stays pending, untouched");
    }

    #[tokio::test]
    async fn a_coded_conflict_persists_its_code_and_extensions_on_the_diverged_intent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/timer-already-running",
                "title": "Timer already running",
                "status": 409,
                "detail": "Stop the running timer first, or pass switch=true to stop-and-switch.",
                "code": "timer-already-running",
                "current": {
                    "id": 114, "activity_id": 258777238, "label": "Ruby OOP Study",
                    "started_at": "2026-07-16T08:59:03.246Z", "paused": false
                },
                "resolutions": ["switch", "keep-remote"]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("coded-conflict");
        store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: at(),
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert!(report.diverged);

        let intents = store.intents().unwrap();
        match &intents[0].state {
            IntentState::Diverged { code, conflict, .. } => {
                assert_eq!(code.as_deref(), Some("timer-already-running"));
                let current = conflict.current.as_ref().expect("the server session");
                assert_eq!(current.label.as_deref(), Some("Ruby OOP Study"));
                assert_eq!(current.activity_id, Some(258777238));
                assert_eq!(
                    conflict.resolutions,
                    vec!["switch", "keep-remote"],
                    "the resolution hints ride along"
                );
            }
            other => panic!("expected diverged, got {other:?}"),
        }
    }

    /// §Diverged · duplicate (offline-write.brief.md): the same intent re-sent
    /// after a lost ack. The server answers from its idempotency store —
    /// byte-identical body, `Idempotency-Replayed: true` — and the intent is
    /// consumed as confirmed, silently: it leaves the queue, no divergence, no
    /// prompt, and exactly one logical write ever existed server-side.
    #[tokio::test]
    async fn a_lost_ack_replay_dedupes_silently_with_no_duplicate_prompt() {
        let store = tmp_store("dedupe");
        let intent = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();

        // First attempt: the write lands server-side but the ack is lost —
        // from the queue's view this is a transport failure, so the intent
        // stays pending with one recorded attempt.
        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(store.intents().unwrap()[0].attempts, 1, "the lost ack");

        // Reconnect: the re-sent intent carries the same stored key, and the
        // server replays the stored first response instead of re-executing.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .and(header("Idempotency-Key", intent.idempotency_key.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Idempotency-Replayed", "true")
                    .set_body_json(serde_json::json!({ "running": true, "paused": true })),
            )
            .expect(1) // exactly one wire call — and it wrote nothing new
            .mount(&server)
            .await;

        let mut words = Vec::new();
        let report = drain_reporting(&client(&server), &store, |i| words.push(i.kind.word()))
            .await
            .unwrap();
        assert_eq!(report.replayed, 1, "consumed as confirmed");
        assert_eq!(report.deduped, 1, "…and known to be the stored replay");
        assert_eq!(report.remaining, 0);
        assert!(!report.diverged, "a dedupe is not a divergence — no prompt");
        assert!(
            store.intents().unwrap().is_empty(),
            "the intent left the queue"
        );
        assert_eq!(
            words,
            vec!["pause"],
            "the transcript reads like any confirmed replay — silent dedupe"
        );
    }

    #[tokio::test]
    async fn transport_halts_the_drain_and_counts_the_attempt() {
        let store = tmp_store("transport");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();

        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(report.remaining, 2);
        assert!(!report.diverged, "offline is not a divergence");

        let intents = store.intents().unwrap();
        assert!(intents.iter().all(Intent::is_pending));
        assert_eq!(intents[0].attempts, 1);
        assert!(intents[0].last_error.is_some());
        assert_eq!(intents[1].attempts, 0, "never reached — the drain stopped");
    }

    #[tokio::test]
    async fn a_held_replay_lock_skips_silently() {
        let store = tmp_store("held-lock");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        let _guard = store.try_replay_lock().unwrap().expect("free lock");

        // The dead api would bump `attempts` if the drain actually ran.
        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(report.remaining, 1);
        assert_eq!(store.intents().unwrap()[0].attempts, 0, "never attempted");
    }

    #[tokio::test]
    async fn an_existing_divergence_gates_the_whole_pass() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("gated");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 409,
                    title: "Conflict".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Default::default(),
                };
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 0, "nothing replays past an open choice");
        assert_eq!(report.remaining, 2);
        assert!(report.diverged);
    }

    #[tokio::test]
    async fn a_parked_intent_never_replays_and_never_gates() {
        let server = MockServer::start().await;
        // Exactly one wire call: the pending resume. The parked pause must
        // neither replay nor gate the pass.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/resume"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("parked");
        let parked = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Parked {
                    reason: "took server · Conflict".into(),
                };
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1, "only the pending intent replayed");
        assert_eq!(report.remaining, 0, "parked is kept, not remaining-to-sync");
        assert!(!report.diverged);

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1, "the parked intent is still stored");
        assert_eq!(intents[0].id, parked.id);
        assert!(intents[0].is_parked(), "kept for review — never deleted");
    }

    // --- plan writes (#115): create replays keyed, adjust/drop replay plain ---

    #[tokio::test]
    async fn activity_create_replays_with_the_stored_idempotency_key() {
        use crate::api::ActivityCreate;
        let server = MockServer::start().await;
        let store = tmp_store("activity-create");
        let body = ActivityCreate {
            title: "one systems paper".into(),
            planned_on: Some("2026-07-13".parse().unwrap()),
            ..Default::default()
        };
        let intent = store.enqueue(IntentKind::ActivityCreate { body }).unwrap();
        // The whole create body re-sends verbatim, carrying the queued key so a
        // lost ack can never mint the plan item twice.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .and(header("Idempotency-Key", intent.idempotency_key.as_str()))
            .and(body_json(serde_json::json!({
                "activity": { "title": "one systems paper", "planned_on": "2026-07-13" }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7, "title": "one systems paper", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "the declare synced");
    }

    #[tokio::test]
    async fn activity_update_and_archive_replay_as_plain_calls() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/5"))
            .and(body_json(serde_json::json!({
                "activity": { "title": "revised" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "revised", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/5/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "revised", "archived_at": "2026-07-16T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("activity-update-archive");
        store
            .enqueue(IntentKind::ActivityUpdate {
                id: 5,
                title: "revised".into(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::ActivityArchive { id: 5 })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2);
        assert_eq!(report.deduped, 0, "plain calls are never a stored replay");
        assert_eq!(report.remaining, 0);
    }
}
