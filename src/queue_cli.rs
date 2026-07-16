//! Headless `engineer queue` — the offline write queue, observable
//! (offline-write.brief.md §8 Foundation; the §Queue inspector / headless-twin
//! boards). The bare read prints one row per unsynced intent; `sync` runs a
//! replay pass now. A look and a nudge — not a sync manager.
//!
//! Exit codes answer "does the queue need me?": 0 drained or empty · 3 writes
//! queued, offline (deliberately not a failure — a cron job must not page on a
//! tunnel) · 4 a divergence is waiting on a choice · 5 the replay itself
//! failed (non-transport, non-problem — e.g. queue io). Output is plain when
//! piped: ANSI colour is applied only on a TTY and never when NO_COLOR is set.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;

use crate::api::{ApiClient, ApiError};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::queue::{self, Intent, IntentState, QueueStore};

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
}

pub async fn run(cfg: &Config, args: QueueArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let store = QueueStore::open_default().map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let outcome = dispatch(&api, &store, args.cmd, args.json, colored).await?;
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

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

    fn fail(reason: impl Into<String>) -> Self {
        Self {
            out: vec![],
            err: vec![reason.into()],
            code: EXIT_FAILED,
        }
    }
}

async fn dispatch(
    api: &ApiClient,
    store: &QueueStore,
    cmd: Option<QueueCmd>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    Ok(match cmd {
        None => read(store, json, colored),
        Some(QueueCmd::Sync) => sync(api, store, json, colored).await,
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
        };
        out.push(format!(
            "{:<5} {:<8} {:<12} {:<6} {state}",
            i.id,
            i.kind.word(),
            i.stream,
            fmt_age(age_s(i, now)),
        ));
    }
    Outcome {
        out,
        err: vec![],
        code,
    }
}

/// The bare read's verdict mirrors `sync`'s: an empty queue is 0, a waiting
/// divergence outranks plain depth.
fn exit_for(intents: &[Intent]) -> i32 {
    if intents.is_empty() {
        0
    } else if intents.iter().any(Intent::is_diverged) {
        EXIT_DIVERGED
    } else {
        EXIT_QUEUED
    }
}

fn json_read(intents: &[Intent], now: i64) -> serde_json::Value {
    serde_json::json!({
        "depth": intents.len(),
        "oldest_age_s": intents.first().map(|i| age_s(i, now)),
        "diverged": intents.iter().filter(|i| i.is_diverged()).count(),
        "intents": intents.iter().map(|i| serde_json::json!({
            "id": i.id,
            "verb": i.kind.word(),
            "stream": i.stream,
            "age_s": age_s(i, now),
            "state": state_word(i),
            "attempts": i.attempts,
        })).collect::<Vec<_>>(),
    })
}

fn state_word(i: &Intent) -> &'static str {
    match i.state {
        IntentState::Pending => "pending",
        IntentState::Diverged { .. } => "diverged",
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
        "queue empty".into()
    };
    Outcome {
        out: vec![line],
        err: vec![],
        code,
    }
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
                };
            })
            .unwrap();
    }

    #[tokio::test]
    async fn empty_queue_reads_calm_and_exits_zero() {
        let dir = scratch();
        let store = seeded(&dir, 0);
        let outcome = dispatch(&dead_api(), &store, None, false, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.out, vec!["queue empty"]);
    }

    #[tokio::test]
    async fn pending_intents_read_as_a_table_and_exit_queued() {
        let dir = scratch();
        let store = seeded(&dir, 2);
        let outcome = dispatch(&dead_api(), &store, None, false, false)
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
        let outcome = dispatch(&dead_api(), &store, None, false, false)
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
        let outcome = dispatch(&dead_api(), &store, None, true, false)
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
        let outcome = dispatch(&dead_api(), &store, None, true, false)
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
        let outcome = dispatch(&dead_api(), &store, None, false, false)
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
        let outcome = dispatch(&client(&server), &store, Some(QueueCmd::Sync), false, false)
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
        let outcome = dispatch(&dead_api(), &store, Some(QueueCmd::Sync), false, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.out, vec!["queue empty"]);
    }

    #[tokio::test]
    async fn sync_against_a_dead_address_is_queued_offline_not_a_failure() {
        let dir = scratch();
        let store = seeded(&dir, 3);
        let outcome = dispatch(&dead_api(), &store, Some(QueueCmd::Sync), false, false)
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
        let outcome = dispatch(&client(&server), &store, Some(QueueCmd::Sync), false, false)
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
        let outcome = dispatch(&client(&server), &store, Some(QueueCmd::Sync), true, false)
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
}
