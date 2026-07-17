//! Resolving a divergence — the pick-a-side engine behind the Timer screen's
//! reconcile panel and `engineer queue resolve` (offline-write.brief.md job 4;
//! the §Diverged boards). One module, one spelling of every outcome: both
//! surfaces call [`resolve`], so a TUI gesture and a headless flag cannot
//! drift apart.
//!
//! The honesty invariant this module exists to keep: **no resolution loses a
//! segment silently.** Every arm either *writes* (a `start_timer(switch)` that
//! stops & saves the server session, a `create_segment` carrying the local
//! minutes) or *keeps* ([`IntentState::Parked`] — the intents stay in
//! `queue.json`, visible as `parked`, excluded from replay, never deleted).
//!
//! The coded conflicts (engineer#806, ADR 0036) sharpen the compositions the
//! generic RFC 7807 fallback couldn't support: a `no-live-timer` divergence
//! proves the session is gone server-side, so keep-local on a pause/resume
//! composes the local minutes into a `create_segment` instead of switching (a
//! switch against a gone session would restart the clock at zero — a silent
//! loss); a `timer-already-running` conflict carries the server session's
//! `current.activity_id`, the last-resort anchor for an otherwise-unbound
//! keep-both. Where the payload *still* can't compose a resolution (no code,
//! no anchor anywhere, no server segment id for the drift composition), the
//! arm refuses with [`ResolveError::CannotCompose`] naming what's missing, and
//! the intent stays diverged — loud, unresolved, un-dropped.

//! **The rejected write (§Diverged · rejected segment, #109).** A 422 on a
//! replayed `SegmentCreate`/`ActivityCreate` (an overlap, a closed study day)
//! resolves through three gestures instead of the pick-a-side pair: **edit**
//! ([`edit_seed`] → `$EDITOR` → [`apply_edit`] re-pends the intent with the
//! corrected payload and a fresh idempotency key), **drop** ([`drop_intent`] —
//! the one genuinely user-chosen delete in the queue's life, always explicit
//! and confirmed by the caller), and **skip** ([`skip_intent`] — parks it,
//! reason `skipped`, kept in `queue.json` and out of replay until a later
//! choice). Both surfaces (the TUI reconcile panel, `engineer queue resolve
//! --edit/--drop/--skip`) call these same functions.

use crate::api::{codes, ApiClient, ApiError, ConflictInfo, Timer};
use crate::timer_clock;

use super::intent::{new_idempotency_key, provisional_id, Intent, IntentKind, IntentState};
use super::store::{QueueError, QueueStore};

/// The three sides a divergence can resolve to. `NAMES` order is the
/// `--keep=` help order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Re-assert the local gesture on the server: `start_timer(switch: true)`
    /// for a session verb (the server's running session is stopped & saved
    /// first), `create_segment` for a stop's minutes.
    KeepLocal,
    /// The server session stands; the local timer intents move to
    /// [`IntentState::Parked`] — kept for review, never deleted.
    TakeServer,
    /// The server session stands *and* the local session is written as a
    /// segment via `create_segment`.
    KeepBoth,
}

impl Resolution {
    pub const NAMES: &'static [&'static str] = &["local", "server", "both"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeepLocal => "local",
            Self::TakeServer => "server",
            Self::KeepBoth => "both",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "local" => Some(Self::KeepLocal),
            "server" => Some(Self::TakeServer),
            "both" => Some(Self::KeepBoth),
            _ => None,
        }
    }
}

/// What a resolution did — every variant is a write or a keep, never a drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// Keep-local on a session verb: the server switched to the local session
    /// (its own running one was stopped & saved by `switch: true`); the
    /// diverged intent left the queue and the drain may continue behind it.
    SwitchedToLocal,
    /// Keep-local on a stop / keep-both: the local minutes landed on the wire
    /// as a segment.
    SegmentWritten {
        activity_id: i64,
        segment_id: i64,
        minutes: u32,
    },
    /// Take-server: this many local intents moved to `parked` — kept for
    /// review in `queue.json`, excluded from replay.
    Parked { count: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("intent #{0} is not waiting on a divergence")]
    NotDiverged(u64),
    /// The stored payload doesn't carry enough to compose this resolution —
    /// the recorded boundary of the generic-conflict fallback. The intent
    /// stays diverged; nothing is written or dropped.
    #[error("{0}")]
    CannotCompose(String),
    /// The saved editor buffer didn't parse back into a valid payload — the
    /// intent stays diverged, untouched, and the refusal names what's wrong.
    #[error("{0}")]
    EditRejected(String),
}

