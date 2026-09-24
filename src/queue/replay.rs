//! The replay pass — pending intents re-send in order when the wire returns
//! (ADR 0004 rule 3).

use std::collections::{HashMap, HashSet};

use crate::api::{ActivityUpdate, ApiClient, ApiError};

use super::intent::{provisional_id, Intent, IntentKind, IntentState};
use super::store::{QueueDocView, QueueError, QueueStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayReport {
    pub replayed: usize,
    pub deduped: usize,
    pub remaining: usize,
    pub diverged: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Api(#[from] ApiError),
}

pub async fn drain(api: &ApiClient, store: &QueueStore) -> Result<ReplayReport, ReplayError> {
    drain_reporting(api, store, |_| {}).await
}

pub async fn drain_reporting(
    api: &ApiClient,
    store: &QueueStore,
    mut on_replay: impl FnMut(&Intent),
) -> Result<ReplayReport, ReplayError> {
    let Some(_guard) = store.try_replay_lock()? else {
        return report(store, 0, 0);
    };

    let intents = store.intents()?;
    let mut blocked: HashSet<String> = intents
        .iter()
        .filter(|i| i.is_diverged())
        .map(|i| i.stream.clone())
        .collect();
    let mut pending: Vec<Intent> = intents.into_iter().filter(Intent::is_pending).collect();
    pending.sort_by_key(|i| i.id);
    let mut replayed = 0usize;
    let mut deduped = 0usize;
    let mut id_map: HashMap<i64, i64> = HashMap::new();

    for intent in pending {
        if blocked.contains(&intent.stream) {
            continue;
        }
        if references_unlanded_create(&intent.kind, &id_map) {
            blocked.insert(intent.stream.clone());
            continue;
        }
        match send_intent(api, &intent, &id_map).await {
            Ok(ack) => {
                if ack.replayed {
                    deduped += 1;
                    tracing::info!(
                        target: "engineer_cli::queue",
                        intent = intent.id,
                        verb = intent.kind.word(),
                        "stored response replayed — the first attempt had landed; consumed as confirmed"
                    );
                }
                let remap = ack
                    .minted_activity_id
                    .map(|real| (provisional_id(intent.id), real));
                if let Some((prov, real)) = remap {
                    id_map.insert(prov, real);
                }
                // One locked write removes the ack and stitches its id onto the
                // dependents: split in two, a crash between them would strand
                // the provisional reference.
                store.mutate(|doc| {
                    doc.intents_mut().retain(|i| i.id != intent.id);
                    if let Some((prov, real)) = remap {
                        remap_activity_references(doc, prov, real);
                    }
                })?;
                replayed += 1;
                on_replay(&intent);
            }
            Err(ApiError::Transport(msg)) => {
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
                blocked.insert(intent.stream.clone());
            }
            Err(e) => return Err(e.into()),
        }
    }

    report(store, replayed, deduped)
}

struct Ack {
    replayed: bool,
    minted_activity_id: Option<i64>,
}

impl Ack {
    fn plain(replayed: bool) -> Self {
        Self {
            replayed,
            minted_activity_id: None,
        }
    }
}

fn resolve_activity(id_map: &HashMap<i64, i64>, activity_id: i64) -> i64 {
    id_map.get(&activity_id).copied().unwrap_or(activity_id)
}

/// A parent create always has the lower id, so by the time a dependent is
/// reached an unresolved negative reference means the parent did not land.
fn references_unlanded_create(kind: &IntentKind, id_map: &HashMap<i64, i64>) -> bool {
    let reference = match kind {
        IntentKind::SegmentCreate { activity_id, .. } => *activity_id,
        IntentKind::ActivityUpdate { id, .. } | IntentKind::ActivityArchive { id } => *id,
        _ => return false,
    };
    resolve_activity(id_map, reference) < 0
}

/// Kinds outside the server's `Idempotency-Key` set (engineer ADR 0036) replay
/// plain; a stored replay is indistinguishable from a first ack there, so their
/// `Ack` never reports a dedupe.
async fn send_intent(
    api: &ApiClient,
    intent: &Intent,
    id_map: &HashMap<i64, i64>,
) -> Result<Ack, ApiError> {
    let key = &intent.idempotency_key;
    match &intent.kind {
        IntentKind::TimerStart {
            activity_id,
            switch,
            ..
        } => api
            .start_timer_idempotent(*activity_id, *switch, key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TimerPause { .. } => api
            .pause_timer_idempotent(key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TimerResume { .. } => api
            .resume_timer_idempotent(key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TimerStop { .. } => api
            .stop_timer_idempotent(key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TimerBind { activity_id, title } => api
            .bind_timer_idempotent(*activity_id, title.clone(), key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TimerDiscard => api.discard_timer().await.map(|()| Ack::plain(false)),
        IntentKind::ActivityCreate { body } => {
            api.create_activity_idempotent(body, key)
                .await
                .map(|k| Ack {
                    replayed: k.replayed,
                    minted_activity_id: Some(k.value.id),
                })
        }
        IntentKind::SegmentCreate {
            activity_id,
            started_at,
            minutes,
        } => api
            .create_segment_idempotent(
                resolve_activity(id_map, *activity_id),
                *started_at,
                *minutes,
                key,
            )
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::ActivityUpdate { id, title } => api
            .update_activity(
                resolve_activity(id_map, *id),
                &ActivityUpdate {
                    title: Some(title.clone()),
                },
            )
            .await
            .map(|_| Ack::plain(false)),
        IntentKind::ActivityArchive { id } => api
            .archive_activity(resolve_activity(id_map, *id))
            .await
            .map(|_| Ack::plain(false)),
        IntentKind::ActivityComplete { id } => {
            api.complete_activity(*id).await.map(|_| Ack::plain(false))
        }
        IntentKind::ActivityUnarchive { id } => {
            api.unarchive_activity(*id).await.map(|_| Ack::plain(false))
        }
        IntentKind::ActivityDuplicate { id } => {
            api.duplicate_activity(*id).await.map(|_| Ack::plain(false))
        }
        IntentKind::WeekNoteWrite { iso_week, body } => api
            .update_week_note(iso_week, body)
            .await
            .map(|_| Ack::plain(false)),
        IntentKind::TargetCreate { body } => api
            .create_target_idempotent(body, key)
            .await
            .map(|k| Ack::plain(k.replayed)),
        IntentKind::TargetAdjust { id, hours } => replay_target_adjust(api, *id, *hours).await,
        IntentKind::TargetRetire { id } => api.retire_target(*id).await.map(|_| Ack::plain(false)),
        IntentKind::NoteCreate { body } => api.create_note(body).await.map(|_| Ack::plain(false)),
        IntentKind::NoteUpdate { id, body } => {
            api.update_note(*id, body).await.map(|_| Ack::plain(false))
        }
        IntentKind::NoteArchive { id } => api.archive_note(*id).await.map(|_| Ack::plain(false)),
        IntentKind::NoteUnarchive { id } => {
            api.unarchive_note(*id).await.map(|_| Ack::plain(false))
        }
        IntentKind::NoteUnlink { id } => api.unlink_note(*id).await.map(|_| Ack::plain(false)),
        IntentKind::BookUpdate { id, body } => {
            api.update_book(*id, body).await.map(|_| Ack::plain(false))
        }
    }
}

fn remap_activity_references(doc: &mut QueueDocView, prov: i64, real: i64) {
    for i in doc.intents_mut().iter_mut() {
        let changed = match &mut i.kind {
            IntentKind::SegmentCreate { activity_id, .. } if *activity_id == prov => {
                *activity_id = real;
                true
            }
            IntentKind::ActivityUpdate { id, .. } | IntentKind::ActivityArchive { id }
                if *id == prov =>
            {
                *id = real;
                true
            }
            _ => false,
        };
        if changed {
            i.stream = i.kind.stream();
        }
    }
}

async fn replay_target_adjust(api: &ApiClient, id: i64, hours: f64) -> Result<Ack, ApiError> {
    match api.update_target(id, hours).await {
        Ok(_) => Ok(Ack::plain(false)),
        Err(e) if e.code() == Some(crate::api::codes::TARGET_VERSION_CLOSED) => {
            match e.live_target_id() {
                Some(live) => api
                    .update_target(live, hours)
                    .await
                    .map(|_| Ack::plain(false)),
                None => Err(e),
            }
        }
        Err(e) => Err(e),
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
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
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
    async fn problem_halts_the_stream_and_persists_diverged() {
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

    #[tokio::test]
    async fn a_lost_ack_replay_dedupes_silently_with_no_duplicate_prompt() {
        let store = tmp_store("dedupe");
        let intent = store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();

        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(store.intents().unwrap()[0].attempts, 1, "the lost ack");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .and(header("Idempotency-Key", intent.idempotency_key.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Idempotency-Replayed", "true")
                    .set_body_json(serde_json::json!({ "running": true, "paused": true })),
            )
            .expect(1)
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

        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(report.remaining, 1);
        assert_eq!(store.intents().unwrap()[0].attempts, 0, "never attempted");
    }

    #[tokio::test]
    async fn an_existing_divergence_gates_its_stream_across_passes() {
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
        assert_eq!(
            report.replayed, 0,
            "nothing replays past the stream's open choice"
        );
        assert_eq!(report.remaining, 2);
        assert!(report.diverged);
    }

    #[tokio::test]
    async fn a_parked_intent_never_replays_and_never_gates() {
        let server = MockServer::start().await;
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

    #[tokio::test]
    async fn activity_lifecycle_verbs_replay_as_plain_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/5/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "T", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/5/unarchive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "T", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/5/duplicate"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "title": "T", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("activity-lifecycle");
        store
            .enqueue(IntentKind::ActivityComplete { id: 5 })
            .unwrap();
        store
            .enqueue(IntentKind::ActivityUnarchive { id: 5 })
            .unwrap();
        store
            .enqueue(IntentKind::ActivityDuplicate { id: 5 })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 3);
        assert_eq!(report.deduped, 0, "plain calls are never a stored replay");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "all three synced");
    }

    #[tokio::test]
    async fn week_note_write_replays_as_a_plain_patch() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/weeks/2026-W29/note"))
            .and(body_json(serde_json::json!({
                "note": { "body": "Read the paper first, build second." }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "iso_week": "2026-W29", "body": "Read the paper first, build second."
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("week-note");
        store
            .enqueue(IntentKind::WeekNoteWrite {
                iso_week: "2026-W29".into(),
                body: "Read the paper first, build second.".into(),
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(report.deduped, 0, "a plain PATCH is never a stored replay");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "the reflection synced");
    }

    #[tokio::test]
    async fn note_create_replays_as_a_plain_post() {
        use crate::api::{Anchor, NoteInput};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/notes"))
            .and(body_json(serde_json::json!({
                "note": { "title": "MVCC keeps one version", "book_id": 3, "anchors": [{ "page": 142 }] }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 9, "title": "MVCC keeps one version", "book_id": 3, "citations": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("note-create");
        store
            .enqueue(IntentKind::NoteCreate {
                body: NoteInput {
                    title: "MVCC keeps one version".into(),
                    book_id: Some(3),
                    anchors: Some(vec![Anchor {
                        page: Some(142),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(report.deduped, 0, "a plain POST is never a stored replay");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "the capture synced");
    }

    #[tokio::test]
    async fn note_update_replays_as_a_plain_patch_with_the_body_verbatim() {
        use crate::api::{Anchor, NoteInput};
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/7"))
            .and(body_json(serde_json::json!({
                "note": { "title": "MVCC", "book_id": 11, "anchors": [{ "chapter_id": 3, "section_id": 32 }] }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "MVCC", "book_id": 11
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("note-update");
        store
            .enqueue(IntentKind::NoteUpdate {
                id: 7,
                body: NoteInput {
                    title: "MVCC".into(),
                    book_id: Some(11),
                    anchors: Some(vec![Anchor {
                        chapter_id: Some(3),
                        section_id: Some(32),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(report.deduped, 0, "a plain PATCH is never a stored replay");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "the edit synced");
    }

    #[tokio::test]
    async fn note_archive_and_unlink_replay_as_plain_calls() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/5/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "T", "archived_at": "2026-07-16T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/5/unlink"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "T", "book_id": null, "book_linked": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("note-archive-unlink");
        store.enqueue(IntentKind::NoteArchive { id: 5 }).unwrap();
        store.enqueue(IntentKind::NoteUnlink { id: 5 }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2);
        assert_eq!(report.deduped, 0, "plain calls are never a stored replay");
        assert_eq!(report.remaining, 0);
    }

    #[tokio::test]
    async fn book_update_replays_as_a_plain_patch() {
        use crate::api::{BookStatus, BookUpdate};
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/books/7"))
            .and(body_json(serde_json::json!({
                "book": { "status": "completed" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "SICP", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("book-update");
        store
            .enqueue(IntentKind::BookUpdate {
                id: 7,
                body: BookUpdate {
                    status: Some(BookStatus::Completed),
                    ..Default::default()
                },
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(report.deduped, 0, "a plain PATCH is never a stored replay");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "the book write synced");
    }

    #[tokio::test]
    async fn target_create_replays_with_the_stored_idempotency_key() {
        use crate::api::{TargetCreate, TargetScope};
        let server = MockServer::start().await;
        let store = tmp_store("target-create");
        let intent = store
            .enqueue(IntentKind::TargetCreate {
                body: TargetCreate {
                    scope: TargetScope::Domain(7),
                    hours_per_week: 6.0,
                },
            })
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/targets"))
            .and(header("Idempotency-Key", intent.idempotency_key.as_str()))
            .and(body_json(serde_json::json!({
                "target": { "axis": "domain", "hours_per_week": 6.0, "domain_id": 7 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain",
                "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 6.0, "active": true, "retired": false
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
    async fn target_adjust_and_retire_replay_as_plain_calls() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .and(body_json(serde_json::json!({
                "target": { "hours_per_week": 8.0 }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain", "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 8.0, "active": true, "retired": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42/retire"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain", "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 8.0, "active": false, "retired": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("target-adjust-retire");
        store
            .enqueue(IntentKind::TargetAdjust { id: 42, hours: 8.0 })
            .unwrap();
        store.enqueue(IntentKind::TargetRetire { id: 42 }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2);
        assert_eq!(report.deduped, 0, "plain calls are never a stored replay");
        assert_eq!(report.remaining, 0);
    }

    #[tokio::test]
    async fn a_closed_version_adjust_readdresses_to_the_live_lineage_row() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/target-version-closed",
                "title": "Target version is closed",
                "status": 422,
                "detail": "Fetch the live target for this axis and scope, then retry.",
                "code": "target-version-closed",
                "live_target_id": 47
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/47"))
            .and(body_json(serde_json::json!({
                "target": { "hours_per_week": 8.0 }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 47, "axis": "domain", "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 8.0, "active": true, "retired": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("target-readdress");
        store
            .enqueue(IntentKind::TargetAdjust { id: 42, hours: 8.0 })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1, "the re-addressed adjust landed");
        assert_eq!(report.remaining, 0);
        assert!(!report.diverged, "a re-address is not a divergence");
        assert!(store.intents().unwrap().is_empty(), "the adjust synced");
    }

    #[tokio::test]
    async fn a_closed_version_adjust_with_no_live_row_diverges() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/target-version-closed",
                "title": "Target version is closed",
                "status": 422,
                "detail": "This lineage is retired.",
                "code": "target-version-closed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("target-readdress-none");
        store
            .enqueue(IntentKind::TargetAdjust { id: 42, hours: 8.0 })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert!(
            report.diverged,
            "no live row to re-address — a real divergence"
        );
        let intents = store.intents().unwrap();
        assert!(intents[0].is_diverged());
    }

    fn completed_create() -> IntentKind {
        use crate::api::ActivityCreate;
        IntentKind::ActivityCreate {
            body: ActivityCreate {
                title: "Raft leader election".into(),
                duration_minutes: Some(20),
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn a_queued_segment_replays_against_the_parents_real_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 55, "title": "Raft leader election", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/55/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": { "started_at": "2026-07-15T13:00:00Z", "duration_minutes": 20 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 55, "minutes": 20
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("idmap-segment");
        let create = store.enqueue(completed_create()).unwrap();
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: provisional_id(create.id),
                started_at: "2026-07-15T13:00:00Z".parse().unwrap(),
                minutes: 20,
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2, "the create and its segment both landed");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty(), "both synced");
    }

    #[tokio::test]
    async fn an_interrupted_drain_persists_the_real_id_onto_the_queued_segment() {
        let store = tmp_store("idmap-interrupted");
        let create = store.enqueue(completed_create()).unwrap();
        let segment = store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: -(create.id as i64),
                started_at: "2026-07-15T13:00:00Z".parse().unwrap(),
                minutes: 20,
            })
            .unwrap();

        let s1 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 55, "title": "Raft leader election", "status": "completed"
            })))
            .mount(&s1)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/55/segments"))
            // No `id` in the body: a decode error aborts the drain, standing in for a
            // crash between the two writes.
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&s1)
            .await;

        let interrupted = drain(&client(&s1), &store).await;
        assert!(interrupted.is_err(), "the pass aborted mid-drain");

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1, "only the segment remains");
        assert_eq!(intents[0].id, segment.id);
        assert!(intents[0].is_pending(), "still to replay — not diverged");
        match &intents[0].kind {
            IntentKind::SegmentCreate { activity_id, .. } => {
                assert_eq!(*activity_id, 55, "the real parent id was stitched on");
            }
            other => panic!("expected a SegmentCreate, got {other:?}"),
        }
        assert_eq!(intents[0].stream, "activity:55", "and the stream too");

        let s2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/55/segments"))
            .and(header("Idempotency-Key", segment.idempotency_key.as_str()))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 55, "minutes": 20
            })))
            .expect(1)
            .mount(&s2)
            .await;

        let report = drain(&client(&s2), &store).await.unwrap();
        assert_eq!(report.replayed, 1, "the segment landed with the real id");
        assert_eq!(report.remaining, 0);
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_queued_edit_of_a_provisional_declare_replays_against_the_real_row() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 55, "title": "draft", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/55"))
            .and(body_json(serde_json::json!({
                "activity": { "title": "revised" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 55, "title": "revised", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("idmap-edit");
        let create = store.enqueue(completed_create()).unwrap();
        store
            .enqueue(IntentKind::ActivityUpdate {
                id: -(create.id as i64),
                title: "revised".into(),
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2, "the declare and its edit both landed");
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_diverged_activity_stream_never_blocks_the_timer_stream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Study day is closed", "status": 422,
                "detail": "2026-07-14 is closed to new work"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/resume"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("per-stream-flow");
        let create = store.enqueue(completed_create()).unwrap();
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TimerResume { at: at() }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 2, "the timer stream drained");
        assert!(
            report.diverged,
            "…while the activity stream waits on a choice"
        );
        assert_eq!(report.remaining, 1, "only the diverged create is left");

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].id, create.id);
        assert!(intents[0].is_diverged());
    }

    #[tokio::test]
    async fn a_segment_on_a_provisional_id_holds_while_its_create_is_diverged() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Study day is closed", "status": 422, "detail": "closed"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(
                r"^/api/v1/activities/.+/segments$",
            ))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42/retire"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain", "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 8.0, "active": false, "retired": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("per-stream-edge");
        let create = store.enqueue(completed_create()).unwrap();
        let segment = store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: -(create.id as i64),
                started_at: "2026-07-15T13:00:00Z".parse().unwrap(),
                minutes: 20,
            })
            .unwrap();
        store.enqueue(IntentKind::TargetRetire { id: 42 }).unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1, "only the independent target landed");
        assert!(report.diverged);
        assert_eq!(
            report.remaining, 2,
            "the diverged create + its held segment"
        );

        let intents = store.intents().unwrap();
        assert!(intents.iter().any(|i| i.id == create.id && i.is_diverged()));
        let held = intents.iter().find(|i| i.id == segment.id).unwrap();
        assert!(held.is_pending(), "held, never sent, never diverged");
        assert_eq!(held.attempts, 0, "the dependent was not even attempted");
    }

    #[tokio::test]
    async fn in_stream_order_holds_behind_a_divergence_while_other_activities_flow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Segment overlaps", "status": 422,
                "detail": "overlaps an existing segment 14:20–15:05"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/9"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/12"))
            .and(body_json(serde_json::json!({
                "activity": { "title": "revised" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 12, "title": "revised", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("per-stream-order");
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        store
            .enqueue(IntentKind::ActivityUpdate {
                id: 9,
                title: "held behind the choice".into(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::ActivityUpdate {
                id: 12,
                title: "revised".into(),
            })
            .unwrap();

        let report = drain(&client(&server), &store).await.unwrap();
        assert_eq!(report.replayed, 1, "only the other activity's stream moved");
        assert!(report.diverged);
        assert_eq!(report.remaining, 2);

        let intents = store.intents().unwrap();
        assert!(intents[0].is_diverged(), "the head hit the wall");
        assert!(intents[1].is_pending(), "same stream: held, untouched");
        assert_eq!(intents[1].attempts, 0);
    }

    #[tokio::test]
    async fn a_transport_failure_halts_every_stream_not_just_its_own() {
        let store = tmp_store("transport-global");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TargetRetire { id: 42 }).unwrap();

        let report = drain(&dead_api(), &store).await.unwrap();
        assert_eq!(report.replayed, 0);
        assert_eq!(report.remaining, 2);

        let intents = store.intents().unwrap();
        assert_eq!(intents[0].attempts, 1);
        assert_eq!(
            intents[1].attempts, 0,
            "another stream is never tried once the wire is down"
        );
    }

    #[tokio::test]
    async fn drain_reporting_never_fires_for_an_intent_that_did_not_land() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Segment overlaps", "status": 422, "detail": "overlap"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("reporting-diverged");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();

        let mut fired = false;
        let report = drain_reporting(&client(&server), &store, |_| fired = true)
            .await
            .unwrap();
        assert!(report.diverged);
        assert!(!fired, "a divergence is not an acknowledgement");

        let store = tmp_store("reporting-offline");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        let report = drain_reporting(&dead_api(), &store, |_| fired = true)
            .await
            .unwrap();
        assert_eq!(report.remaining, 1);
        assert!(!fired, "a transport failure is not an acknowledgement");
    }

    #[tokio::test]
    async fn an_existing_divergence_never_gates_another_stream() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42/retire"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain", "scope": { "axis": "domain", "value": 7 },
                "hours_per_week": 8.0, "active": false, "retired": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("gated-other-stream");
        store.enqueue(IntentKind::TimerPause { at: at() }).unwrap();
        store.enqueue(IntentKind::TargetRetire { id: 42 }).unwrap();
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
        assert_eq!(report.replayed, 1, "the target stream replayed");
        assert!(
            report.diverged,
            "the timer stream still waits on its choice"
        );
        assert_eq!(report.remaining, 1);
    }
}
