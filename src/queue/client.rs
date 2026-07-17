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

use crate::api::{
    Activity, ActivityCreate, ActivityUpdate, ApiClient, ApiError, Book, BookUpdate, Note,
    NoteInput, Segment, TargetCreate, TargetRef, Timer, TimerStopped, WeekNote,
};
use crate::timer_cache;
use crate::timer_clock;

use super::fold::{self, Provenance};
use super::intent::{provisional_id, Intent, IntentKind};
use super::replay::{self, ReplayError, ReplayReport};
use super::resolve::{self, Resolution, ResolveError, Resolved};
use super::store::{QueueStore, QueueSummary};

/// A stand-in `segment_id` for a stop that landed in the queue: the real id is
/// server-minted on replay, so the provisional confirmation carries a negative
/// sentinel and the caller renders "queued" instead of `segment #N`.
pub const PROVISIONAL_SEGMENT_ID: i64 = -1;

/// A stand-in `id` for a plan item declared while offline: the real id is
/// server-minted on replay, so the provisional row carries a negative sentinel
/// and the board renders it `◔ … queued` instead of a real activity id.
pub const PROVISIONAL_ACTIVITY_ID: i64 = -1;

/// A stand-in `id` for a target declared while offline — a negative sentinel
/// like [`PROVISIONAL_ACTIVITY_ID`], so the Progress screen renders the declare
/// as queued rather than a real target id (the caller draws the queued line from
/// its own label; this provisional value is discarded).
pub const PROVISIONAL_TARGET_ID: i64 = -1;