/// Apply `resolution` to the diverged intent `intent_id`. `cached` is the
/// last-known server snapshot (the read cache) — the local session's identity
/// when the diverged verb doesn't carry one itself. Callers continue the
/// drain after a successful keep-local/keep-both; take-server parks the whole
/// local session, so there is nothing left to drain behind it.
pub async fn resolve(
    api: &ApiClient,
    store: &QueueStore,
    cached: Option<&Timer>,
    intent_id: u64,
    resolution: Resolution,
    now: jiff::Timestamp,
) -> Result<Resolved, ResolveError> {
    let intents = store.intents()?;
    let Some(intent) = intents.iter().find(|i| i.id == intent_id) else {
        return Err(ResolveError::NotDiverged(intent_id));
    };
    let IntentState::Diverged {
        title,
        code,
        conflict,
        ..
    } = &intent.state
    else {
        return Err(ResolveError::NotDiverged(intent_id));
    };
    // `no-live-timer` is server proof the session is gone (ADR 0036) — the
    // compositions below key on it. An absent or unknown code keeps every
    // generic-fallback arm exactly as it was.
    let no_live_timer = code.as_deref() == Some(codes::NO_LIVE_TIMER);
    // The local session's remaining gestures: the diverged intent plus every
    // in-play intent queued behind it on the same stream (FIFO means nothing
    // pending can sit before the head that hit the wall).
    let group: Vec<&Intent> = intents
        .iter()
        .filter(|i| i.stream == intent.stream && i.id >= intent.id && !i.is_parked())
        .collect();

    // Only timer verbs compose a resolution today. A diverged plan write
    // (`activity_create`/`_update`/`_archive`) can still be parked via
    // take-server; keep-local/keep-both fall to the `other =>` refusals below —
    // richer activity divergence (declare/adjust/drop reconciliation) is Phase-3
    // (#110/#111), not this slice.
    match resolution {
        Resolution::TakeServer => park_group(store, &group, title),
        Resolution::KeepLocal => match &intent.kind {
            IntentKind::TimerStart { activity_id, .. } => {
                switch_to_local(api, store, intent.id, *activity_id).await
            }
            IntentKind::TimerPause { .. } | IntentKind::TimerResume { .. } if no_live_timer => {
                // The session is gone server-side: there is nothing to switch
                // away from, and a fresh `start_timer` would restart the local
                // clock at zero — a silent loss. The honest keep-local is the
                // minutes themselves: compose the local session and write it.
                write_local_session(api, store, cached, conflict, &group, now).await
            }
            IntentKind::TimerPause { .. } | IntentKind::TimerResume { .. } => {
                // The verb carries no session identity; the local session is
                // the cached snapshot's.
                switch_to_local(api, store, intent.id, cached.and_then(|t| t.activity_id)).await
            }
            IntentKind::TimerStop {
                at,
                local_elapsed_s,
            } => write_local_stop(api, store, cached, intent.id, *at, *local_elapsed_s).await,
            other => Err(ResolveError::CannotCompose(format!(
                "a diverged {} has no keep-local composition — the conflict payload carries nothing to re-assert; take server (park), or resolve it on the web",
                other.word()
            ))),
        },
        Resolution::KeepBoth => match &intent.kind {
            IntentKind::TimerStart { .. }
            | IntentKind::TimerPause { .. }
            | IntentKind::TimerResume { .. } => {
                write_local_session(api, store, cached, conflict, &group, now).await
            }
            IntentKind::TimerStop { .. } => Err(ResolveError::CannotCompose(
                "a diverged stop can't keep both — the conflict payload names no server segment to reconcile against; keep local writes your minutes, take server parks them".into(),
            )),
            other => Err(ResolveError::CannotCompose(format!(
                "a diverged {} has no keep-both composition — the conflict payload names no second session to keep; take server (park), or resolve it on the web",
                other.word()
            ))),
        },
    }
}

/// Take-server: move the whole group to `Parked` under the writer lock.
/// Nothing leaves the queue — kept for review is the whole point.
fn park_group(
    store: &QueueStore,
    group: &[&Intent],
    title: &str,
) -> Result<Resolved, ResolveError> {
    let ids: Vec<u64> = group.iter().map(|i| i.id).collect();
    let reason = format!("took server · {title}");
    let count = store.mutate(|doc| {
        let mut count = 0;
        for i in doc.intents_mut().iter_mut() {
            if ids.contains(&i.id) && !i.is_parked() {
                i.state = IntentState::Parked {
                    reason: reason.clone(),
                };
                count += 1;
            }
        }
        count
    })?;
    Ok(Resolved::Parked { count })
}

// ---------------------------------------------------------------------------
// The rejected write's gestures (§Diverged · rejected segment, #109):
// edit / drop / skip. All three act on a *diverged* intent only.
// ---------------------------------------------------------------------------

/// The editable representation the `$EDITOR` hand-off opens for a rejected
/// write — small `key: value` lines, seeded from the stored payload, with the
/// server's objection as a comment so the fix is made against it. `None` for
/// kinds that carry nothing time-shaped to edit (a diverged timer verb
/// resolves through the pick-a-side panel instead).
pub fn edit_seed(intent: &Intent) -> Option<String> {
    let objection = match &intent.state {
        IntentState::Diverged { status, title, .. } => format!("{status} {title}"),
        _ => return None,
    };
    let mut lines = vec![
        format!(
            "# Queued {} — the server refused it: {objection}.",
            intent.kind.word()
        ),
        "# Edit the values and save to retry; quit without saving to leave it as is.".to_string(),
    ];
    match &intent.kind {
        IntentKind::SegmentCreate {
            started_at,
            minutes,
            ..
        } => {
            lines.push(format!("started_at: {started_at}"));
            lines.push(format!("minutes: {minutes}"));
        }
        IntentKind::ActivityCreate { body } => {
            lines.push(format!("title: {}", body.title));
            if let Some(minutes) = body.duration_minutes {
                lines.push(format!("minutes: {minutes}"));
            }
            if let Some(day) = body.planned_on {
                lines.push(format!("planned_on: {day}"));
            }
        }
        _ => return None,
    }
    lines.push(String::new());
    Some(lines.join("\n"))
}

