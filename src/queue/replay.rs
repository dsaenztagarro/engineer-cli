//! The replay pass — pending intents re-send in order when the wire returns
//! (offline-write.brief.md §8 Foundation, "a replay-on-reconnect pass").
//!
//! Strict FIFO by intent `id`, one intent at a time, single-flight across
//! processes via the `replay.lock` sidecar. The server stays authoritative:
//! a replayed intent leaves the queue the moment the server acknowledges it,
//! and the pass halts the instant the server *disagrees* — a divergence needs
//! a human choice before anything later replays, or ordering would lie.

use crate::api::{ApiClient, ApiError};

use super::intent::{Intent, IntentKind, IntentState};
use super::store::{QueueError, QueueStore};

/// What one drain accomplished — the callers render this, so both the
/// `engineer queue sync` line and the TUI tile speak from the same numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayReport {
    /// Intents the server acknowledged; they have left the queue.
    pub replayed: usize,
    /// Intents still in the queue after the pass (pending + diverged).
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
    let Some(_guard) = store.try_replay_lock()? else {
        return report(store, 0);
    };

    if store.summary()?.diverged > 0 {
        return report(store, 0);
    }

    let mut pending = store.pending()?;
    pending.sort_by_key(|i| i.id);
    let mut replayed = 0usize;

    for intent in pending {
        match send_intent(api, &intent).await {
            Ok(()) => {
                // Acknowledged — the server is authoritative now; the intent
                // leaves the queue (under the writer lock, like all mutation).
                store.mutate(|doc| doc.intents_mut().retain(|i| i.id != intent.id))?;
                replayed += 1;
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
            }) => {
                // The server moved on — persist its objection verbatim and
                // halt: nothing later replays past an unresolved divergence.
                store.mutate(|doc| {
                    if let Some(i) = doc.intents_mut().iter_mut().find(|i| i.id == intent.id) {
                        i.attempts += 1;
                        i.state = IntentState::Diverged {
                            status,
                            title,
                            detail,
                            type_uri,
                            errors,
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

    report(store, replayed)
}

/// Re-send one intent through the typed call for its kind, carrying the
/// stored `Idempotency-Key` so a lost ack can never double-write.
async fn send_intent(api: &ApiClient, intent: &Intent) -> Result<(), ApiError> {
    let key = &intent.idempotency_key;
    match &intent.kind {
        IntentKind::TimerStart {
            activity_id,
            switch,
            ..
        } => api
            .start_timer_idempotent(*activity_id, *switch, key)
            .await
            .map(drop),
        IntentKind::TimerPause { .. } => api.pause_timer_idempotent(key).await.map(drop),
        IntentKind::TimerResume { .. } => api.resume_timer_idempotent(key).await.map(drop),
        IntentKind::TimerStop { .. } => api.stop_timer_idempotent(key).await.map(drop),
        IntentKind::TimerBind { activity_id, title } => api
            .bind_timer_idempotent(*activity_id, title.clone(), key)
            .await
            .map(drop),
        // DELETE needs no idempotent variant: deleting the singleton twice is
        // naturally idempotent — the second delete finds nothing to write.
        IntentKind::TimerDiscard => api.discard_timer().await,
    }
}

fn report(store: &QueueStore, replayed: usize) -> Result<ReplayReport, ReplayError> {
    let summary = store.summary()?;
    Ok(ReplayReport {
        replayed,
        remaining: summary.depth,
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
            } => {
                assert_eq!(*status, 422);
                assert_eq!(title, "Segment overlaps");
                assert!(detail.contains("09:00"));
                assert!(type_uri.as_deref().unwrap().contains("overlap"));
                assert_eq!(errors.len(), 1, "the full RFC 7807 payload is kept");
            }
            IntentState::Pending => panic!("expected the rejected intent to be diverged"),
        }
        assert!(intents[1].is_pending(), "the rest stays pending, untouched");
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
                };
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 0, "nothing replays past an open choice");
        assert_eq!(report.remaining, 2);
        assert!(report.diverged);
    }
}
