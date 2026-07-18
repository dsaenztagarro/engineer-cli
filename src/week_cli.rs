//! Headless `engineer week` (the planned-vs-done readout), `engineer week
//! reflect` (the `$EDITOR` retro reflection write), and `engineer plan` (declare
//! a plan item) — week-planning.brief.md's core: a readout, a one-liner, and one
//! stored line, not a planning canvas. The reflection persists through the v1
//! week-note route (dsaenztagarro/engineer#805, engineer PR #807), routed through
//! `QueuedClient` so an offline write queues like every other mutation.

use std::io::{IsTerminal, Read};

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;
use jiff::{civil::Date, Zoned};

use crate::api::{ActivityCreate, ApiClient, ApiError, Week};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::messages;
use crate::queue::QueuedClient;

#[derive(Args)]
pub struct WeekArgs {
    /// ISO week id (`YYYY-Www`); defaults to the current study week.
    #[arg(long, global = true)]
    week: Option<String>,
    /// Emit as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<WeekCmd>,
}

#[derive(Subcommand)]
enum WeekCmd {
    /// Write this week's retro reflection — the one stored line. Opens `$EDITOR`
    /// (the `git commit` pattern) on a TTY; takes the body from `-m/--message` or
    /// piped stdin otherwise. An empty body clears the note.
    Reflect(ReflectArgs),
}

#[derive(Args)]
pub struct ReflectArgs {
    /// The reflection text, inline (`git commit -m`). Omit to open `$EDITOR` (a
    /// TTY) or read piped stdin.
    #[arg(long, short = 'm')]
    message: Option<String>,
}

#[derive(Args)]
pub struct PlanArgs {
    /// The plan item's title.
    title: String,
    /// The day to plan it on (`YYYY-MM-DD`); defaults to today.
    #[arg(long)]
    on: Option<String>,
    /// Activity kind (e.g. reading, coding).
    #[arg(long)]
    kind: Option<String>,
    /// Rough size in minutes — the retro's done-threshold basis.
    #[arg(long)]
    size: Option<u32>,
    /// Domain id.
    #[arg(long)]
    domain: Option<i64>,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

pub async fn run_week(cfg: &Config, args: WeekArgs) -> Result<i32> {
    match args.cmd {
        Some(WeekCmd::Reflect(reflect)) => run_reflect(cfg, args.week, args.json, reflect).await,
        None => run_readout(cfg, args.week, args.json).await,
    }
}

/// The planned-vs-done readout (`engineer week [--week] [--json]`).
async fn run_readout(cfg: &Config, week: Option<String>, json: bool) -> Result<i32> {
    let api = client(cfg).await?;
    let colored = colored();
    let iso = week.unwrap_or_else(current_iso_week);
    let week = match api.get_week(&iso).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{}", problem_text(e));
            return Ok(1);
        }
    };
    if json {
        println!("{}", json_week(&week));
    } else {
        for line in human_week(&week, colored) {
            println!("{line}");
        }
    }
    Ok(0)
}

/// The `$EDITOR` retro reflection write (`engineer week reflect`). The git-commit
/// shape: `-m` inline, else piped stdin, else `$EDITOR` seeded from the current
/// note (a TTY). An empty body clears the note. Routes through `QueuedClient`, so
/// an offline write queues; `--json` echoes the persisted note.
async fn run_reflect(
    cfg: &Config,
    week: Option<String>,
    json: bool,
    args: ReflectArgs,
) -> Result<i32> {
    let api = client(cfg).await?;
    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = colored();
    let iso = week.unwrap_or_else(current_iso_week);

    let body = if let Some(message) = args.message {
        message
    } else if !std::io::stdin().is_terminal() {
        // Piped stdin (`echo "…" | engineer week reflect`): the whole stream is
        // the body, one trailing newline trimmed.
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim_end_matches('\n').to_string()
    } else {
        // A TTY: open $EDITOR seeded with the current note (the git-commit
        // pattern). A quit-without-write leaves the note untouched.
        let seed = match api.get_week(&iso).await {
            Ok(w) => w.note.body,
            Err(e) => {
                eprintln!("{}", problem_text(e));
                return Ok(1);
            }
        };
        match crate::editor::edit(&seed)? {
            crate::editor::EditorOutcome::Saved(body) => body,
            crate::editor::EditorOutcome::Aborted => {
                println!("reflection unchanged");
                return Ok(0);
            }
        }
    };

    let outcome = reflect_dispatch(&queued, &iso, &body, json, colored).await;
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

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
            code: 1,
        }
    }
}