/// Parse a saved editor buffer back into the diverged intent's payload and
/// re-pend it, all under the writer lock: the corrected write rejoins the
/// queue as pending and the next drain retries it. The idempotency key is
/// re-minted — the edited payload is a *new* logical write, and re-sending a
/// different body under the old key would either replay the stored rejection
/// or trip the server's key/payload mismatch guard. A buffer that doesn't
/// parse refuses with [`ResolveError::EditRejected`] and changes nothing.
pub fn apply_edit(
    store: &QueueStore,
    intent_id: u64,
    buffer: &str,
) -> Result<Intent, ResolveError> {
    let fields = parse_edit_lines(buffer)?;
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let minutes = get("minutes")
        .map(|v| {
            v.parse::<u32>().ok().filter(|m| *m > 0).ok_or_else(|| {
                ResolveError::EditRejected(format!(
                    "minutes must be a whole number above zero, got \"{v}\""
                ))
            })
        })
        .transpose()?;
    let started_at = get("started_at")
        .map(|v| {
            v.parse::<jiff::Timestamp>().map_err(|_| {
                ResolveError::EditRejected(format!(
                    "started_at must be an RFC 3339 timestamp (e.g. 2026-07-15T14:02:00Z), got \"{v}\""
                ))
            })
        })
        .transpose()?;
    let planned_on = get("planned_on")
        .map(|v| {
            v.parse::<jiff::civil::Date>().map_err(|_| {
                ResolveError::EditRejected(format!(
                    "planned_on must be a calendar day (e.g. 2026-07-15), got \"{v}\""
                ))
            })
        })
        .transpose()?;
    let title = get("title").map(str::to_string);
    if title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err(ResolveError::EditRejected("title must not be empty".into()));
    }
    // A field the kind can't hold refuses rather than silently dropping — an
    // ignored `started_at` would retry the unchanged payload behind the
    // user's back.
    let refuse_foreign = |kind: &str, allowed: &[&str]| -> Result<(), ResolveError> {
        match fields.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            Some((k, _)) => Err(ResolveError::EditRejected(format!(
                "a queued {kind} has no \"{k}\" — editable fields: {}",
                allowed.join(", ")
            ))),
            None => Ok(()),
        }
    };

    let updated = store.mutate(|doc| -> Result<Intent, ResolveError> {
        let Some(intent) = doc.intents_mut().iter_mut().find(|i| i.id == intent_id) else {
            return Err(ResolveError::NotDiverged(intent_id));
        };
        if !intent.is_diverged() {
            return Err(ResolveError::NotDiverged(intent_id));
        }
        match &mut intent.kind {
            IntentKind::SegmentCreate {
                started_at: at,
                minutes: m,
                ..
            } => {
                refuse_foreign("log", &["started_at", "minutes"])?;
                if let Some(v) = started_at {
                    *at = v;
                }
                if let Some(v) = minutes {
                    *m = v;
                }
            }
            IntentKind::ActivityCreate { body } => {
                refuse_foreign("plan", &["title", "minutes", "planned_on"])?;
                if let Some(v) = title {
                    body.title = v;
                }
                if let Some(v) = minutes {
                    body.duration_minutes = Some(v);
                }
                if let Some(v) = planned_on {
                    body.planned_on = Some(v);
                }
            }
            other => {
                return Err(ResolveError::EditRejected(format!(
                    "a diverged {} has nothing editable — resolve it with keep local/server/both instead",
                    other.word()
                )))
            }
        }
        intent.state = IntentState::Pending;
        intent.last_error = None;
        intent.idempotency_key = new_idempotency_key();
        Ok(intent.clone())
    })??;
    Ok(updated)
}

/// Drop a diverged intent — the queue's one genuinely user-chosen delete,
/// distinct from [`IntentState::Parked`] (which keeps). The *caller* owns the
/// confirmation (the TUI's second `x`, the CLI's `--force`); this function is
/// the final act. Refuses while still-queued intents reference the intent's
/// provisional id — dropping the parent create would orphan them against an
/// id that will never exist. Returns the dropped record so the surface can
/// say exactly what left.
pub fn drop_intent(store: &QueueStore, intent_id: u64) -> Result<Intent, ResolveError> {
    store.mutate(|doc| -> Result<Intent, ResolveError> {
        let Some(intent) = doc.intents().iter().find(|i| i.id == intent_id).cloned() else {
            return Err(ResolveError::NotDiverged(intent_id));
        };
        if !intent.is_diverged() {
            return Err(ResolveError::NotDiverged(intent_id));
        }
        let prov = provisional_id(intent.id);
        let dependents = doc
            .intents()
            .iter()
            .filter(|i| references_activity(&i.kind, prov))
            .count();
        if dependents > 0 {
            return Err(ResolveError::CannotCompose(format!(
                "{dependents} queued write{} still reference{} this provisional activity — edit it instead, or resolve on the web; dropping it would orphan them",
                if dependents == 1 { "" } else { "s" },
                if dependents == 1 { "s" } else { "" },
            )));
        }
        doc.intents_mut().retain(|i| i.id != intent_id);
        Ok(intent)
    })?
}

/// Skip a diverged intent: park it (reason `skipped · <the objection>`), kept
/// in `queue.json` for a later decision, excluded from replay — so its stream
/// unblocks without anything being written or dropped.
pub fn skip_intent(store: &QueueStore, intent_id: u64) -> Result<Intent, ResolveError> {
    store.mutate(|doc| -> Result<Intent, ResolveError> {
        let Some(intent) = doc.intents_mut().iter_mut().find(|i| i.id == intent_id) else {
            return Err(ResolveError::NotDiverged(intent_id));
        };
        let IntentState::Diverged { title, .. } = &intent.state else {
            return Err(ResolveError::NotDiverged(intent_id));
        };
        intent.state = IntentState::Parked {
            reason: format!("skipped · {title}"),
        };
        Ok(intent.clone())
    })?
}

/// Does this kind reference the given (provisional) activity id?
fn references_activity(kind: &IntentKind, activity_id: i64) -> bool {
    match kind {
        IntentKind::SegmentCreate {
            activity_id: id, ..
        }
        | IntentKind::ActivityUpdate { id, .. }
        | IntentKind::ActivityArchive { id } => *id == activity_id,
        _ => false,
    }
}

