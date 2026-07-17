//! Headless `engineer queue` — the offline write queue, observable
//! (offline-write.brief.md §8 Foundation; the §Queue inspector / headless-twin
//! boards). The bare read prints one row per unsynced intent; `sync` runs a
//! replay pass now; `resolve` picks a side on a waiting divergence — the
//! headless twin of the Timer screen's reconcile panel. A look and a nudge —
//! not a sync manager.
//!
//! Exit codes answer "does the queue need me?": 0 drained, empty, or only
//! parked-for-review · 3 writes queued, offline (deliberately not a failure —
//! a cron job must not page on a tunnel) · 4 a divergence is waiting on a
//! choice · 5 the replay itself failed (non-transport, non-problem — e.g.
//! queue io). `resolve` exits 0 on success and 1 when the resolution can't
//! apply (unknown id, not a divergence, a composition the stored payload
//! can't support, an editor buffer that doesn't parse, or offline where the
//! wire is needed). The rejected-write gestures (#109, §Diverged · rejected
//! segment) ride the same verb: `--edit` opens the payload in `$EDITOR` and
//! retries (exit 4 when the server still refuses), `--drop --force` is the
//! explicit discard, `--skip` parks it for later. Output is plain when
//! piped: ANSI colour is applied only on a TTY and never when NO_COLOR is
//! set.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;

use crate::api::{ApiClient, ApiError, Timer};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::queue::{self, Intent, IntentState, QueueStore, Resolution, Resolved};

#[derive(Args)]
pub struct QueueArgs {
    /// Emit JSON instead of the human table (valid on the bare read and `sync`).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Option<QueueCmd>,
}

#[derive(Subcommand)]
enum QueueCmd {
    /// Replay the queue now — pending intents re-send in order.
    Sync,
    /// Resolve a waiting divergence — pick a side, or (for a rejected
    /// segment/log) edit, drop, or skip it; nothing is ever dropped silently.
    Resolve {
        /// The intent id from the bare read's `#` column.
        id: u64,
        /// local: re-assert your session/segment on the server · server: park
        /// the local intents for review (never deleted) · both: write the
        /// local session as a segment and let the server session stand.
        #[arg(long, value_parser = Resolution::NAMES.to_vec(),
              conflicts_with_all = ["edit", "drop", "skip"])]
        keep: Option<String>,
        /// Open the rejected write's payload in $EDITOR (its times/minutes
        /// lines), then retry the replay with the corrected values.
        #[arg(long, conflicts_with_all = ["drop", "skip"])]
        edit: bool,
        /// Drop the rejected write — explicit and final, nothing is written.
        /// Requires --force: dropping discards the queued gesture forever.
        #[arg(long, conflicts_with = "skip")]
        drop: bool,
        /// Confirm --drop.
        #[arg(long, requires = "drop")]
        force: bool,
        /// Skip it for now — parked in the queue for review, excluded from
        /// replay; the stream behind it keeps syncing.
        #[arg(long)]
        skip: bool,
    },
}