/// Persist the reflection through the queue seam and render the persisted note
/// (`--json`) or a confirmation line. An empty body reads as a clear. Testable in
/// isolation with a scratch `QueuedClient` (a dead address exercises the offline
/// enqueue).
async fn reflect_dispatch(
    queued: &QueuedClient,
    iso: &str,
    body: &str,
    json: bool,
    colored: bool,
) -> Outcome {
    match queued.update_week_note(iso, body).await {
        Ok(outcome) => {
            let provisional = outcome.is_provisional();
            if json {
                return Outcome::ok(serde_json::to_string(outcome.value()).unwrap_or_default());
            }
            let verb = if body.trim().is_empty() {
                "reflection cleared"
            } else {
                "reflection saved"
            };
            let mut line = format!("{} {verb} · {iso}", paint("●", COLOR_OK, colored));
            if provisional {
                line.push_str(&paint("  · queued (offline)", COLOR_MUTED, colored));
            }
            Outcome::ok(line)
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

pub async fn run_plan(cfg: &Config, args: PlanArgs) -> Result<i32> {
    let api = client(cfg).await?;
    let colored = colored();
    let on = match &args.on {
        Some(s) => match s.parse::<Date>() {
            Ok(d) => d,
            Err(_) => {
                eprintln!("--on must be a date like 2026-07-15");
                return Ok(1);
            }
        },
        None => Zoned::now().date(),
    };
    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let create = ActivityCreate {
        title: args.title.clone(),
        planned_on: Some(on),
        target_duration_minutes: args.size,
        kind: args.kind.clone(),
        domain_id: args.domain,
        ..Default::default()
    };
    let outcome = plan_dispatch(&queued, &create, on, args.json, colored).await;
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

/// Declare a plan item through the queue seam and render the confirmation. An
/// offline declare enqueues (a provisional negative-id row) and still exits 0
/// with the `queued (offline)` tail; `--json` carries `queued` so a script can
/// tell a synced declare from a deferred one. Testable in isolation with a
/// scratch `QueuedClient` (a dead address exercises the offline enqueue).
async fn plan_dispatch(
    queued: &QueuedClient,
    create: &ActivityCreate,
    on: Date,
    json: bool,
    colored: bool,
) -> Outcome {
    match queued.create_activity(create).await {
        Ok(outcome) => {
            let provisional = outcome.is_provisional();
            let a = outcome.value();
            if json {
                return Outcome::ok(
                    serde_json::json!({
                        "id": a.id,
                        "title": a.title,
                        "planned_on": on.to_string(),
                        "queued": provisional,
                    })
                    .to_string(),
                );
            }
            let mut line = format!(
                "{} planned \"{}\" on {on}",
                paint("●", COLOR_OK, colored),
                a.title
            );
            if provisional {
                line.push_str(&paint("  · queued (offline)", COLOR_MUTED, colored));
            }
            Outcome::ok(line)
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

/// The readout: a week header, one line per plan item with its state, and the
/// planned-vs-done summary.
fn human_week(week: &Week, colored: bool) -> Vec<String> {
    let mut out = Vec::new();
    let phase = if week.week.closed {
        "closed"
    } else {
        "in progress"
    };
    out.push(paint(
        &format!("{} · {phase}", week.week.id),
        COLOR_MUTED,
        colored,
    ));

    if week.items().next().is_none() {
        out.push(paint("nothing planned this week", COLOR_MUTED, colored));
    } else {
        for item in week.items() {
            let (word, color) = if item.done {
                ("done ✓", COLOR_OK)
            } else if item.state == "left" {
                ("missed", COLOR_WARN)
            } else if item.state == "live" {
                ("in progress", COLOR_ACCENT)
            } else {
                ("planned", COLOR_MUTED)
            };
            out.push(format!("  {}  {}", item.title, paint(word, color, colored)));
        }
    }

    let pvd = &week.planned_vs_done;
    out.push(format!(
        "planned {} · done {} · {:.1}h logged",
        pvd.planned,
        pvd.done,
        pvd.logged_minutes as f64 / 60.0
    ));
    out
}

fn json_week(week: &Week) -> serde_json::Value {
    let items: Vec<serde_json::Value> = week
        .items()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "title": i.title,
                "state": i.state,
                "done": i.done,
                "size_minutes": i.size_minutes,
                "logged_minutes": i.logged_minutes,
            })
        })
        .collect();
    serde_json::json!({
        "week": { "id": week.week.id, "closed": week.week.closed },
        "items": items,
        "planned_vs_done": {
            "planned": week.planned_vs_done.planned,
            "done": week.planned_vs_done.done,
            "logged_minutes": week.planned_vs_done.logged_minutes,
            "planned_minutes": week.planned_vs_done.planned_minutes,
        },
    })
}

async fn client(cfg: &Config) -> Result<ApiClient> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    Ok(ApiClient::with_token(cfg.api_url.clone(), token))
}

fn colored() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn current_iso_week() -> String {
    let iso = Zoned::now().date().iso_week_date();
    format!("{:04}-W{:02}", iso.year(), iso.week())
}

fn problem_text(e: ApiError) -> String {
    match e {
        ApiError::Unauthorized => messages::not_authenticated().into(),
        ApiError::Problem { detail, .. } if !detail.is_empty() => detail,
        ApiError::Problem { title, .. } => title,
        other => other.to_string(),
    }
}

const COLOR_OK: u8 = 108;
const COLOR_WARN: u8 = 179;
const COLOR_ACCENT: u8 = 105;
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
    use crate::api::Week;

    fn week(closed: bool) -> Week {
        serde_json::from_value(serde_json::json!({
            "week": { "id": "2026-W29", "closed": closed },
            "days": [
                { "items": [
                    { "id": 1, "title": "SICP ch.3", "state": "done", "done": true,
                      "size_minutes": 90, "logged_minutes": 95 },
                    { "id": 2, "title": "systems paper", "state": "left", "done": false,
                      "size_minutes": 60, "logged_minutes": 0 }
                ]}
            ],
            "planned_vs_done": { "planned": 2, "done": 1, "logged_minutes": 95, "planned_minutes": 150 }
        }))
        .unwrap()
    }

    #[test]
    fn readout_shows_state_per_item_and_the_summary() {
        let lines = human_week(&week(false), false);
        assert!(lines[0].contains("2026-W29 · in progress"));
        assert!(lines
            .iter()
            .any(|l| l.contains("SICP ch.3") && l.contains("done ✓")));
        assert!(lines
            .iter()
            .any(|l| l.contains("systems paper") && l.contains("missed")));
        assert!(lines
            .iter()
            .any(|l| l.contains("planned 2 · done 1 · 1.6h logged")));
    }

    #[test]
    fn json_carries_items_and_the_comparison() {
        let v = json_week(&week(true));
        assert_eq!(v["week"]["closed"], true);
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert_eq!(v["items"][0]["done"], true);
        assert_eq!(v["planned_vs_done"]["done"], 1);
    }

    // --- reflect (#117): the retro write through the queue seam ---

    mod reflect {
        use super::super::reflect_dispatch;
        use crate::api::ApiClient;
        use crate::queue::{QueueStore, QueuedClient};
        use url::Url;
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn scratch() -> std::path::PathBuf {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "engineer-week-cli-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn queued_at(api: &ApiClient, dir: &std::path::Path) -> QueuedClient {
            QueuedClient::with_paths(
                api,
                QueueStore::at(dir.join("queue.json")),
                dir.join("timer-cache.json"),
            )
        }

        fn dead_api() -> ApiClient {
            ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "t".into())
        }

        #[tokio::test]
        async fn writes_the_wrapped_body_and_confirms() {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .and(path("/api/v1/weeks/2026-W29/note"))
                .and(body_json(serde_json::json!({
                    "note": { "body": "Read the paper first, build second." }
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iso_week": "2026-W29",
                    "body": "Read the paper first, build second.",
                    "updated_at": "2026-07-17T09:30:00Z"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
            let dir = scratch();
            let queued = queued_at(&api, &dir);
            let out = reflect_dispatch(
                &queued,
                "2026-W29",
                "Read the paper first, build second.",
                false,
                false,
            )
            .await;
            assert_eq!(out.code, 0);
            assert!(
                out.out[0].contains("reflection saved · 2026-W29"),
                "{:?}",
                out.out
            );
        }

        #[tokio::test]
        async fn json_echoes_the_persisted_note() {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .and(path("/api/v1/weeks/2026-W29/note"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iso_week": "2026-W29", "body": "build second",
                    "updated_at": "2026-07-17T09:30:00Z"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
            let dir = scratch();
            let queued = queued_at(&api, &dir);
            let out = reflect_dispatch(&queued, "2026-W29", "build second", true, false).await;
            assert_eq!(out.code, 0);
            let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
            assert_eq!(v["iso_week"], "2026-W29");
            assert_eq!(v["body"], "build second");
        }

        #[tokio::test]
        async fn an_empty_body_reads_as_a_clear() {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .and(path("/api/v1/weeks/2026-W29/note"))
                .and(body_json(serde_json::json!({ "note": { "body": "" } })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iso_week": "2026-W29", "body": ""
                })))
                .expect(1)
                .mount(&server)
                .await;

            let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
            let dir = scratch();
            let queued = queued_at(&api, &dir);
            let out = reflect_dispatch(&queued, "2026-W29", "", false, false).await;
            assert_eq!(out.code, 0);
            assert!(out.out[0].contains("reflection cleared"), "{:?}", out.out);
        }

        #[tokio::test]
        async fn a_dead_address_enqueues_the_write() {
            // Offline: the write can't bounce — it queues, and the CLI still
            // exits 0 with the `queued (offline)` tail.
            let dir = scratch();
            let queued = queued_at(&dead_api(), &dir);
            let out =
                reflect_dispatch(&queued, "2026-W29", "queued while offline", false, false).await;
            assert_eq!(out.code, 0);
            assert!(out.out[0].contains("queued (offline)"), "{:?}", out.out);

            let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
            assert_eq!(intents.len(), 1, "the reflection landed in the queue");
            assert_eq!(intents[0].kind.word(), "reflect");
            assert_eq!(intents[0].stream, "week:2026-W29");
        }
    }

    // --- plan add (#110): the declare through the queue seam ---

    mod plan {
        use super::super::plan_dispatch;
        use crate::api::{ActivityCreate, ApiClient};
        use crate::queue::{QueueStore, QueuedClient};
        use url::Url;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn scratch() -> std::path::PathBuf {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "engineer-plan-cli-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn queued_at(api: &ApiClient, dir: &std::path::Path) -> QueuedClient {
            QueuedClient::with_paths(
                api,
                QueueStore::at(dir.join("queue.json")),
                dir.join("timer-cache.json"),
            )
        }

        fn dead_api() -> ApiClient {
            ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "t".into())
        }

        fn create() -> ActivityCreate {
            ActivityCreate {
                title: "one systems paper".into(),
                planned_on: Some("2026-07-13".parse().unwrap()),
                ..Default::default()
            }
        }

        #[tokio::test]
        async fn a_live_declare_confirms_and_queues_nothing() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/activities"))
                .and(body_partial_json(serde_json::json!({
                    "activity": { "title": "one systems paper", "planned_on": "2026-07-13" }
                })))
                .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": 7, "title": "one systems paper", "status": "planned"
                })))
                .expect(1)
                .mount(&server)
                .await;

            let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
            let dir = scratch();
            let queued = queued_at(&api, &dir);
            let on = "2026-07-13".parse().unwrap();
            let out = plan_dispatch(&queued, &create(), on, false, false).await;
            assert_eq!(out.code, 0);
            assert!(
                out.out[0].contains("planned \"one systems paper\" on 2026-07-13"),
                "{:?}",
                out.out
            );
            assert!(
                !out.out[0].contains("queued"),
                "a live declare isn't queued"
            );
            assert!(QueueStore::at(dir.join("queue.json"))
                .pending()
                .unwrap()
                .is_empty());
        }

        #[tokio::test]
        async fn a_dead_address_enqueues_the_declare() {
            // Offline: the declare can't bounce — it queues, and the CLI still
            // exits 0 with the `queued (offline)` tail.
            let dir = scratch();
            let queued = queued_at(&dead_api(), &dir);
            let on = "2026-07-13".parse().unwrap();
            let out = plan_dispatch(&queued, &create(), on, false, false).await;
            assert_eq!(out.code, 0);
            assert!(out.out[0].contains("queued (offline)"), "{:?}", out.out);

            let intents = QueueStore::at(dir.join("queue.json")).pending().unwrap();
            assert_eq!(intents.len(), 1, "the declare landed in the queue");
            assert_eq!(intents[0].kind.word(), "plan");
            assert_eq!(intents[0].stream, "activity");
        }

        #[tokio::test]
        async fn json_carries_the_queued_flag_offline() {
            let dir = scratch();
            let queued = queued_at(&dead_api(), &dir);
            let on = "2026-07-13".parse().unwrap();
            let out = plan_dispatch(&queued, &create(), on, true, false).await;
            assert_eq!(out.code, 0);
            let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
            assert_eq!(v["title"], "one systems paper");
            assert_eq!(v["planned_on"], "2026-07-13");
            assert_eq!(v["queued"], true, "a script can tell a deferred declare");
            assert!(
                v["id"].as_i64().unwrap() < 0,
                "the provisional negative id is honest"
            );
        }
    }
}