/// Split a saved buffer into `key: value` pairs, skipping blank lines and `#`
/// comments. An unrecognised line refuses loudly — a silent skip could eat a
/// typo'd `minutes` and retry the unchanged payload.
fn parse_edit_lines(buffer: &str) -> Result<Vec<(String, String)>, ResolveError> {
    const KEYS: [&str; 4] = ["started_at", "minutes", "title", "planned_on"];
    let mut fields = Vec::new();
    for line in buffer.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(ResolveError::EditRejected(format!(
                "unrecognised line \"{line}\" — expected `key: value`"
            )));
        };
        let key = key.trim();
        if !KEYS.contains(&key) {
            return Err(ResolveError::EditRejected(format!(
                "unknown field \"{key}\" — editable fields: {}",
                KEYS.join(", ")
            )));
        }
        fields.push((key.to_string(), value.trim().to_string()));
    }
    if fields.is_empty() {
        return Err(ResolveError::EditRejected(
            "the buffer names no fields — nothing to retry".into(),
        ));
    }
    Ok(fields)
}

/// Keep-local on a session verb: `start_timer(activity_id, switch: true)` —
/// the server stops & saves its running session and the local one takes over.
/// Only after the server acknowledges does the diverged intent leave the
/// queue; the pending intents behind it stay and replay on the next drain.
async fn switch_to_local(
    api: &ApiClient,
    store: &QueueStore,
    intent_id: u64,
    activity_id: Option<i64>,
) -> Result<Resolved, ResolveError> {
    api.start_timer(activity_id, true).await?;
    store.mutate(|doc| doc.intents_mut().retain(|i| i.id != intent_id))?;
    Ok(Resolved::SwitchedToLocal)
}

/// Keep-local on a diverged stop: the local minutes become an explicit
/// `create_segment` — the honest composition either way: a `no-live-timer`
/// stop has no server segment to `update_segment` against (the session is
/// gone), and none of the shipped conflict codes carries a segment id for the
/// drift composition (ADR 0036), so the write is always a fresh segment.
async fn write_local_stop(
    api: &ApiClient,
    store: &QueueStore,
    cached: Option<&Timer>,
    intent_id: u64,
    at: jiff::Timestamp,
    local_elapsed_s: i64,
) -> Result<Resolved, ResolveError> {
    let Some(activity_id) = cached.and_then(|t| t.activity_id) else {
        return Err(ResolveError::CannotCompose(
            "the stopped session's activity is unknown — the conflict payload names none and no cached snapshot holds it; take server (park), or resolve it on the web".into(),
        ));
    };
    let started_at =
        jiff::Timestamp::from_second(at.as_second() - local_elapsed_s.max(0)).unwrap_or(at);
    let minutes = to_minutes(local_elapsed_s);
    let segment = api.create_segment(activity_id, started_at, minutes).await?;
    store.mutate(|doc| doc.intents_mut().retain(|i| i.id != intent_id))?;
    Ok(Resolved::SegmentWritten {
        activity_id,
        segment_id: segment.id,
        minutes,
    })
}

/// Write the local session as a segment (keep-both, and keep-local on a
/// `no-live-timer` session verb): compose it from what is actually stored —
/// the group's own verbs, seeded from a queued start, the cached snapshot, or
/// the coded conflict's `current` — and write it via `create_segment`. Any
/// server session is untouched. The whole group leaves the queue only after
/// the write lands: its outcome now lives in the segment.
async fn write_local_session(
    api: &ApiClient,
    store: &QueueStore,
    cached: Option<&Timer>,
    conflict: &ConflictInfo,
    group: &[&Intent],
    now: jiff::Timestamp,
) -> Result<Resolved, ResolveError> {
    let (activity_id, started_at, elapsed_s) = compose_local_session(cached, conflict, group, now)?;
    let minutes = to_minutes(elapsed_s);
    let segment = api.create_segment(activity_id, started_at, minutes).await?;
    let ids: Vec<u64> = group.iter().map(|i| i.id).collect();
    store.mutate(|doc| doc.intents_mut().retain(|i| !ids.contains(&i.id)))?;
    Ok(Resolved::SegmentWritten {
        activity_id,
        segment_id: segment.id,
        minutes,
    })
}