pub async fn run(cfg: &Config, args: QueueArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let store = QueueStore::open_default().map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // The last-known server snapshot — the local session's identity when a
    // diverged verb doesn't carry one (resolve's keep-local/keep-both).
    let cached = crate::timer_cache::load().map(|s| s.timer);

    let editor = crate::editor::resolve_editor();
    let outcome = dispatch(&api, &store, cached, args.cmd, args.json, colored, &editor).await?;
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

/// A resolve that can't apply — unknown id, not a divergence, an unsupported
/// composition, or offline. The refusal contract `engineer timer` also uses.
const EXIT_REFUSED: i32 = 1;
/// Queued-offline: writes are waiting, which is a state, not a failure.
const EXIT_QUEUED: i32 = 3;
/// A divergence waits on a human choice.
const EXIT_DIVERGED: i32 = 4;
/// The replay itself failed — non-transport, non-problem (queue io, auth, …).
const EXIT_FAILED: i32 = 5;

struct Outcome {
    out: Vec<String>,
    err: Vec<String>,
    code: i32,
}

impl Outcome {
    fn ok(line: impl Into<String>) -> Self {
        Self {
            out: vec![line.into()],
            err: vec![],
            code: 0,
        }
    }

    fn refuse(reason: impl Into<String>) -> Self {
        Self {
            out: vec![],
            err: vec![reason.into()],
            code: EXIT_REFUSED,
        }
    }

    fn fail(reason: impl Into<String>) -> Self {
        Self {
            out: vec![],
            err: vec![reason.into()],
            code: EXIT_FAILED,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    api: &ApiClient,
    store: &QueueStore,
    cached: Option<Timer>,
    cmd: Option<QueueCmd>,
    json: bool,
    colored: bool,
    editor: &str,
) -> Result<Outcome, ApiError> {
    Ok(match cmd {
        None => read(store, json, colored),
        Some(QueueCmd::Sync) => sync(api, store, json, colored).await,
        Some(QueueCmd::Resolve {
            id,
            keep,
            edit,
            drop,
            force,
            skip,
        }) => match (keep, edit, drop, skip) {
            (Some(keep), false, false, false) => {
                resolve_cmd(api, store, cached.as_ref(), id, &keep, json, colored).await
            }
            (None, true, false, false) => edit_cmd(api, store, id, json, colored, editor).await,
            (None, false, true, false) => drop_cmd(api, store, id, force, json, colored).await,
            (None, false, false, true) => skip_cmd(api, store, id, json, colored).await,
            _ => Outcome::refuse(
                "pick exactly one resolution: --keep=local|server|both, --edit, --drop, or --skip",
            ),
        },
    })
}

// ---------------------------------------------------------------- read

fn read(store: &QueueStore, json: bool, colored: bool) -> Outcome {
    // A queue that can't be read is the loud case (unlike the read cache):
    // silently rendering "empty" over stuck intents would hide lost writes.
    let intents = match store.intents() {
        Ok(intents) => intents,
        Err(e) => return Outcome::fail(e.to_string()),
    };
    let now = jiff::Timestamp::now().as_second();
    let code = exit_for(&intents);

    if json {
        return Outcome {
            out: vec![json_read(&intents, now).to_string()],
            err: vec![],
            code,
        };
    }
    if intents.is_empty() {
        return Outcome::ok("queue empty");
    }

    let mut out = vec![paint(
        &format!(
            "{:<5} {:<8} {:<12} {:<6} STATE",
            "#", "INTENT", "TARGET", "AGE"
        ),
        COLOR_MUTED,
        colored,
    )];
    for i in &intents {
        let state = match &i.state {
            IntentState::Pending => paint("pending", COLOR_QUEUED, colored),
            IntentState::Diverged { .. } => paint("diverged", COLOR_DIVERGED, colored),
            IntentState::Parked { .. } => paint("parked", COLOR_MUTED, colored),
        };
        out.push(format!(
            "{:<5} {:<8} {:<12} {:<6} {state}",
            i.id,
            i.kind.word(),
            i.stream,
            fmt_age(age_s(i, now)),
        ));
    }
    // The divergence is the loud state: below the table, each waiting choice
    // names the server's objection verbatim and spells the way out.
    for i in intents.iter().filter(|i| i.is_diverged()) {
        if let IntentState::Diverged {
            status,
            title,
            detail,
            ..
        } = &i.state
        {
            let objection = if detail.is_empty() {
                title.clone()
            } else {
                format!("{title} — {detail}")
            };
            out.push(paint(
                &format!(
                    "✗ #{} {} diverged ({status}) — {objection}",
                    i.id,
                    i.kind.word()
                ),
                COLOR_DIVERGED,
                colored,
            ));
            out.push(paint(
                &format!(
                    "  resolve: engineer queue resolve {} --keep=local|server|both",
                    i.id
                ),
                COLOR_MUTED,
                colored,
            ));
        }
    }
    Outcome {
        out,
        err: vec![],
        code,
    }
}

/// The bare read's verdict mirrors `sync`'s: a waiting divergence outranks
/// plain depth; an empty or parked-only queue is calm (parked intents are
/// kept for review, not waiting to sync).
fn exit_for(intents: &[Intent]) -> i32 {
    if intents.iter().any(Intent::is_diverged) {
        EXIT_DIVERGED
    } else if intents.iter().any(Intent::is_pending) {
        EXIT_QUEUED
    } else {
        0
    }
}

fn json_read(intents: &[Intent], now: i64) -> serde_json::Value {
    serde_json::json!({
        "depth": intents.len(),
        "oldest_age_s": intents.first().map(|i| age_s(i, now)),
        "diverged": intents.iter().filter(|i| i.is_diverged()).count(),
        "parked": intents.iter().filter(|i| i.is_parked()).count(),
        "intents": intents.iter().map(|i| {
            let mut v = serde_json::json!({
                "id": i.id,
                "verb": i.kind.word(),
                "stream": i.stream,
                "age_s": age_s(i, now),
                "state": state_word(i),
                "attempts": i.attempts,
            });
            // The stored objection rides along so a script can read the
            // divergence without the TUI — the coded conflict included, so a
            // consumer can switch on `code` and read the extensions instead of
            // parsing prose.
            match &i.state {
                IntentState::Diverged { status, title, detail, code, conflict, .. } => {
                    let mut problem = serde_json::json!({
                        "status": status, "title": title, "detail": detail,
                    });
                    if let Some(code) = code {
                        problem["code"] = serde_json::json!(code);
                    }
                    if !conflict.is_empty() {
                        problem["conflict"] = serde_json::to_value(conflict)
                            .unwrap_or(serde_json::Value::Null);
                    }
                    v["problem"] = problem;
                }
                IntentState::Parked { reason } => {
                    v["reason"] = serde_json::json!(reason);
                }
                IntentState::Pending => {}
            }
            v
        }).collect::<Vec<_>>(),
    })
}

fn state_word(i: &Intent) -> &'static str {
    match i.state {
        IntentState::Pending => "pending",
        IntentState::Diverged { .. } => "diverged",
        IntentState::Parked { .. } => "parked",
    }
}

fn age_s(i: &Intent, now: i64) -> i64 {
    (now - i.queued_at.as_second()).max(0)
}

/// `42s` · `7m` · `3h` · `2d` — the queue table's one-glance age.
fn fmt_age(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// ---------------------------------------------------------------- sync

async fn sync(api: &ApiClient, store: &QueueStore, json: bool, colored: bool) -> Outcome {
    let report = match queue::drain(api, store).await {
        Ok(report) => report,
        Err(e) => return Outcome::fail(format!("replay failed: {e}")),
    };
    let code = if report.diverged {
        EXIT_DIVERGED
    } else if report.remaining > 0 {
        EXIT_QUEUED
    } else {
        0
    };

    if json {
        let value = serde_json::json!({
            "replayed": report.replayed,
            "remaining": report.remaining,
            "diverged": report.diverged,
        });
        return Outcome {
            out: vec![value.to_string()],
            err: vec![],
            code,
        };
    }

    let line = if report.diverged {
        format!(
            "{} — the server disagrees; {} replayed · {} still queued behind the choice",
            paint("✗ diverged", COLOR_DIVERGED, colored),
            report.replayed,
            report.remaining
        )
    } else if report.remaining > 0 && report.replayed > 0 {
        format!(
            "{} replayed · {} still queued, offline",
            report.replayed, report.remaining
        )
    } else if report.remaining > 0 {
        format!("{} still queued, offline", report.remaining)
    } else if report.replayed > 0 {
        format!(
            "{} — {} replayed",
            paint("✓ synced", COLOR_SYNCED, colored),
            report.replayed
        )
    } else {
        // Parked intents are out of play but still stored — an "empty" line
        // over them would hide the kept-for-review work.
        match store.summary().map(|s| s.parked).unwrap_or(0) {
            0 => "queue empty".into(),
            n => format!("nothing to replay · {n} parked for review"),
        }
    };
    Outcome {
        out: vec![line],
        err: vec![],
        code,
    }
}

// ---------------------------------------------------------------- resolve

/// `resolve <id> --keep=local|server|both` — the headless twin of the Timer
/// screen's reconcile panel, through the same `queue::resolve` engine. A
/// keep-local/keep-both continues the drain behind the choice, exactly as the
/// TUI does.
async fn resolve_cmd(
    api: &ApiClient,
    store: &QueueStore,
    cached: Option<&Timer>,
    id: u64,
    keep: &str,
    json: bool,
    colored: bool,
) -> Outcome {
    let Some(resolution) = Resolution::from_name(keep) else {
        // clap's value_parser already constrains this; belt and braces.
        return Outcome::refuse(format!("unknown --keep \"{keep}\" — local | server | both"));
    };
    let resolved =
        match queue::resolve(api, store, cached, id, resolution, jiff::Timestamp::now()).await {
            Ok(resolved) => resolved,
            Err(queue::ResolveError::Api(ApiError::Transport(_))) => {
                return Outcome::refuse("offline — resolving needs the wire; retry online");
            }
            Err(
                e @ (queue::ResolveError::NotDiverged(_)
                | queue::ResolveError::CannotCompose(_)
                | queue::ResolveError::EditRejected(_)),
            ) => {
                return Outcome::refuse(e.to_string());
            }
            Err(queue::ResolveError::Api(e)) => {
                return Outcome::refuse(format!("the server refused the resolution: {e}"));
            }
            Err(e @ queue::ResolveError::Queue(_)) => return Outcome::fail(e.to_string()),
        };

    // Keep-local/keep-both unblocked the queue — continue the drain behind the
    // choice. Take-server parked the whole session; there is nothing behind it.
    let replayed = match resolved {
        Resolved::SwitchedToLocal | Resolved::SegmentWritten { .. } => {
            queue::drain(api, store).await.ok().map(|r| r.replayed)
        }
        Resolved::Parked { .. } => None,
    };

    if json {
        let mut v = serde_json::json!({ "resolved": id, "keep": resolution.as_str() });
        match resolved {
            Resolved::SwitchedToLocal => v["outcome"] = "switched".into(),
            Resolved::SegmentWritten {
                activity_id,
                segment_id,
                minutes,
            } => {
                v["outcome"] = "segment".into();
                v["activity_id"] = activity_id.into();
                v["segment_id"] = segment_id.into();
                v["minutes"] = minutes.into();
            }
            Resolved::Parked { count } => {
                v["outcome"] = "parked".into();
                v["parked"] = count.into();
            }
        }
        if let Some(n) = replayed {
            v["replayed"] = n.into();
        }
        return Outcome::ok(v.to_string());
    }

    let mut line = match resolved {
        Resolved::SwitchedToLocal => format!(
            "{} kept local — the server stopped & saved its session; yours took over",
            paint("✓", COLOR_SYNCED, colored)
        ),
        Resolved::SegmentWritten {
            segment_id,
            minutes,
            ..
        } => format!(
            "{} kept — {minutes}m written (segment {segment_id}); nothing lost",
            paint("✓", COLOR_SYNCED, colored)
        ),
        Resolved::Parked { count } => format!(
            "{} took server — {count} intent{} parked for review, nothing deleted",
            paint("✓", COLOR_SYNCED, colored),
            if count == 1 { "" } else { "s" }
        ),
    };
    if let Some(n) = replayed.filter(|n| *n > 0) {
        line.push_str(&format!(" · {n} replayed behind it"));
    }
    Outcome::ok(line)
}

// ------------------------------- the rejected write's gestures (#109) ------

/// `resolve <id> --edit` — the §Diverged · rejected segment `e`: open the
/// stored payload's times in `$EDITOR`, re-pend the corrected write, retry
/// the replay. An abort (`:cq`) changes nothing; a buffer that doesn't parse
/// refuses and the intent stays diverged. Exits 4 when the retry diverges
/// again — the caller must know the server still refuses.
async fn edit_cmd(
    api: &ApiClient,
    store: &QueueStore,
    id: u64,
    json: bool,
    colored: bool,
    editor: &str,
) -> Outcome {
    let intent = match store.intents() {
        Ok(intents) => intents.into_iter().find(|i| i.id == id),
        Err(e) => return Outcome::fail(e.to_string()),
    };
    let Some(intent) = intent.filter(Intent::is_diverged) else {
        return Outcome::refuse(format!("intent #{id} is not waiting on a divergence"));
    };
    let Some(seed) = queue::edit_seed(&intent) else {
        return Outcome::refuse(format!(
            "a diverged {} has nothing editable — resolve it with --keep=local|server|both",
            intent.kind.word()
        ));
    };
    let buffer = match crate::editor::edit_with(editor, &seed) {
        Ok(crate::editor::EditorOutcome::Saved(buffer)) => buffer,
        Ok(crate::editor::EditorOutcome::Aborted) => {
            return Outcome::refuse("edit aborted — nothing changed, the intent stays diverged");
        }
        Err(e) => return Outcome::fail(format!("editor failed: {e}")),
    };
    let updated = match queue::apply_edit(store, id, &buffer) {
        Ok(updated) => updated,
        Err(e @ queue::ResolveError::Queue(_)) => return Outcome::fail(e.to_string()),
        Err(e) => return Outcome::refuse(e.to_string()),
    };

    // The corrected write is pending again — retry now and report honestly.
    let report = queue::drain(api, store).await.ok();
    let (still_diverged, replayed) = report
        .map(|r| (r.diverged, r.replayed))
        .unwrap_or((false, 0));
    let code = if still_diverged { EXIT_DIVERGED } else { 0 };

    if json {
        let mut v = serde_json::json!({
            "resolved": id, "outcome": "edited",
            "verb": updated.kind.word(),
            "replayed": replayed, "diverged": still_diverged,
        });
        if still_diverged {
            v["outcome"] = "edited-still-diverged".into();
        }
        return Outcome {
            out: vec![v.to_string()],
            err: vec![],
            code,
        };
    }
    let line = if still_diverged {
        format!(
            "{} edited, but the server still refuses — diverged again; edit once more, or --drop/--skip",
            paint("✗", COLOR_DIVERGED, colored)
        )
    } else {
        format!(
            "{} edited — the corrected {} replayed ({replayed} landed)",
            paint("✓", COLOR_SYNCED, colored),
            updated.kind.word()
        )
    };
    Outcome {
        out: vec![line],
        err: vec![],
        code,
    }
}

/// `resolve <id> --drop --force` — the §Diverged · rejected segment `x`: the
/// queue's one user-chosen delete. `--force` is the confirmation (the TUI's
/// second `x`); without it the verb refuses and nothing changes.
async fn drop_cmd(
    api: &ApiClient,
    store: &QueueStore,
    id: u64,
    force: bool,
    json: bool,
    colored: bool,
) -> Outcome {
    if !force {
        return Outcome::refuse(
            "dropping discards the queued write forever — rerun with --force to confirm",
        );
    }
    let dropped = match queue::drop_intent(store, id) {
        Ok(dropped) => dropped,
        Err(e @ queue::ResolveError::Queue(_)) => return Outcome::fail(e.to_string()),
        Err(e) => return Outcome::refuse(e.to_string()),
    };
    // The stream is unblocked — drain what was queued behind the choice.
    let replayed = queue::drain(api, store).await.ok().map(|r| r.replayed);

    if json {
        let mut v = serde_json::json!({
            "resolved": id, "outcome": "dropped", "verb": dropped.kind.word(),
        });
        if let Some(n) = replayed {
            v["replayed"] = n.into();
        }
        return Outcome::ok(v.to_string());
    }
    let mut line = format!(
        "{} dropped — the queued {} left the queue; nothing was written",
        paint("✓", COLOR_SYNCED, colored),
        dropped.kind.word()
    );
    if let Some(n) = replayed.filter(|n| *n > 0) {
        line.push_str(&format!(" · {n} replayed behind it"));
    }
    Outcome::ok(line)
}

/// `resolve <id> --skip` — the §Diverged · rejected segment `s`: park it
/// (reason `skipped`), kept in the queue for a later decision, out of the
/// replay line; the stream behind it keeps syncing.
async fn skip_cmd(
    api: &ApiClient,
    store: &QueueStore,
    id: u64,
    json: bool,
    colored: bool,
) -> Outcome {
    let skipped = match queue::skip_intent(store, id) {
        Ok(skipped) => skipped,
        Err(e @ queue::ResolveError::Queue(_)) => return Outcome::fail(e.to_string()),
        Err(e) => return Outcome::refuse(e.to_string()),
    };
    let replayed = queue::drain(api, store).await.ok().map(|r| r.replayed);

    if json {
        let mut v = serde_json::json!({
            "resolved": id, "outcome": "skipped", "verb": skipped.kind.word(),
        });
        if let Some(n) = replayed {
            v["replayed"] = n.into();
        }
        return Outcome::ok(v.to_string());
    }
    let mut line = format!(
        "{} skipped — the {} stays in the queue (parked), nothing lost",
        paint("✓", COLOR_SYNCED, colored),
        skipped.kind.word()
    );
    if let Some(n) = replayed.filter(|n| *n > 0) {
        line.push_str(&format!(" · {n} replayed behind it"));
    }
    Outcome::ok(line)
}

// Terminal-palette 256 colours (docs/designs/README.md palette mapping).
const COLOR_SYNCED: u8 = 108; // success green
const COLOR_QUEUED: u8 = 105; // accent indigo
const COLOR_DIVERGED: u8 = 167; // danger red
const COLOR_MUTED: u8 = 244;

fn paint(s: &str, color: u8, colored: bool) -> String {
    if colored {
        format!("\x1b[38;5;{color}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::IntentKind;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-queue-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    fn seeded(dir: &std::path::Path, n: usize) -> QueueStore {
        let store = QueueStore::at(dir.join("queue.json"));
        for _ in 0..n {
            store
                .enqueue(IntentKind::TimerPause {
                    at: jiff::Timestamp::now(),
                })
                .unwrap();
        }
        store
    }

    fn diverge_first(store: &QueueStore) {
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 422,
                    title: "Segment overlaps".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Default::default(),
                };
            })
            .unwrap();
    }

    /// The pick-a-side shape of the resolve verb, gestures off.
    fn resolve_keep(id: u64, keep: &str) -> QueueCmd {
        QueueCmd::Resolve {
            id,
            keep: Some(keep.into()),
            edit: false,
            drop: false,
            force: false,
            skip: false,
        }
    }

    /// One rejected-write gesture, everything else off.
    fn resolve_gesture(id: u64, edit: bool, drop: bool, force: bool, skip: bool) -> QueueCmd {
        QueueCmd::Resolve {
            id,
            keep: None,
            edit,
            drop,
            force,
            skip,
        }
    }

    #[tokio::test]
    async fn empty_queue_reads_calm_and_exits_zero() {
        let dir = scratch();
        let store = seeded(&dir, 0);
        let outcome = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.out, vec!["queue empty"]);
    }

    #[tokio::test]
    async fn pending_intents_read_as_a_table_and_exit_queued() {
        let dir = scratch();
        let store = seeded(&dir, 2);
        let outcome = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, EXIT_QUEUED);
        assert_eq!(outcome.out.len(), 3, "header + one row per intent");
        assert!(outcome.out[0].contains("INTENT"));
        assert!(outcome.out[0].contains("STATE"));
        assert!(outcome.out[1].contains("pause"));
        assert!(outcome.out[1].contains("timer"));
        assert!(outcome.out[1].contains("pending"));
    }

    #[tokio::test]
    async fn a_diverged_intent_in_the_store_exits_diverged() {
        let dir = scratch();
        let store = seeded(&dir, 2);
        diverge_first(&store);
        let outcome = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, EXIT_DIVERGED);
        assert!(outcome.out[1].contains("diverged"));
    }

    #[tokio::test]
    async fn json_read_carries_the_contract_fields() {
        let dir = scratch();
        let store = seeded(&dir, 2);
        diverge_first(&store);
        let outcome = dispatch(&dead_api(), &store, None, None, true, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, EXIT_DIVERGED);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["depth"], 2);
        assert!(v["oldest_age_s"].as_i64().unwrap() >= 0);
        assert_eq!(v["diverged"], 1);
        assert_eq!(v["intents"].as_array().unwrap().len(), 2);
        assert_eq!(v["intents"][0]["verb"], "pause");
        assert_eq!(v["intents"][0]["stream"], "timer");
        assert_eq!(v["intents"][0]["state"], "diverged");
        assert_eq!(v["intents"][1]["state"], "pending");
        assert_eq!(v["intents"][1]["attempts"], 0);
    }

    #[tokio::test]
    async fn json_read_of_an_empty_queue_has_null_age() {
        let dir = scratch();
        let store = seeded(&dir, 0);
        let outcome = dispatch(&dead_api(), &store, None, None, true, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["depth"], 0);
        assert_eq!(v["oldest_age_s"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_corrupt_queue_is_exit_five_not_a_silent_empty() {
        let dir = scratch();
        let store = seeded(&dir, 1);
        std::fs::write(dir.join("queue.json"), "{not json").unwrap();
        let outcome = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(outcome.err[0].contains("corrupt"));
    }

    #[tokio::test]
    async fn sync_drains_and_reports_the_count() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(2)
            .mount(&server)
            .await;

        let dir = scratch();
        let store = seeded(&dir, 2);
        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(QueueCmd::Sync),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.out, vec!["✓ synced — 2 replayed"]);
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sync_on_an_empty_queue_is_calm() {
        let dir = scratch();
        let store = seeded(&dir, 0);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(QueueCmd::Sync),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.out, vec!["queue empty"]);
    }

    #[tokio::test]
    async fn sync_against_a_dead_address_is_queued_offline_not_a_failure() {
        let dir = scratch();
        let store = seeded(&dir, 3);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(QueueCmd::Sync),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, EXIT_QUEUED);
        assert_eq!(outcome.out, vec!["3 still queued, offline"]);
        assert!(outcome.err.is_empty(), "cron must not page on a tunnel");
    }

    #[tokio::test]
    async fn sync_reports_a_divergence_and_exits_four() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Segment overlaps", "status": 422, "detail": "…"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = scratch();
        let store = seeded(&dir, 2);
        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(QueueCmd::Sync),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, EXIT_DIVERGED);
        assert!(outcome.out[0].contains("diverged"));
        assert!(outcome.out[0].contains("2 still queued"));
    }

    #[tokio::test]
    async fn sync_json_mirrors_the_report() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = scratch();
        let store = seeded(&dir, 1);
        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(QueueCmd::Sync),
            true,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["replayed"], 1);
        assert_eq!(v["remaining"], 0);
        assert_eq!(v["diverged"], false);
    }

    #[test]
    fn ages_read_at_a_glance() {
        assert_eq!(fmt_age(42), "42s");
        assert_eq!(fmt_age(420), "7m");
        assert_eq!(fmt_age(7200), "2h");
        assert_eq!(fmt_age(200_000), "2d");
    }

    // -------------------------------------------------- resolve (#106)

    fn cached_running() -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "activity_id": 9, "label": "systems",
            "started_at": "2026-07-15T09:00:00Z", "elapsed_seconds": 0
        }))
        .unwrap()
    }

    /// A diverged start at the queue head with a pending pause behind it.
    fn seeded_diverged_start(dir: &std::path::Path) -> (QueueStore, u64) {
        let store = QueueStore::at(dir.join("queue.json"));
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerPause {
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        diverge_first(&store);
        (store, start.id)
    }

    #[tokio::test]
    async fn bare_read_names_the_objection_and_the_way_out() {
        let dir = scratch();
        let store = seeded(&dir, 1);
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 409,
                    title: "Conflict".into(),
                    detail: "a timer is already running".into(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Default::default(),
                };
            })
            .unwrap();
        let outcome = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(outcome.code, EXIT_DIVERGED);
        let text = outcome.out.join("\n");
        assert!(
            text.contains("Conflict — a timer is already running"),
            "the server's objection verbatim: {text}"
        );
        assert!(
            text.contains("engineer queue resolve 1 --keep=local|server|both"),
            "the way out is spelled: {text}"
        );
    }

    #[tokio::test]
    async fn resolve_keep_local_switches_and_drains_the_rest() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The continued drain replays the pending pause behind the choice.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = scratch();
        let (store, id) = seeded_diverged_start(&dir);
        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(resolve_keep(id, "local")),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("kept local"), "{}", outcome.out[0]);
        assert!(
            outcome.out[0].contains("1 replayed behind it"),
            "{}",
            outcome.out[0]
        );
        assert!(store.intents().unwrap().is_empty(), "the queue drained");
    }

    #[tokio::test]
    async fn resolve_take_server_parks_and_exits_calm_after() {
        let dir = scratch();
        let (store, id) = seeded_diverged_start(&dir);
        // Take-server needs no wire at all — parking is a local keep.
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_keep(id, "server")),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(
            outcome.out[0].contains("2 intents parked for review"),
            "{}",
            outcome.out[0]
        );
        assert!(
            outcome.out[0].contains("nothing deleted"),
            "{}",
            outcome.out[0]
        );

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 2, "kept, not deleted");
        assert!(intents.iter().all(Intent::is_parked));

        // The bare read now shows parked rows and reads calm (exit 0).
        let read = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(read.code, 0, "parked-only is a calm queue");
        assert!(read.out[1].contains("parked"), "{}", read.out[1]);
    }

    #[tokio::test]
    async fn resolve_keep_both_writes_the_segment_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 9, "minutes": 47
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = scratch();
        let store = QueueStore::at(dir.join("queue.json"));
        let start = store
            .enqueue(IntentKind::TimerStart {
                activity_id: Some(9),
                switch: false,
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerStop {
                at: jiff::Timestamp::now(),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge_first(&store);

        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(resolve_keep(start.id, "both")),
            true,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["resolved"], start.id);
        assert_eq!(v["keep"], "both");
        assert_eq!(v["outcome"], "segment");
        assert_eq!(v["segment_id"], 88);
        assert_eq!(v["minutes"], 47);
        assert!(store.intents().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resolve_keep_local_on_a_stop_uses_the_cached_activity() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 90, "activity_id": 9, "minutes": 47
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = scratch();
        let store = QueueStore::at(dir.join("queue.json"));
        let stop = store
            .enqueue(IntentKind::TimerStop {
                at: jiff::Timestamp::now(),
                local_elapsed_s: 2832,
            })
            .unwrap();
        diverge_first(&store);

        let outcome = dispatch(
            &client(&server),
            &store,
            Some(cached_running()),
            Some(resolve_keep(stop.id, "local")),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(
            outcome.out[0].contains("47m written (segment 90)"),
            "{}",
            outcome.out[0]
        );
    }

    #[tokio::test]
    async fn resolve_refusals_exit_one_and_change_nothing() {
        // Unknown id.
        let dir = scratch();
        let store = seeded(&dir, 1);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_keep(99, "server")),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("not waiting on a divergence"));

        // Offline keep-local: resolving needs the wire; the intent stays.
        let dir = scratch();
        let (store, id) = seeded_diverged_start(&dir);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_keep(id, "local")),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("offline"), "{}", outcome.err[0]);
        assert!(
            store.intents().unwrap()[0].is_diverged(),
            "kept, not dropped"
        );
    }

    #[tokio::test]
    async fn json_read_carries_the_problem_and_parked_fields() {
        let dir = scratch();
        let store = seeded(&dir, 3);
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 409,
                    title: "Timer already running".into(),
                    detail: "a timer is already running".into(),
                    type_uri: None,
                    errors: vec![],
                    code: Some("timer-already-running".into()),
                    conflict: serde_json::from_value(serde_json::json!({
                        "current": {
                            "id": 114, "activity_id": 9, "label": "systems",
                            "started_at": "2026-07-15T09:00:00Z", "paused": false
                        },
                        "resolutions": ["switch", "keep-remote"]
                    }))
                    .unwrap(),
                };
                doc.intents_mut()[1].state = IntentState::Parked {
                    reason: "took server · Conflict".into(),
                };
            })
            .unwrap();

        let outcome = dispatch(&dead_api(), &store, None, None, true, false, "false")
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["diverged"], 1);
        assert_eq!(v["parked"], 1);
        assert_eq!(v["intents"][0]["problem"]["status"], 409);
        assert_eq!(v["intents"][0]["problem"]["title"], "Timer already running");
        // The coded conflict is machine-readable: switch on `code`, read the
        // extensions — no prose parsing.
        assert_eq!(v["intents"][0]["problem"]["code"], "timer-already-running");
        assert_eq!(
            v["intents"][0]["problem"]["conflict"]["current"]["label"],
            "systems"
        );
        assert_eq!(
            v["intents"][0]["problem"]["conflict"]["resolutions"][0],
            "switch"
        );
        assert_eq!(v["intents"][1]["state"], "parked");
        assert_eq!(v["intents"][1]["reason"], "took server · Conflict");
        assert!(
            v["intents"][2]["problem"].is_null(),
            "pending rows carry no objection"
        );
    }

    #[tokio::test]
    async fn json_read_of_a_codeless_problem_carries_no_code_member() {
        let dir = scratch();
        let store = seeded(&dir, 1);
        diverge_first(&store);
        let outcome = dispatch(&dead_api(), &store, None, None, true, false, "false")
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["intents"][0]["problem"]["status"], 422);
        assert!(
            v["intents"][0]["problem"]["code"].is_null(),
            "the generic fallback stays exactly as it was"
        );
        assert!(v["intents"][0]["problem"]["conflict"].is_null());
    }

    #[tokio::test]
    async fn sync_over_a_parked_only_queue_says_so() {
        let dir = scratch();
        let store = seeded(&dir, 1);
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Parked {
                    reason: "took server · Conflict".into(),
                };
            })
            .unwrap();
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(QueueCmd::Sync),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0, "parked is not queued-offline");
        assert_eq!(outcome.out, vec!["nothing to replay · 1 parked for review"]);
    }

    // ------------------------- the rejected write's gestures (#109) --------

    /// A diverged `SegmentCreate` — the §Diverged · rejected segment case.
    fn seeded_rejected_segment(dir: &std::path::Path) -> (QueueStore, u64) {
        let store = QueueStore::at(dir.join("queue.json"));
        let seg = store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        diverge_first(&store);
        (store, seg.id)
    }

    /// The headless edit-retry round-trip, fake `$EDITOR` and all: the script
    /// rewrites the buffer's times, the corrected segment replays on the wire,
    /// and the queue drains.
    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_edit_roundtrips_through_the_editor_and_retries() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let (store, id) = seeded_rejected_segment(&dir);

        // A fake editor that replaces the seeded buffer with corrected times.
        let editor = dir.join("fake-editor.sh");
        std::fs::write(
            &editor,
            "#!/bin/sh\nprintf 'started_at: 2026-07-15T15:10:00Z\\nminutes: 30\\n' > \"$1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "segment": { "started_at": "2026-07-15T15:10:00Z", "duration_minutes": 30 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 9, "minutes": 30
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(
            &client(&server),
            &store,
            None,
            Some(resolve_gesture(id, true, false, false, false)),
            false,
            false,
            editor.to_str().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("edited"), "{}", outcome.out[0]);
        assert!(outcome.out[0].contains("1 landed"), "{}", outcome.out[0]);
        assert!(
            store.intents().unwrap().is_empty(),
            "the corrected write synced"
        );
    }

    /// An aborted edit (`:cq` — the editor exits non-zero) changes nothing:
    /// the intent stays diverged and the verb refuses.
    #[tokio::test]
    async fn resolve_edit_abort_keeps_the_intent_diverged() {
        let dir = scratch();
        let (store, id) = seeded_rejected_segment(&dir);
        // `false` exits 1 without touching the buffer — an abort.
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_gesture(id, true, false, false, false)),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(
            outcome.err[0].contains("edit aborted"),
            "{}",
            outcome.err[0]
        );
        assert!(store.intents().unwrap()[0].is_diverged(), "untouched");
    }

    #[tokio::test]
    async fn resolve_drop_requires_force_then_removes_the_intent() {
        let dir = scratch();
        let (store, id) = seeded_rejected_segment(&dir);

        // Without --force: refused — drop is explicit AND confirmed.
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_gesture(id, false, true, false, false)),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("--force"), "{}", outcome.err[0]);
        assert_eq!(store.intents().unwrap().len(), 1, "nothing left the queue");

        // With --force: gone, explicitly — and the line says nothing was written.
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_gesture(id, false, true, true, false)),
            true,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["outcome"], "dropped");
        assert_eq!(v["verb"], "log");
        assert!(
            store.intents().unwrap().is_empty(),
            "the one user-chosen delete"
        );
    }

    #[tokio::test]
    async fn resolve_skip_parks_and_the_queue_reads_calm() {
        let dir = scratch();
        let (store, id) = seeded_rejected_segment(&dir);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_gesture(id, false, false, false, true)),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("skipped"), "{}", outcome.out[0]);
        assert!(
            outcome.out[0].contains("nothing lost"),
            "{}",
            outcome.out[0]
        );

        let intents = store.intents().unwrap();
        assert_eq!(intents.len(), 1, "kept in the queue");
        assert!(intents[0].is_parked());

        // The bare read now shows the parked row and exits calm.
        let read = dispatch(&dead_api(), &store, None, None, false, false, "false")
            .await
            .unwrap();
        assert_eq!(read.code, 0, "skipped-only is a calm queue");
    }

    #[tokio::test]
    async fn resolve_with_no_gesture_refuses_naming_the_choices() {
        let dir = scratch();
        let (store, id) = seeded_rejected_segment(&dir);
        let outcome = dispatch(
            &dead_api(),
            &store,
            None,
            Some(resolve_gesture(id, false, false, false, false)),
            false,
            false,
            "false",
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(
            outcome.err[0].contains("--keep=local|server|both, --edit, --drop, or --skip"),
            "{}",
            outcome.err[0]
        );
        assert!(store.intents().unwrap()[0].is_diverged(), "untouched");
    }
}