/// A stand-in `id` for a note captured while offline — a negative sentinel like
/// the others, so a queued capture renders as provisional (the caller draws its
/// confirmation from the echoed title, not this id).
pub const PROVISIONAL_NOTE_ID: i64 = -1;

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
                pending: 0,
                diverged: 0,
                parked: 0,
            }
        })
    }

    /// The first intent waiting on a divergence choice, payload and all — what
    /// the Timer screen's reconcile panel renders. Best-effort like the other
    /// reads: an unreadable queue reads as no divergence here.
    pub fn first_diverged(&self) -> Option<Intent> {
        self.store
            .intents()
            .ok()?
            .into_iter()
            .find(Intent::is_diverged)
    }

    /// Apply a divergence resolution (`queue::resolve`), seeding the local
    /// session's identity from this client's read cache. Callers continue the
    /// drain after a successful keep-local/keep-both.
    pub async fn resolve_divergence(
        &self,
        intent_id: u64,
        resolution: Resolution,
    ) -> Result<Resolved, ResolveError> {
        let cached = self.cached_timer().map(|s| s.timer);
        resolve::resolve(
            &self.api,
            &self.store,
            cached.as_ref(),
            intent_id,
            resolution,
            jiff::Timestamp::now(),
        )
        .await
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
    /// after a successful one-shot read. Skips instantly when nothing is in
    /// play (a summary check before taking any lock — parked intents never
    /// re-trigger it) and swallows failures with a log line: the caller's own
    /// call carries the user-facing error, and a divergence keeps surfacing
    /// through the `queued`/`diverged` read fields until it is resolved.
    pub async fn drain_best_effort(&self) {
        if self.queue_summary().in_play() == 0 {
            return;
        }
        if let Err(e) = self.drain().await {
            tracing::warn!(target: "engineer_cli::queue", error = %e, "queue drain failed");
        }
    }

    /// The reconnect drain the TUI header poll fires: like [`drain_best_effort`],
    /// but streaming each acknowledged intent to `on_replay` so the caller can
    /// paint the reconnect transcript (`back online · replaying the queue…`).
    /// Same best-effort contract — skips instantly when nothing is in play (no
    /// lock taken, so no false transcript), swallows a failed pass with a log line —
    /// and returns the [`ReplayReport`] the `✓ synced` tile reads. `None` when
    /// there was nothing to drain, so the caller shows nothing.
    ///
    /// [`drain_best_effort`]: Self::drain_best_effort
    pub async fn drain_reporting(&self, on_replay: impl FnMut(&Intent)) -> Option<ReplayReport> {
        if self.queue_summary().in_play() == 0 {
            return None;
        }
        match replay::drain_reporting(&self.api, &self.store, on_replay).await {
            Ok(report) => Some(report),
            Err(e) => {
                tracing::warn!(target: "engineer_cli::queue", error = %e, "reconnect drain failed");
                None
            }
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

    /// Declare a plan item — a `planned` activity carrying `planned_on` (the
    /// board's `a`). Drain-before-live, then the live POST; offline it enqueues
    /// an [`IntentKind::ActivityCreate`] and returns a provisional negative-id
    /// row the board renders `◔ queued`. Like [`start_timer`](Self::start_timer),
    /// an offline create needs no cached snapshot — declaring a fresh item is
    /// legitimate with nothing local to fold over.
    pub async fn create_activity(
        &self,
        body: &ActivityCreate,
    ) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        match self.api.create_activity(body).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => {
                // Enqueue first (the gesture must never be lost), then seed the
                // provisional row's id from the enqueued intent — `-(intent.id)`,
                // the negative the replay id-map rewrites once this create lands
                // and a queued segment referencing it must find the real parent
                // (#108). A blank create body still names its own row.
                let intent = self
                    .store
                    .enqueue(IntentKind::ActivityCreate { body: body.clone() })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                let mut provisional = provisional_activity(body);
                provisional.id = provisional_id(intent.id);
                Ok(WriteOutcome::Provisional(provisional))
            }
            Err(e) => Err(e),
        }
    }

    /// Adjust a plan item's title in place (the board's `e`). Offline enqueues
    /// an [`IntentKind::ActivityUpdate`] and returns the row with the new title.
    pub async fn update_activity(
        &self,
        id: i64,
        title: &str,
    ) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        let body = ActivityUpdate {
            title: Some(title.to_string()),
        };
        match self.api.update_activity(id, &body).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => self.defer_activity(
                IntentKind::ActivityUpdate {
                    id,
                    title: title.to_string(),
                },
                Activity {
                    id,
                    title: title.to_string(),
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Drop a plan item — archive it (the board's `d`, second press). Offline
    /// enqueues an [`IntentKind::ActivityArchive`] and returns the row marked
    /// archived.
    pub async fn archive_activity(&self, id: i64) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        match self.api.archive_activity(id).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => self.defer_activity(
                IntentKind::ActivityArchive { id },
                Activity {
                    id,
                    archived_at: Some(jiff::Timestamp::now()),
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Mark an activity done — the Activities table's `c` (`POST
    /// /api/v1/activities/:id/complete`). Drain-before-live, then the live POST;
    /// offline it enqueues an [`IntentKind::ActivityComplete`] and returns the
    /// row field-flipped to `completed`. Always a real id — the table refuses the
    /// gesture on a still-queued provisional row (#109), so `id` is never
    /// provisional here.
    pub async fn complete_activity(&self, id: i64) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        match self.api.complete_activity(id).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => self.defer_activity(
                IntentKind::ActivityComplete { id },
                Activity {
                    id,
                    status: Some("completed".into()),
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Restore an archived plan item — the Activities table's `a` toggle on an
    /// archived row (`PATCH /api/v1/activities/:id/unarchive`). Offline enqueues
    /// an [`IntentKind::ActivityUnarchive`] and returns the row field-flipped back
    /// to active (`archived_at` cleared — the `Default`).
    pub async fn unarchive_activity(&self, id: i64) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        match self.api.unarchive_activity(id).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => self.defer_activity(
                IntentKind::ActivityUnarchive { id },
                // Field-flip: not archived — the cleared `archived_at` is the
                // `Default`, so the synthesized row already reads as active.
                Activity {
                    id,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// "Do this again" — the Activities table's `d` (`POST
    /// /api/v1/activities/:id/duplicate`). The server mints a fresh `planned`
    /// copy; offline it enqueues an [`IntentKind::ActivityDuplicate`] and
    /// synthesizes a provisional negative-id copy the caller renders as queued.
    /// Duplicate is not in the server's `Idempotency-Key` opt-in set (ADR 0036),
    /// so the replay re-fires plain: a lost ack that re-sends mints a *second*
    /// visible, archivable copy — the accepted #110 risk (a duplicate beats a
    /// silently-dropped gesture, and a planned copy is never double-*counted*).
    pub async fn duplicate_activity(&self, id: i64) -> Result<WriteOutcome<Activity>, ApiError> {
        self.drain_best_effort().await;
        match self.api.duplicate_activity(id).await {
            Ok(a) => Ok(WriteOutcome::Confirmed(a)),
            Err(ApiError::Transport(_)) => {
                // A duplicate mints a new row — synthesize a provisional
                // negative-id copy like a create does, seeded from the enqueued
                // intent's id. The caller draws its `queued` line from its own
                // label; this stand-in only needs to read as a fresh planned row.
                let intent = self
                    .store
                    .enqueue(IntentKind::ActivityDuplicate { id })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                Ok(WriteOutcome::Provisional(Activity {
                    id: provisional_id(intent.id),
                    status: Some("planned".into()),
                    ..Default::default()
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Append a manual segment to an existing activity — the `engineer log
    /// --activity` write (after-the-fact time on work already recorded).
    /// Drain-before-live, then the live POST; offline it enqueues an
    /// [`IntentKind::SegmentCreate`] and returns a provisional `◔ queued`
    /// segment.
    ///
    /// `activity_id` is normally the real id the *live* fuzzy resolve returned —
    /// the append shape refuses offline before ever reaching here when it can't
    /// resolve one, exactly like a query'd `timer start` (`engineer log`), so
    /// what queues is a race where the resolve landed but the write's wire then
    /// dropped. When the id is a still-queued create's provisional negative id,
    /// the replay stitches the real one on before the segment posts (#108).
    pub async fn create_segment(
        &self,
        activity_id: i64,
        started_at: jiff::Timestamp,
        minutes: u32,
    ) -> Result<WriteOutcome<Segment>, ApiError> {
        self.drain_best_effort().await;
        match self
            .api
            .create_segment(activity_id, started_at, minutes)
            .await
        {
            Ok(seg) => Ok(WriteOutcome::Confirmed(seg)),
            Err(ApiError::Transport(_)) => {
                let intent = self
                    .store
                    .enqueue(IntentKind::SegmentCreate {
                        activity_id,
                        started_at,
                        minutes,
                    })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                Ok(WriteOutcome::Provisional(Segment {
                    id: provisional_id(intent.id),
                    activity_id: Some(activity_id),
                    minutes: Some(minutes),
                    started_at: Some(started_at),
                    ended_at: None,
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Persist the week's retro reflection (the board's `i`, `engineer week
    /// reflect`) — `PATCH /api/v1/weeks/:iso_week/note`. Drain-before-live, then
    /// the live PATCH; offline it enqueues an [`IntentKind::WeekNoteWrite`] and
    /// echoes the written body back as the provisional note. Like a plan create,
    /// an offline note write needs no cached snapshot — the reflection names its
    /// own body, so there is always something to synthesize. An empty `body` is a
    /// deliberate clear (the server's `week_notes` contract).
    pub async fn update_week_note(
        &self,
        iso_week: &str,
        body: &str,
    ) -> Result<WriteOutcome<WeekNote>, ApiError> {
        self.drain_best_effort().await;
        match self.api.update_week_note(iso_week, body).await {
            Ok(note) => Ok(WriteOutcome::Confirmed(note)),
            Err(ApiError::Transport(_)) => {
                self.store
                    .enqueue(IntentKind::WeekNoteWrite {
                        iso_week: iso_week.to_string(),
                        body: body.to_string(),
                    })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                // Synthesis is trivial — the written body echoed straight back.
                Ok(WriteOutcome::Provisional(WeekNote {
                    iso_week: iso_week.to_string(),
                    body: body.to_string(),
                    updated_at: None,
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Declare a weekly target — the Progress `n` flow (`POST /api/v1/targets`).
    /// Drain-before-live, then the live POST; offline it enqueues an
    /// [`IntentKind::TargetCreate`] carrying the whole body and returns a
    /// provisional negative-id row the screen renders as queued. Like a plan
    /// create, an offline declare needs no cached snapshot — a fresh target is
    /// legitimate with nothing local to fold.
    pub async fn create_target(
        &self,
        create: &TargetCreate,
    ) -> Result<WriteOutcome<TargetRef>, ApiError> {
        self.drain_best_effort().await;
        match self.api.create_target(create).await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(_)) => self.defer_target(
                IntentKind::TargetCreate {
                    body: create.clone(),
                },
                TargetRef {
                    id: PROVISIONAL_TARGET_ID,
                    hours_per_week: create.hours_per_week,
                    active: true,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Adjust a target's weekly hours (Progress `e`). Offline enqueues an
    /// [`IntentKind::TargetAdjust`]; the replay re-addresses a closed version to
    /// the lineage's live row (ADR 0026). A confirmed adjust returns the LIVE row
    /// — its id may differ when the edit minted a successor version, so the caller
    /// re-reads rather than trusting the addressed id.
    pub async fn adjust_target(
        &self,
        id: i64,
        hours: f64,
    ) -> Result<WriteOutcome<TargetRef>, ApiError> {
        self.drain_best_effort().await;
        match self.api.update_target(id, hours).await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(_)) => self.defer_target(
                IntentKind::TargetAdjust { id, hours },
                TargetRef {
                    id,
                    hours_per_week: hours,
                    active: true,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Retire a target — close the lineage, never delete (Progress `x`). Offline
    /// enqueues an [`IntentKind::TargetRetire`] and returns the row marked
    /// retired; the replay is a plain call (a second retire is idempotent).
    pub async fn retire_target(&self, id: i64) -> Result<WriteOutcome<TargetRef>, ApiError> {
        self.drain_best_effort().await;
        match self.api.retire_target(id).await {
            Ok(t) => Ok(WriteOutcome::Confirmed(t)),
            Err(ApiError::Transport(_)) => self.defer_target(
                IntentKind::TargetRetire { id },
                TargetRef {
                    id,
                    active: false,
                    retired: true,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Capture a study note — `POST /api/v1/notes` (the quick-capture overlay's
    /// save, and `engineer note capture`). Drain-before-live, then the live POST;
    /// offline it enqueues an [`IntentKind::NoteCreate`] carrying the whole body
    /// and echoes a provisional negative-id note the caller renders as queued.
    /// Like a plan / target declare, an offline capture needs no cached snapshot —
    /// a fresh note names its own body, always legitimate to synthesize.
    ///
    /// Anchored captures (`--book`) never reach the offline arm: the book search
    /// that resolves the anchor is a live read, so a `--book` capture already
    /// refused before this call. What queues is always a loose thought.
    pub async fn create_note(&self, body: &NoteInput) -> Result<WriteOutcome<Note>, ApiError> {
        self.drain_best_effort().await;
        match self.api.create_note(body).await {
            Ok(n) => Ok(WriteOutcome::Confirmed(n)),
            Err(ApiError::Transport(_)) => {
                self.store
                    .enqueue(IntentKind::NoteCreate { body: body.clone() })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                Ok(WriteOutcome::Provisional(provisional_note(body)))
            }
            Err(e) => Err(e),
        }
    }

    /// Revise a study note in place — `PATCH /api/v1/notes/:id` (the browser's
    /// `e` edit overlay save, including the #124 anchor save). Drain-before-live,
    /// then the live PATCH; offline it enqueues an [`IntentKind::NoteUpdate`]
    /// carrying the whole body **verbatim** and echoes a provisional note with
    /// the new body. The body rides the intent untouched, so the `NoteInput`
    /// omit-vs-replace anchors contract is held through the queue: an omitted
    /// `anchors` replays as an omit (citations untouched), a present one replays
    /// as a replace. Always a real server id — an offline-created note is not
    /// reachable to edit before it syncs.
    pub async fn update_note(
        &self,
        id: i64,
        body: &NoteInput,
    ) -> Result<WriteOutcome<Note>, ApiError> {
        self.drain_best_effort().await;
        match self.api.update_note(id, body).await {
            Ok(n) => Ok(WriteOutcome::Confirmed(n)),
            Err(ApiError::Transport(_)) => self.defer_note(
                IntentKind::NoteUpdate {
                    id,
                    body: body.clone(),
                },
                Note {
                    id,
                    title: body.title.clone(),
                    content: body.content.clone(),
                    book_id: body.book_id,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Shelve a note — `PATCH /api/v1/notes/:id/archive` (the browser's `a` on an
    /// active note). Offline enqueues an [`IntentKind::NoteArchive`] and returns
    /// the row field-flipped to archived.
    pub async fn archive_note(&self, id: i64) -> Result<WriteOutcome<Note>, ApiError> {
        self.drain_best_effort().await;
        match self.api.archive_note(id).await {
            Ok(n) => Ok(WriteOutcome::Confirmed(n)),
            Err(ApiError::Transport(_)) => self.defer_note(
                IntentKind::NoteArchive { id },
                Note {
                    id,
                    archived_at: Some(jiff::Timestamp::now()),
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Restore a shelved note — `PATCH /api/v1/notes/:id/unarchive` (the
    /// browser's `a` on an archived note). Offline enqueues an
    /// [`IntentKind::NoteUnarchive`] and returns the row field-flipped back to
    /// active (`archived_at` cleared — the `Default`).
    pub async fn unarchive_note(&self, id: i64) -> Result<WriteOutcome<Note>, ApiError> {
        self.drain_best_effort().await;
        match self.api.unarchive_note(id).await {
            Ok(n) => Ok(WriteOutcome::Confirmed(n)),
            Err(ApiError::Transport(_)) => self.defer_note(
                IntentKind::NoteUnarchive { id },
                Note {
                    id,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Detach a note from its book — `PATCH /api/v1/notes/:id/unlink` (the
    /// detail's `u`). Offline enqueues an [`IntentKind::NoteUnlink`] and returns
    /// the row field-flipped loose (`book_id` cleared, `book_linked` false — both
    /// the `Default`).
    pub async fn unlink_note(&self, id: i64) -> Result<WriteOutcome<Note>, ApiError> {
        self.drain_best_effort().await;
        match self.api.unlink_note(id).await {
            Ok(n) => Ok(WriteOutcome::Confirmed(n)),
            Err(ApiError::Transport(_)) => self.defer_note(
                IntentKind::NoteUnlink { id },
                Note {
                    id,
                    ..Default::default()
                },
            ),
            Err(e) => Err(e),
        }
    }

    /// Update a book in place — `PATCH /api/v1/books/:id` (the detail's `s`
    /// status flip, `p` page set, `⎵` chapter-done). Drain-before-live, then the
    /// live PATCH; offline it enqueues an [`IntentKind::BookUpdate`] carrying the
    /// whole partial body and returns `current` field-flipped by the set fields —
    /// a faithful stand-in the detail renders in place of the confirmed row until
    /// it syncs. Always a real server id — books are never created offline.
    ///
    /// `current` is the last-known book the caller already holds; the seam
    /// field-flips it (rather than synthesizing a stub) so the provisional row
    /// keeps the book's title, author, and progress instead of blanking them.
    pub async fn update_book(
        &self,
        id: i64,
        body: &BookUpdate,
        current: &Book,
    ) -> Result<WriteOutcome<Book>, ApiError> {
        self.drain_best_effort().await;
        match self.api.update_book(id, body).await {
            Ok(b) => Ok(WriteOutcome::Confirmed(b)),
            Err(ApiError::Transport(_)) => {
                self.store
                    .enqueue(IntentKind::BookUpdate {
                        id,
                        body: body.clone(),
                    })
                    .map_err(|e| {
                        ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
                    })?;
                let mut flipped = current.clone();
                if let Some(s) = body.status {
                    flipped.status = s;
                }
                if let Some(p) = body.current_page {
                    flipped.current_page = Some(p);
                }
                if let Some(c) = body.current_chapter_id {
                    flipped.current_chapter_id = Some(c);
                }
                Ok(WriteOutcome::Provisional(flipped))
            }
            Err(e) => Err(e),
        }
    }

    /// The offline arm the note field-flip verbs share (edit/archive/unarchive/
    /// unlink) — the [`defer_activity`] twin for [`Note`]: enqueue first (the
    /// gesture must never be lost), then return the provisional stand-in. No
    /// cached snapshot is needed — a note write names its own row.
    ///
    /// [`defer_activity`]: Self::defer_activity
    fn defer_note(
        &self,
        kind: IntentKind,
        provisional: Note,
    ) -> Result<WriteOutcome<Note>, ApiError> {
        self.store.enqueue(kind).map_err(|e| {
            ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
        })?;
        Ok(WriteOutcome::Provisional(provisional))
    }

    /// The offline arm the target-write verbs share — the [`defer_activity`]
    /// twin for [`TargetRef`]: enqueue first (the gesture must never be lost),
    /// then return the provisional stand-in. No cached snapshot is needed — a
    /// target write names its own row.
    ///
    /// [`defer_activity`]: Self::defer_activity
    fn defer_target(
        &self,
        kind: IntentKind,
        provisional: TargetRef,
    ) -> Result<WriteOutcome<TargetRef>, ApiError> {
        self.store.enqueue(kind).map_err(|e| {
            ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
        })?;
        Ok(WriteOutcome::Provisional(provisional))
    }

    /// The offline arm the plan-write verbs share: enqueue the intent (the
    /// gesture must never be lost — a loud error if even that fails), then
    /// return the provisional stand-in. Unlike the timer [`defer`](Self::defer),
    /// no cached snapshot is required — a plan write names its own row (a fresh
    /// negative id for a create, the target id for adjust/drop), so there is
    /// always something to synthesize.
    fn defer_activity(
        &self,
        kind: IntentKind,
        provisional: Activity,
    ) -> Result<WriteOutcome<Activity>, ApiError> {
        self.store.enqueue(kind).map_err(|e| {
            ApiError::Transport(format!("offline, and queueing the write failed: {e}"))
        })?;
        Ok(WriteOutcome::Provisional(provisional))
    }

    /// The offline arm shared by every wrapped timer verb: enqueue first (the
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

/// A negative-id stand-in for a queued capture, seeded from the create body —
/// the caller renders it as queued until the replay mints the real note.
fn provisional_note(body: &NoteInput) -> Note {
    Note {
        id: PROVISIONAL_NOTE_ID,
        title: body.title.clone(),
        content: body.content.clone(),
        book_id: body.book_id,
        ..Default::default()
    }
}

/// A negative-id `planned` stand-in for a queued declare, seeded from the create
/// body — the board renders it `◔ … queued` until the replay mints the real row.
fn provisional_activity(body: &ActivityCreate) -> Activity {
    Activity {
        id: PROVISIONAL_ACTIVITY_ID,
        title: body.title.clone(),
        kind: body.kind.clone(),
        // Echoed so a completed-activity log's `--json` carries the duration it
        // was given; a plan declare (no duration) leaves it `None`, as before.
        duration_minutes: body.duration_minutes,
        status: Some("planned".into()),
        ..Default::default()
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

    // --- plan writes (#115): create / update / archive through the seam ---

    #[tokio::test]
    async fn live_create_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7, "title": "one systems paper", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-create");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let body = ActivityCreate {
            title: "one systems paper".into(),
            ..Default::default()
        };
        let out = queued.create_activity(&body).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().id, 7);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_create_enqueues_and_synthesizes_a_provisional_row() {
        // Unlike the other verbs, a declare needs no cached snapshot — there is
        // nothing local to fold; a fresh negative-id row is always legitimate.
        let api = dead_api();
        let dir = tmp_dir("offline-create");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        let body = ActivityCreate {
            title: "one systems paper".into(),
            planned_on: Some("2026-07-13".parse().unwrap()),
            ..Default::default()
        };
        let out = queued.create_activity(&body).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().title, "one systems paper");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "plan");
        assert_eq!(intents[0].stream, "activity");
        assert_eq!(
            out.value().id,
            -(intents[0].id as i64),
            "the provisional id is -(intent.id) — the replay id-map's key (#108)"
        );
    }

    #[tokio::test]
    async fn offline_update_enqueues_and_synthesizes() {
        let api = dead_api();
        let dir = tmp_dir("offline-update");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.update_activity(42, "new title").await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().id, 42);
        assert_eq!(out.value().title, "new title");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "adjust");
        assert_eq!(
            intents[0].stream, "activity:42",
            "keyed on the row it edits"
        );
    }

    #[tokio::test]
    async fn offline_archive_enqueues_and_marks_archived() {
        let api = dead_api();
        let dir = tmp_dir("offline-archive");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.archive_activity(42).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().is_archived());

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "drop");
        assert_eq!(intents[0].stream, "activity:42");
    }

    // --- activity lifecycle verbs (#110): complete / unarchive / duplicate ---

    #[tokio::test]
    async fn live_complete_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/7/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "T", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-complete");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.complete_activity(7).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().status.as_deref(), Some("completed"));
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_complete_enqueues_and_marks_completed() {
        let api = dead_api();
        let dir = tmp_dir("offline-complete");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.complete_activity(42).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().id, 42);
        assert_eq!(
            out.value().status.as_deref(),
            Some("completed"),
            "field-flip synthesis"
        );

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "complete");
        assert_eq!(intents[0].stream, "activity:42");
    }

    #[tokio::test]
    async fn offline_unarchive_enqueues_and_marks_active() {
        let api = dead_api();
        let dir = tmp_dir("offline-unarchive");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.unarchive_activity(42).await.unwrap();
        assert!(out.is_provisional());
        assert!(!out.value().is_archived(), "field-flip: restored to active");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "unarchive");
        assert_eq!(intents[0].stream, "activity:42");
    }

    #[tokio::test]
    async fn offline_duplicate_enqueues_and_synthesizes_a_provisional_copy() {
        let api = dead_api();
        let dir = tmp_dir("offline-duplicate");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.duplicate_activity(3).await.unwrap();
        assert!(out.is_provisional());
        assert!(
            out.value().id < 0,
            "a queued duplicate has no server id yet"
        );
        assert_eq!(out.value().status.as_deref(), Some("planned"));

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "duplicate");
        assert_eq!(
            intents[0].stream, "activity:3",
            "keyed on the source row it copies"
        );
        assert_eq!(
            out.value().id,
            -(intents[0].id as i64),
            "the provisional id is -(intent.id)"
        );
    }

    // --- segment append (#108): create_segment through the seam ---

    #[tokio::test]
    async fn live_segment_append_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 71, "activity_id": 9, "minutes": 30
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-segment");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let started = jiff::Timestamp::now();
        let out = queued.create_segment(9, started, 30).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().id, 71);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_segment_append_enqueues_and_returns_a_provisional_segment() {
        // The append shape reaches the offline arm only on a race — the live
        // fuzzy resolve landed a real activity id, then the write's wire dropped.
        let api = dead_api();
        let dir = tmp_dir("offline-segment");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let started: jiff::Timestamp = "2026-07-15T13:00:00Z".parse().unwrap();
        let out = queued.create_segment(9, started, 20).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().activity_id, Some(9), "the resolved real parent");
        assert_eq!(out.value().minutes, Some(20));
        assert!(out.value().id < 0, "a queued segment has no server id yet");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "log");
        assert_eq!(intents[0].stream, "activity:9");
        match &intents[0].kind {
            IntentKind::SegmentCreate {
                activity_id,
                minutes,
                ..
            } => {
                assert_eq!(*activity_id, 9);
                assert_eq!(*minutes, 20);
            }
            other => panic!("expected a SegmentCreate intent, got {other:?}"),
        }
    }

    // --- reflection (#117): the week note through the seam ---

    #[tokio::test]
    async fn live_week_note_write_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/weeks/2026-W29/note"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "iso_week": "2026-W29", "body": "build second"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-week-note");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued
            .update_week_note("2026-W29", "build second")
            .await
            .unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().body, "build second");
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_week_note_write_enqueues_and_echoes_the_body() {
        // Like a plan create, a note write needs no cached snapshot — the
        // reflection names its own body, always legitimate to synthesize.
        let api = dead_api();
        let dir = tmp_dir("offline-week-note");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        let out = queued
            .update_week_note("2026-W29", "Read the paper first, build second.")
            .await
            .unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().body, "Read the paper first, build second.");
        assert_eq!(out.value().iso_week, "2026-W29");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "reflect");
        assert_eq!(intents[0].stream, "week:2026-W29");
    }

    // --- note capture (#123): create through the seam ---

    #[tokio::test]
    async fn live_capture_is_confirmed_and_queues_nothing() {
        use crate::api::NoteInput;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/notes"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 9, "title": "MVCC keeps one version", "citations": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-capture");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let body = NoteInput {
            title: "MVCC keeps one version".into(),
            ..Default::default()
        };
        let out = queued.create_note(&body).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().id, 9);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_capture_enqueues_and_echoes_a_provisional_note() {
        // Like a plan / target declare, a capture needs no cached snapshot — the
        // note names its own body, always legitimate to synthesize.
        use crate::api::NoteInput;
        let api = dead_api();
        let dir = tmp_dir("offline-capture");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        let body = NoteInput {
            title: "teach CAP via a live partition demo".into(),
            content: Some("teach CAP via a live partition demo".into()),
            ..Default::default()
        };
        let out = queued.create_note(&body).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().title, "teach CAP via a live partition demo");
        assert!(out.value().id < 0, "a queued capture has no server id yet");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "capture");
        assert_eq!(intents[0].stream, "note");
        match &intents[0].kind {
            IntentKind::NoteCreate { body } => {
                assert_eq!(body.title, "teach CAP via a live partition demo");
                assert!(body.book_id.is_none(), "a queued capture is loose");
            }
            other => panic!("expected a NoteCreate intent, got {other:?}"),
        }
    }

    // --- target writes (#121): declare / adjust / retire through the seam ---

    #[tokio::test]
    async fn live_declare_is_confirmed_and_queues_nothing() {
        use crate::api::{TargetCreate, TargetScope};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/targets"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 42, "axis": "domain",
                "scope": { "axis": "domain", "value": 7, "domain": { "id": 7, "name": "Distributed Systems" } },
                "hours_per_week": 6.0, "active": true, "retired": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-declare");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let create = TargetCreate {
            scope: TargetScope::Domain(7),
            hours_per_week: 6.0,
        };
        let out = queued.create_target(&create).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().id, 42);
        assert_eq!(out.value().scope.name(), "Distributed Systems");
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_declare_enqueues_and_synthesizes_a_provisional_row() {
        // The dead-address offline declare (#121): a fresh target needs no cached
        // snapshot — a negative-id provisional row is always legitimate.
        use crate::api::{TargetCreate, TargetScope};
        let api = dead_api();
        let dir = tmp_dir("offline-declare");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"), // never written
        );

        let create = TargetCreate {
            scope: TargetScope::Kind("coding".into()),
            hours_per_week: 4.0,
        };
        let out = queued.create_target(&create).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().id < 0, "a queued declare has no server id yet");
        assert!((out.value().hours_per_week - 4.0).abs() < 1e-9);

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "declare");
        assert_eq!(intents[0].stream, "target");
        match &intents[0].kind {
            IntentKind::TargetCreate { body } => {
                assert_eq!(body.scope, TargetScope::Kind("coding".into()));
                assert!((body.hours_per_week - 4.0).abs() < 1e-9);
            }
            other => panic!("expected a TargetCreate intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offline_adjust_enqueues_keyed_on_the_lineage() {
        let api = dead_api();
        let dir = tmp_dir("offline-target-adjust");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.adjust_target(42, 8.0).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().id, 42);
        assert!((out.value().hours_per_week - 8.0).abs() < 1e-9);

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "adjust");
        assert_eq!(intents[0].stream, "target:42", "keyed on the row it edits");
    }

    #[tokio::test]
    async fn offline_retire_enqueues_and_marks_retired() {
        let api = dead_api();
        let dir = tmp_dir("offline-target-retire");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.retire_target(42).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().retired);
        assert!(!out.value().active);

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "retire");
        assert_eq!(intents[0].stream, "target:42");
    }

    // --- note writes (#111): edit / archive / unarchive / unlink through the seam ---

    #[tokio::test]
    async fn live_note_update_is_confirmed_and_queues_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "revised", "citations": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-note-update");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let body = NoteInput {
            title: "revised".into(),
            ..Default::default()
        };
        let out = queued.update_note(7, &body).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().id, 7);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_note_update_enqueues_and_echoes_the_body_verbatim() {
        use crate::api::Anchor;
        let api = dead_api();
        let dir = tmp_dir("offline-note-update");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let body = NoteInput {
            title: "MVCC".into(),
            book_id: Some(11),
            anchors: Some(vec![Anchor {
                chapter_id: Some(3),
                section_id: Some(32),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let out = queued.update_note(7, &body).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().id, 7);
        assert_eq!(out.value().title, "MVCC");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "edit");
        assert_eq!(intents[0].stream, "note:7");
        // The intent carries the body verbatim — anchors and all — so the
        // omit-vs-replace contract rides the queue untouched.
        match &intents[0].kind {
            IntentKind::NoteUpdate { body, .. } => {
                assert_eq!(body.book_id, Some(11));
                let anchors = body.anchors.as_ref().expect("the anchor is kept");
                assert_eq!(anchors[0].chapter_id, Some(3));
                assert_eq!(anchors[0].section_id, Some(32));
            }
            other => panic!("expected a NoteUpdate intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offline_note_update_holds_the_anchors_omit_contract() {
        // An edit that never touched the anchor omits `anchors` — that omission
        // must survive the queue so the replay leaves the citations untouched.
        let api = dead_api();
        let dir = tmp_dir("offline-note-update-omit");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let body = NoteInput {
            title: "kept".into(),
            book_id: Some(3),
            anchors: None,
            ..Default::default()
        };
        queued.update_note(9, &body).await.unwrap();

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        match &intents[0].kind {
            IntentKind::NoteUpdate { body, .. } => {
                assert!(body.anchors.is_none(), "an omitted anchor stays omitted");
                assert_eq!(body.book_id, Some(3), "the book link still rides along");
            }
            other => panic!("expected a NoteUpdate intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offline_note_archive_and_unarchive_flip_the_row() {
        let api = dead_api();
        let dir = tmp_dir("offline-note-archive");
        let store = QueueStore::at(dir.join("queue.json"));
        let queued = QueuedClient::with_paths(&api, store, dir.join("timer-cache.json"));

        let out = queued.archive_note(5).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().archived_at.is_some(), "field-flip: archived");

        let out = queued.unarchive_note(6).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().archived_at.is_none(), "field-flip: active");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "archive");
        assert_eq!(intents[0].stream, "note:5");
        assert_eq!(intents[1].kind.word(), "unarchive");
        assert_eq!(intents[1].stream, "note:6");
    }

    #[tokio::test]
    async fn offline_note_unlink_flips_the_row_loose() {
        let api = dead_api();
        let dir = tmp_dir("offline-note-unlink");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let out = queued.unlink_note(4).await.unwrap();
        assert!(out.is_provisional());
        assert!(out.value().book_id.is_none(), "field-flip: detached");
        assert!(!out.value().book_linked);

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].kind.word(), "unlink");
        assert_eq!(intents[0].stream, "note:4");
    }

    // --- book writes (#111): status / page / chapter through the seam ---

    #[tokio::test]
    async fn live_book_update_is_confirmed_and_queues_nothing() {
        use crate::api::BookStatus;
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/books/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "SICP", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let dir = tmp_dir("live-book-update");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let current: Book = serde_json::from_value(serde_json::json!({
            "id": 7, "title": "SICP", "status": "reading"
        }))
        .unwrap();
        let body = BookUpdate {
            status: Some(BookStatus::Completed),
            ..Default::default()
        };
        let out = queued.update_book(7, &body, &current).await.unwrap();
        assert!(!out.is_provisional());
        assert_eq!(out.value().status, BookStatus::Completed);
        assert_eq!(queued.queue_summary().depth, 0);
    }

    #[tokio::test]
    async fn offline_book_update_enqueues_and_field_flips_the_current_book() {
        use crate::api::BookStatus;
        let api = dead_api();
        let dir = tmp_dir("offline-book-update");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        // A full current book: the flip must keep title/author, not blank them.
        let current: Book = serde_json::from_value(serde_json::json!({
            "id": 7, "title": "SICP", "author": "Abelson & Sussman",
            "status": "reading", "current_page": 100
        }))
        .unwrap();
        let body = BookUpdate {
            status: Some(BookStatus::OnHold),
            ..Default::default()
        };
        let out = queued.update_book(7, &body, &current).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().status, BookStatus::OnHold, "field-flip");
        assert_eq!(out.value().title, "SICP", "the flip keeps the other fields");
        assert_eq!(out.value().author.as_deref(), Some("Abelson & Sussman"));
        assert_eq!(out.value().current_page, Some(100));

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "book");
        assert_eq!(intents[0].stream, "book:7");
        match &intents[0].kind {
            IntentKind::BookUpdate { id, body } => {
                assert_eq!(*id, 7);
                assert_eq!(body.status, Some(BookStatus::OnHold));
            }
            other => panic!("expected a BookUpdate intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offline_book_page_set_flips_the_page_and_queues_it() {
        let api = dead_api();
        let dir = tmp_dir("offline-book-page");
        let queued = QueuedClient::with_paths(
            &api,
            QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        );

        let current: Book = serde_json::from_value(serde_json::json!({
            "id": 7, "title": "SICP", "status": "reading", "current_page": 100
        }))
        .unwrap();
        let body = BookUpdate {
            current_page: Some(142),
            ..Default::default()
        };
        let out = queued.update_book(7, &body, &current).await.unwrap();
        assert!(out.is_provisional());
        assert_eq!(out.value().current_page, Some(142), "the page flipped");

        let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
        assert_eq!(intents[0].stream, "book:7");
        match &intents[0].kind {
            IntentKind::BookUpdate { body, .. } => {
                assert_eq!(body.current_page, Some(142));
                assert!(body.status.is_none(), "only the page rode the intent");
            }
            other => panic!("expected a BookUpdate intent, got {other:?}"),
        }
    }
}