/// The local session as the stored payload supports composing it:
/// `activity_id` from a queued start/bind (else the cached snapshot, else the
/// coded conflict's `current.activity_id`), `started_at` from the queued start
/// (else the cached anchor), elapsed from a queued stop's `local_elapsed_s`
/// (else the group's pause/resume folded over the seed via `timer_clock`,
/// materialized at `now`). Anything less refuses — the recorded boundary,
/// never a guess.
fn compose_local_session(
    cached: Option<&Timer>,
    conflict: &ConflictInfo,
    group: &[&Intent],
    now: jiff::Timestamp,
) -> Result<(i64, jiff::Timestamp, i64), ResolveError> {
    let activity_id = group
        .iter()
        .find_map(|i| match &i.kind {
            IntentKind::TimerStart { activity_id, .. } | IntentKind::TimerBind { activity_id, .. } => {
                *activity_id
            }
            _ => None,
        })
        .or_else(|| cached.and_then(|t| t.activity_id))
        // Last resort, `timer-already-running` only: the server session's
        // activity. Not a guess — the panel showed that session (label and
        // all) before the user chose to keep both, so the choice was made
        // against exactly this anchor (the #106 boundary this dissolves).
        .or_else(|| conflict.current.as_ref().and_then(|c| c.activity_id))
        .ok_or_else(|| {
            ResolveError::CannotCompose(
                "the local session is unbound and the conflict payload names no activity — nothing to write its segment on; take server (park), or resolve it on the web".into(),
            )
        })?;

    // Seed the clock: a queued start anchors the local session; otherwise it
    // predates the queue and the cached snapshot's anchor is the local truth.
    let mut timer = match group.iter().find_map(|i| match &i.kind {
        IntentKind::TimerStart { at, .. } => Some(*at),
        _ => None,
    }) {
        Some(at) => timer_clock::apply_start(Some(activity_id), None, at),
        None => cached
            .filter(|t| t.running && t.started_at.is_some())
            .cloned()
            .ok_or_else(|| {
                ResolveError::CannotCompose(
                    "the local session has no anchor — no queued start and no cached running snapshot to compose from; take server (park), or resolve it on the web".into(),
                )
            })?,
    };
    let started_at = timer
        .started_at
        .expect("both seed arms guarantee an anchor");

    // Fold the group's clock verbs; a queued stop's own local_elapsed_s is
    // the gestured truth and wins over recomputing it.
    let mut stopped_elapsed: Option<i64> = None;
    for intent in group {
        match &intent.kind {
            IntentKind::TimerPause { at } => timer = timer_clock::apply_pause(timer, *at),
            IntentKind::TimerResume { at } => timer = timer_clock::apply_resume(timer, *at),
            IntentKind::TimerStop {
                local_elapsed_s, ..
            } => stopped_elapsed = Some(*local_elapsed_s),
            IntentKind::TimerDiscard => {
                return Err(ResolveError::CannotCompose(
                    "the local session ends in a queued discard — nothing to keep; take server (park) instead".into(),
                ));
            }
            IntentKind::TimerStart { .. } | IntentKind::TimerBind { .. } => {}
            // A timer-stream group never holds a plan write or a week note
            // (those key on the `activity`/`activity:<id>`/`week:<iso>` streams),
            // so this is unreachable — the match stays exhaustive over `IntentKind`.
            IntentKind::ActivityCreate { .. }
            | IntentKind::ActivityUpdate { .. }
            | IntentKind::ActivityArchive { .. }
            | IntentKind::SegmentCreate { .. }
            | IntentKind::WeekNoteWrite { .. }
            | IntentKind::TargetCreate { .. }
            | IntentKind::TargetAdjust { .. }
            | IntentKind::TargetRetire { .. }
            | IntentKind::NoteCreate { .. } => {
                unreachable!("a non-timer write never shares a timer session's stream")
            }
        }
    }
    let elapsed_s = stopped_elapsed.unwrap_or_else(|| timer_clock::elapsed(&timer, now));
    Ok((activity_id, started_at, elapsed_s))
}

/// Whole minutes, the same nearest-minute rounding `timer_clock::apply_stop`
/// uses for the confirmation line.
fn to_minutes(elapsed_s: i64) -> u32 {
    ((elapsed_s.max(0) + 30) / 60) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::FieldError;
    use url::Url;
    use wiremock::matchers::{body_json, body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tmp_store(tag: &str) -> QueueStore {
        let dir =
            std::env::temp_dir().join(format!("engineer-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        QueueStore::at(dir.join("queue.json"))
    }

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn now() -> jiff::Timestamp {
        ts("2026-07-15T10:00:00Z")
    }

    fn diverge(store: &QueueStore, id: u64) {
        diverge_as(store, id, 409, "Conflict", None, ConflictInfo::default());
    }

    /// Diverge with a coded conflict — the enriched contract (engineer#806).
    fn diverge_as(
        store: &QueueStore,
        id: u64,
        status: u16,
        title: &str,
        code: Option<&str>,
        conflict: ConflictInfo,
    ) {
        store
            .mutate(|doc| {
                if let Some(i) = doc.intents_mut().iter_mut().find(|i| i.id == id) {
                    i.state = IntentState::Diverged {
                        status,
                        title: title.into(),
                        detail: "a timer is already running".into(),
                        type_uri: None,
                        errors: Vec::<FieldError>::new(),
                        code: code.map(Into::into),
                        conflict: Box::new(conflict),
                    };
                }
            })
            .unwrap();
    }

    fn no_live_timer(store: &QueueStore, id: u64) {
        diverge_as(
            store,
            id,
            404,
            "No running timer",
            Some(codes::NO_LIVE_TIMER),
            ConflictInfo::default(),
        );
    }

    fn cached_running() -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "activity_id": 9, "label": "systems",
            "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 0
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn keep_local_on_a_diverged_start_switches_and_removes_only_the_head() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(
                serde_json::json!({ "activity_id": 9, "switch": true }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-local-start");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        let behind = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        diverge(&store, start.id);

        let resolved = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(resolved, Resolved::SwitchedToLocal);

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1, "the diverged head left the queue");
        assert_eq!(intents[0].id, behind.id);
        assert!(
            intents[0].is_pending(),
            "the rest stays pending — the continued drain replays it"
        );
    }

    #[tokio::test]
    async fn keep_local_on_a_diverged_pause_switches_to_the_cached_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(
                serde_json::json!({ "activity_id": 9, "switch": true }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-local-pause");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        diverge(&store, pause.id);

        let cached = cached_running();
        let resolved = resolve(
            &client(&server),
            &store,
            Some(&cached),
            pause.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(resolved, Resolved::SwitchedToLocal);
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn take_server_parks_the_whole_local_session_and_deletes_nothing() {
        let server = MockServer::start().await;
        // Take-server writes nothing and the parked intents never replay.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("take-server");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        diverge(&store, start.id);

        let resolved = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::TakeServer,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(resolved, Resolved::Parked { count: 2 });

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 2, "never deleted — kept for review");
        assert!(intents.iter().all(Intent::is_parked));
        match &intents[0].state {
            IntentState::Parked { reason } => {
                assert!(reason.contains("took server"), "{reason}");
                assert!(reason.contains("Conflict"), "the objection rides along");
            }
            other => panic!("expected parked, got {other:?}"),
        }
        assert!(store.pending().unwrap().is_empty(), "excluded from replay");

        // The proof the deliverable asks for: a later drain replays nothing.
        let report = super::super::replay::drain(&client(&server), &store)
            .await
            .unwrap();
        assert_eq!(report.replayed, 0, "a parked intent never replays");
        assert_eq!(report.remaining, 0);
    }

    #[tokio::test]
    async fn keep_both_writes_the_composed_session_from_start_and_stop() {
        let server = MockServer::start().await;
        // started_at from the queued start; minutes from the queued stop's
        // local_elapsed_s (2832s ≈ 47m) — asserted on the wire.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": {
                    "started_at": "2026-07-15T09:13:00Z",
                    "duration_minutes": 47
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 9, "minutes": 47
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-both");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerStop {
                at: ts("2026-07-15T10:00:12Z"),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge(&store, start.id);

        let resolved = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::KeepBoth,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(
            resolved,
            Resolved::SegmentWritten {
                activity_id: 9,
                segment_id: 88,
                minutes: 47
            }
        );
        assert!(
            store.intents().unwrap().is_empty(),
            "the written segment carries the whole session — the group leaves"
        );
    }

    #[tokio::test]
    async fn keep_both_on_a_still_running_session_materializes_elapsed_at_now() {
        let server = MockServer::start().await;
        // Started 09:13, paused 09:30→09:40 (10m banked), now 10:00 → 37m.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": { "duration_minutes": 37 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 89, "activity_id": 9, "minutes": 37
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-both-running");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:30:00Z"),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerResume {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        diverge(&store, start.id);

        let resolved = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::KeepBoth,
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(
            resolved,
            Resolved::SegmentWritten { minutes: 37, .. }
        ));
    }

    #[tokio::test]
    async fn keep_local_on_a_diverged_stop_writes_the_minutes_as_a_segment() {
        let server = MockServer::start().await;
        // 2832s back from the stop moment 10:00:12 → started 09:13:00, 47m.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": {
                    "started_at": "2026-07-15T09:13:00Z",
                    "duration_minutes": 47
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 90, "activity_id": 9, "minutes": 47
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-local-stop");
        let stop = store
            .enqueue(IntentKind::TimerStop {
                at: ts("2026-07-15T10:00:12Z"),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge(&store, stop.id);

        let cached = cached_running();
        let resolved = resolve(
            &client(&server),
            &store,
            Some(&cached),
            stop.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(
            resolved,
            Resolved::SegmentWritten {
                segment_id: 90,
                minutes: 47,
                ..
            }
        ));
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn keep_local_on_a_no_live_timer_pause_writes_the_minutes_instead_of_switching() {
        let server = MockServer::start().await;
        // The session is gone server-side (coded proof) — keep-local composes
        // the local session and writes it: cached anchor 09:00, queued pause
        // at 09:30 freezes the clock → 30m. No start_timer call: there is no
        // session to switch away from, and a fresh start would zero the clock.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": {
                    "started_at": "2026-07-15T09:00:00Z",
                    "duration_minutes": 30
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 91, "activity_id": 9, "minutes": 30
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("keep-local-gone-pause");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:30:00Z"),
            })
            .unwrap();
        no_live_timer(&store, pause.id);

        let cached = cached_running();
        let resolved = resolve(
            &client(&server),
            &store,
            Some(&cached),
            pause.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap();
        assert!(
            matches!(
                resolved,
                Resolved::SegmentWritten {
                    activity_id: 9,
                    segment_id: 91,
                    minutes: 30
                }
            ),
            "got {resolved:?}"
        );
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn keep_local_on_a_no_live_timer_pause_without_an_anchor_refuses_loudly() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = tmp_store("keep-local-gone-noanchor");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:30:00Z"),
            })
            .unwrap();
        no_live_timer(&store, pause.id);

        // No cached snapshot: nothing names the gone session's activity, so
        // there is nothing to write the composed segment on.
        let err = resolve(
            &client(&server),
            &store,
            None,
            pause.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ResolveError::CannotCompose(_)), "{err}");
        assert!(
            err.to_string().contains("names no activity"),
            "the refusal names what's missing: {err}"
        );
        assert!(
            store.intents().unwrap()[0].is_diverged(),
            "kept, not dropped"
        );
    }

    #[tokio::test]
    async fn keep_both_on_an_unbound_start_anchors_on_the_conflicts_current_activity() {
        let server = MockServer::start().await;
        // The #106 boundary this ticket dissolves: an unbound local start used
        // to refuse keep-both ("no activity to write its segment on"); the
        // coded conflict's `current.activity_id` now anchors it.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/42/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": {
                    "started_at": "2026-07-15T09:13:00Z",
                    "duration_minutes": 47
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 92, "activity_id": 42, "minutes": 47
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("keep-both-current-anchor");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: None,
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerStop {
                at: ts("2026-07-15T10:00:12Z"),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge_as(
            &store,
            start.id,
            409,
            "Timer already running",
            Some(codes::TIMER_ALREADY_RUNNING),
            serde_json::from_value(serde_json::json!({
                "current": {
                    "id": 114, "activity_id": 42, "label": "Ruby OOP Study",
                    "started_at": "2026-07-15T08:59:03Z", "paused": false
                },
                "resolutions": ["switch", "keep-remote"]
            }))
            .unwrap(),
        );

        let resolved = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::KeepBoth,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(
            resolved,
            Resolved::SegmentWritten {
                activity_id: 42,
                segment_id: 92,
                minutes: 47
            }
        );
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn compositions_the_payload_cannot_support_refuse_and_keep_the_intent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let api = client(&server);

        // keep-both on a diverged stop: one segment at stake, no second side.
        let store = tmp_store("cannot-both-stop");
        let stop = store
            .enqueue(IntentKind::TimerStop {
                at: ts("2026-07-15T10:00:12Z"),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge(&store, stop.id);
        let err = resolve(&api, &store, None, stop.id, Resolution::KeepBoth, now())
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::CannotCompose(_)), "{err}");
        assert!(
            err.to_string().contains("no server segment"),
            "the refusal names the missing piece: {err}"
        );
        assert!(store.intents().unwrap()[0].is_diverged(), "still diverged");

        // keep-local on a diverged stop with no cached activity: nothing to
        // write the segment on.
        let err = resolve(&api, &store, None, stop.id, Resolution::KeepLocal, now())
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::CannotCompose(_)), "{err}");
        assert!(
            err.to_string().contains("names none"),
            "the refusal names the missing piece: {err}"
        );
        assert!(
            store.intents().unwrap()[0].is_diverged(),
            "kept, not dropped"
        );

        // keep-both on an unbound local session: no activity anywhere — not
        // even a coded conflict snapshot to anchor on.
        let store = tmp_store("cannot-both-unbound");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: None,
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        diverge(&store, start.id);
        let err = resolve(&api, &store, None, start.id, Resolution::KeepBoth, now())
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::CannotCompose(_)), "{err}");
        assert!(
            err.to_string().contains("names no activity"),
            "the refusal names the missing piece: {err}"
        );
        assert_eq!(store.intents().unwrap().len(), 1, "kept, not dropped");
    }

    #[tokio::test]
    async fn a_pending_or_unknown_intent_is_not_resolvable() {
        let server = MockServer::start().await;
        let store = tmp_store("not-diverged");
        let pending = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();

        let err = resolve(
            &client(&server),
            &store,
            None,
            pending.id,
            Resolution::TakeServer,
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ResolveError::NotDiverged(_)), "{err}");

        let err = resolve(
            &client(&server),
            &store,
            None,
            999,
            Resolution::TakeServer,
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ResolveError::NotDiverged(999)), "{err}");
    }

    // --- the rejected write's gestures (#109): edit / drop / skip -----------

    /// A diverged `SegmentCreate` — the §Diverged · rejected segment case.
    fn seeded_rejected_segment(tag: &str) -> (QueueStore, Intent) {
        let store = tmp_store(tag);
        let seg = store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: ts("2026-07-15T14:02:00Z"),
                minutes: 45,
            })
            .unwrap();
        diverge_as(
            &store,
            seg.id,
            422,
            "Segment overlaps",
            None,
            ConflictInfo::default(),
        );
        (store, seg)
    }

    #[test]
    fn edit_seed_names_the_objection_and_the_editable_times() {
        let (store, seg) = seeded_rejected_segment("edit-seed");
        let intent = store.intents().unwrap().remove(0);
        let seed = edit_seed(&intent).expect("a rejected segment is editable");
        assert!(seed.contains("422 Segment overlaps"), "{seed}");
        assert!(seed.contains("started_at: 2026-07-15T14:02:00Z"), "{seed}");
        assert!(seed.contains("minutes: 45"), "{seed}");
        let _ = seg;

        // A diverged timer verb has nothing time-shaped to edit.
        let store = tmp_store("edit-seed-timer");
        let pause = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        diverge(&store, pause.id);
        assert!(edit_seed(&store.intents().unwrap()[0]).is_none());
    }

    /// The edit-retry round-trip: the corrected payload re-pends under the
    /// writer lock with a **fresh** idempotency key, and the next drain lands
    /// it — asserted on the wire, corrected times and all.
    #[tokio::test]
    async fn apply_edit_repends_the_corrected_segment_and_the_drain_retries_it() {
        let (store, seg) = seeded_rejected_segment("edit-retry");

        let updated = apply_edit(
            &store,
            seg.id,
            "# comment survives\nstarted_at: 2026-07-15T15:10:00Z\nminutes: 30\n",
        )
        .unwrap();
        assert!(updated.is_pending(), "back in the replay line");
        assert_ne!(
            updated.idempotency_key, seg.idempotency_key,
            "the edited payload is a new logical write — never the old key"
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(serde_json::json!({
                "segment": { "started_at": "2026-07-15T15:10:00Z", "duration_minutes": 30 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 9, "minutes": 30
            })))
            .expect(1)
            .mount(&server)
            .await;

        let report = super::super::replay::drain(&client(&server), &store)
            .await
            .unwrap();
        assert_eq!(report.replayed, 1, "the corrected write landed");
        assert!(store.intents().unwrap().is_empty());
    }

    #[test]
    fn apply_edit_refuses_bad_buffers_and_changes_nothing() {
        let (store, seg) = seeded_rejected_segment("edit-refuse");
        for (buffer, names) in [
            ("minutes: 0", "above zero"),
            ("minutes: soon", "above zero"),
            ("started_at: 14:02", "RFC 3339"),
            ("nonsense line", "key: value"),
            ("elapsed: 45", "unknown field"),
            ("title: Raft", "no \"title\""), // a segment has no title line
            ("# only comments\n", "nothing to retry"),
        ] {
            let err = apply_edit(&store, seg.id, buffer).unwrap_err();
            assert!(
                matches!(err, ResolveError::EditRejected(_)),
                "{buffer}: {err}"
            );
            assert!(err.to_string().contains(names), "{buffer}: {err}");
            assert!(
                store.intents().unwrap()[0].is_diverged(),
                "kept diverged, untouched"
            );
        }

        // A pending intent is not editable — edit is a divergence gesture.
        let pending = store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: ts("2026-07-15T16:00:00Z"),
                minutes: 10,
            })
            .unwrap();
        assert!(matches!(
            apply_edit(&store, pending.id, "minutes: 20"),
            Err(ResolveError::NotDiverged(_))
        ));
    }

    #[test]
    fn apply_edit_reshapes_a_rejected_activity_create() {
        use crate::api::ActivityCreate;
        let store = tmp_store("edit-create");
        let create = store
            .enqueue(IntentKind::ActivityCreate {
                body: ActivityCreate {
                    title: "Raft leader election".into(),
                    duration_minutes: Some(20),
                    planned_on: Some("2026-07-14".parse().unwrap()),
                    ..Default::default()
                },
            })
            .unwrap();
        diverge_as(
            &store,
            create.id,
            422,
            "Study day is closed",
            None,
            ConflictInfo::default(),
        );

        // The closed-day fix: move the day, trim the minutes.
        apply_edit(&store, create.id, "planned_on: 2026-07-15\nminutes: 25").unwrap();
        let intents = store.intents().unwrap();
        assert!(intents[0].is_pending());
        match &intents[0].kind {
            IntentKind::ActivityCreate { body } => {
                assert_eq!(body.planned_on, Some("2026-07-15".parse().unwrap()));
                assert_eq!(body.duration_minutes, Some(25));
                assert_eq!(body.title, "Raft leader election", "untouched fields keep");
            }
            other => panic!("expected the create, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drop_is_explicit_removes_the_intent_and_unblocks_its_stream() {
        let (store, seg) = seeded_rejected_segment("drop");
        // A pending edit behind the diverged segment on the same stream.
        store
            .enqueue(IntentKind::ActivityUpdate {
                id: 9,
                title: "revised".into(),
            })
            .unwrap();

        let dropped = drop_intent(&store, seg.id).unwrap();
        assert_eq!(dropped.id, seg.id, "the surface can say what left");
        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1, "gone — the one user-chosen delete");
        assert!(intents[0].is_pending(), "the stream is unblocked");

        // The unblocked stream drains behind the choice.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 9, "title": "revised", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let report = super::super::replay::drain(&client(&server), &store)
            .await
            .unwrap();
        assert_eq!(report.replayed, 1);
        assert!(!report.diverged);
    }

    #[test]
    fn drop_refuses_a_pending_intent_and_a_parent_with_dependents() {
        // Pending: not a divergence choice.
        let store = tmp_store("drop-refuse");
        let pending = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        assert!(matches!(
            drop_intent(&store, pending.id),
            Err(ResolveError::NotDiverged(_))
        ));

        // A diverged create with a queued segment referencing its provisional
        // id: dropping the parent would orphan the segment forever — refuse,
        // naming the way out.
        use crate::api::ActivityCreate;
        let store = tmp_store("drop-orphan");
        let create = store
            .enqueue(IntentKind::ActivityCreate {
                body: ActivityCreate {
                    title: "Raft".into(),
                    duration_minutes: Some(20),
                    ..Default::default()
                },
            })
            .unwrap();
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: -(create.id as i64),
                started_at: ts("2026-07-15T14:02:00Z"),
                minutes: 15,
            })
            .unwrap();
        diverge(&store, create.id);
        let err = drop_intent(&store, create.id).unwrap_err();
        assert!(matches!(err, ResolveError::CannotCompose(_)), "{err}");
        assert!(err.to_string().contains("orphan"), "{err}");
        assert_eq!(store.intents().unwrap().len(), 2, "nothing left the queue");
    }

    #[tokio::test]
    async fn skip_parks_as_skipped_and_it_never_replays() {
        let (store, seg) = seeded_rejected_segment("skip");
        let skipped = skip_intent(&store, seg.id).unwrap();
        match &skipped.state {
            IntentState::Parked { reason } => {
                assert!(reason.starts_with("skipped"), "{reason}");
                assert!(
                    reason.contains("Segment overlaps"),
                    "the objection rides along: {reason}"
                );
            }
            other => panic!("expected parked, got {other:?}"),
        }

        // Kept in the queue, out of the replay line — a drain touches nothing.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let report = super::super::replay::drain(&client(&server), &store)
            .await
            .unwrap();
        assert_eq!(report.replayed, 0, "a skipped intent never replays");
        assert_eq!(report.remaining, 0, "…and never counts as waiting");
        assert!(!report.diverged);
        assert_eq!(store.intents().unwrap().len(), 1, "kept, never deleted");

        // Skip is a divergence gesture too.
        let pending = store
            .enqueue(IntentKind::TimerPause {
                at: ts("2026-07-15T09:40:00Z"),
            })
            .unwrap();
        assert!(matches!(
            skip_intent(&store, pending.id),
            Err(ResolveError::NotDiverged(_))
        ));
    }

    #[tokio::test]
    async fn a_server_refusal_mid_resolve_keeps_the_intent_diverged() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Unprocessable", "detail": "no"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let store = tmp_store("refused-switch");
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: ts("2026-07-15T09:13:00Z"),
            })
            .unwrap();
        diverge(&store, start.id);

        let err = resolve(
            &client(&server),
            &store,
            None,
            start.id,
            Resolution::KeepLocal,
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ResolveError::Api(ApiError::Problem { .. })));
        assert!(
            store.intents().unwrap()[0].is_diverged(),
            "an unacknowledged resolution changes nothing — loud, not lossy"
        );
    }
}
