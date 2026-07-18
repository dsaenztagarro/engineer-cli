//! Headless `engineer target` — the one-shot twin of the Progress screen's
//! target verbs (docs/designs/briefs/proposed/progress.brief.md §6).
//!
//! `list` / `declare` / `adjust` / `retire`, with `--json` (machine) and a plain
//! per-line form (pipe). Output is plain when piped: ANSI colour is applied only
//! on a TTY and never when NO_COLOR is set. Exit 0 on success, 1 on refusal
//! (bad args, not found, or a closed target version), with the reason on stderr.
//!
//! Targets are append-only versions (engineer ADR 0026): `adjust` returns the
//! LIVE row — its id may differ from the one addressed — and `retire` closes a
//! lineage without ever deleting its history.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;

use crate::api::{ApiClient, ApiError, TargetCreate, TargetRef, TargetScope, TargetState};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::messages;
use crate::queue::QueuedClient;

#[derive(Args)]
pub struct TargetArgs {
    /// Emit JSON instead of the human line (an array for `list`, an object for
    /// declare/adjust/retire).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Option<TargetCmd>,
}

#[derive(Subcommand)]
enum TargetCmd {
    /// List targets — one row per lineage (default: active only).
    List {
        /// Include retired lineages alongside the active ones.
        #[arg(long)]
        all: bool,
        /// Only the retired lineages.
        #[arg(long, conflicts_with = "all")]
        retired: bool,
    },
    /// Declare a weekly target — exactly one scope plus the weekly hours.
    Declare {
        /// Scope the target to a domain, by id.
        #[arg(long)]
        domain: Option<i64>,
        /// Scope the target to an activity kind (e.g. `coding`).
        #[arg(long)]
        kind: Option<String>,
        /// Scope the target to an intent.
        #[arg(long)]
        intent: Option<String>,
        /// Weekly hours to aim for.
        #[arg(long)]
        hours: f64,
    },
    /// Adjust a target's weekly hours. Prints the live row — its id may change.
    Adjust { id: i64, hours: f64 },
    /// Retire a target — closes the lineage, keeping its history (never deletes).
    Retire { id: i64 },
}

pub async fn run(cfg: &Config, args: TargetArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let outcome = dispatch(&api, &queued, args.cmd, args.json, colored).await?;
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

    fn lines(out: Vec<String>) -> Self {
        Self {
            out,
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

async fn dispatch(
    api: &ApiClient,
    queued: &QueuedClient,
    cmd: Option<TargetCmd>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    match cmd {
        None => list(api, TargetState::Active, json, colored).await,
        Some(TargetCmd::List { all, retired }) => {
            let state = if retired {
                TargetState::Retired
            } else if all {
                TargetState::All
            } else {
                TargetState::Active
            };
            list(api, state, json, colored).await
        }
        Some(TargetCmd::Declare {
            domain,
            kind,
            intent,
            hours,
        }) => declare(queued, domain, kind, intent, hours, json, colored).await,
        Some(TargetCmd::Adjust { id, hours }) => adjust(queued, id, hours, json, colored).await,
        Some(TargetCmd::Retire { id }) => retire(queued, id, json, colored).await,
    }
}

async fn list(
    api: &ApiClient,
    state: TargetState,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let targets = match api.list_targets(state).await {
        Ok(list) => list.data,
        Err(e) => return Ok(refuse_problem(e)),
    };
    if json {
        let arr: Vec<serde_json::Value> = targets.iter().map(json_target).collect();
        return Ok(Outcome::ok(serde_json::Value::Array(arr).to_string()));
    }
    if targets.is_empty() {
        // A teaching empty state, mirroring the Progress screen's.
        return Ok(Outcome::ok(
            "no targets — declare one: engineer target declare --domain <id> --hours <n>",
        ));
    }
    Ok(Outcome::lines(
        targets.iter().map(|t| plain_target(t, colored)).collect(),
    ))
}

async fn declare(
    queued: &QueuedClient,
    domain: Option<i64>,
    kind: Option<String>,
    intent: Option<String>,
    hours: f64,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let scope = match resolve_scope(domain, kind, intent) {
        Ok(scope) => scope,
        Err(reason) => return Ok(Outcome::refuse(reason)),
    };
    if hours <= 0.0 {
        return Ok(Outcome::refuse("--hours must be greater than 0"));
    }
    match queued
        .create_target(&TargetCreate {
            scope,
            hours_per_week: hours,
        })
        .await
    {
        Ok(out) => {
            let t = out.value();
            if json {
                let mut v = json_target(t);
                if out.is_provisional() {
                    v["queued"] = true.into();
                }
                return Ok(Outcome::ok(v.to_string()));
            }
            let mut line = format!(
                "{} declared {} · {}h/wk  (target {})",
                paint("●", COLOR_OK, colored),
                t.scope.name(),
                fmt_hours(t.hours_per_week),
                t.id,
            );
            if out.is_provisional() {
                line.push_str(&queued_suffix(colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(e) => Ok(refuse_problem(e)),
    }
}

/// `· queued (offline)` — the provisional tail on a write that landed in the
/// queue instead of on the server (the `engineer timer` idiom).
fn queued_suffix(colored: bool) -> String {
    paint("  · queued (offline)", COLOR_MUTED, colored)
}

async fn adjust(
    queued: &QueuedClient,
    id: i64,
    hours: f64,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    if hours <= 0.0 {
        return Ok(Outcome::refuse("--hours must be greater than 0"));
    }
    match queued.adjust_target(id, hours).await {
        Ok(out) => {
            let t = out.value();
            if json {
                let mut v = json_target(t);
                if out.is_provisional() {
                    v["queued"] = true.into();
                }
                return Ok(Outcome::ok(v.to_string()));
            }
            // The adjust may have minted a successor version with a new id.
            let moved = if t.id != id {
                format!(" · lineage now at target {}", t.id)
            } else {
                String::new()
            };
            let mut line = format!(
                "{} adjusted {} → {}h/wk  (target {}){moved}",
                paint("●", COLOR_ACCENT, colored),
                t.scope.name(),
                fmt_hours(t.hours_per_week),
                t.id,
            );
            if out.is_provisional() {
                line.push_str(&queued_suffix(colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(e) => Ok(refuse_problem(e)),
    }
}

async fn retire(
    queued: &QueuedClient,
    id: i64,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    match queued.retire_target(id).await {
        Ok(out) => {
            let t = out.value();
            if json {
                let mut v = json_target(t);
                if out.is_provisional() {
                    v["queued"] = true.into();
                }
                return Ok(Outcome::ok(v.to_string()));
            }
            let mut line = format!(
                "{} retired {} — history kept  (target {})",
                paint("■", COLOR_MUTED, colored),
                t.scope.name(),
                t.id,
            );
            if out.is_provisional() {
                line.push_str(&queued_suffix(colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(e) => Ok(refuse_problem(e)),
    }
}

// ---------------------------------------------------------------- shapes

/// Turn the three scope flags into exactly one [`TargetScope`], or a refusal
/// reason when zero or more than one is given.
fn resolve_scope(
    domain: Option<i64>,
    kind: Option<String>,
    intent: Option<String>,
) -> Result<TargetScope, String> {
    match (domain, kind, intent) {
        (Some(id), None, None) => Ok(TargetScope::Domain(id)),
        (None, Some(k), None) => Ok(TargetScope::Kind(k)),
        (None, None, Some(i)) => Ok(TargetScope::Intent(i)),
        (None, None, None) => Err(
            "declare needs a scope — one of --domain <id> | --kind <name> | --intent <name>".into(),
        ),
        _ => Err(
            "declare takes exactly one scope — not more than one of --domain/--kind/--intent"
                .into(),
        ),
    }
}

fn state_word(t: &TargetRef) -> &'static str {
    if t.retired {
        "retired"
    } else if t.active {
        "active"
    } else {
        "superseded"
    }
}

/// `42  domain  distributed systems  6h/wk  active` — field order is stable.
fn plain_target(t: &TargetRef, colored: bool) -> String {
    let word = state_word(t);
    let color = match word {
        "active" => COLOR_ACCENT,
        _ => COLOR_MUTED,
    };
    format!(
        "{}  {}  {}  {}h/wk  {}",
        t.id,
        t.axis,
        t.scope.name(),
        fmt_hours(t.hours_per_week),
        paint(word, color, colored),
    )
}

fn json_target(t: &TargetRef) -> serde_json::Value {
    serde_json::json!({
        "id": t.id,
        "axis": t.axis,
        "scope": {
            "axis": t.scope.axis,
            "value": t.scope.value,
            "name": t.scope.name(),
        },
        "hours_per_week": t.hours_per_week,
        "active": t.active,
        "retired": t.retired,
    })
}

/// Map an API error to a one-line refusal (exit 1) — the 404/422 cases carry a
/// human `detail` the server already phrased (e.g. the closed-version hint).
fn refuse_problem(e: ApiError) -> Outcome {
    match e {
        ApiError::Problem { status: 404, .. } => Outcome::refuse("no such target"),
        ApiError::Problem { title, detail, .. } => Outcome::refuse(problem_text(&title, &detail)),
        ApiError::Unauthorized => Outcome::refuse(messages::not_authenticated()),
        other => Outcome::refuse(other.to_string()),
    }
}

fn problem_text(title: &str, detail: &str) -> String {
    if detail.is_empty() {
        title.to_string()
    } else {
        detail.to_string()
    }
}

/// Format weekly hours without a trailing `.0`: `6h`, but `2.5h` when fractional.
fn fmt_hours(hours: f64) -> String {
    if hours.fract().abs() < 1e-9 {
        format!("{hours:.0}")
    } else {
        format!("{hours:.1}")
    }
}

// Terminal-palette 256 colours (docs/designs/README.md palette mapping).
const COLOR_OK: u8 = 108; // success green
const COLOR_ACCENT: u8 = 105; // accent indigo
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
    use url::Url;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    /// A per-test scratch dir so the queue and read cache never touch the
    /// shared XDG state (the `timer_cli` test idiom).
    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-target-cli-{}-{}",
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
            crate::queue::QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        )
    }

    /// `dispatch` with an isolated, empty queue — what most tests need.
    async fn run_dispatch(
        api: &ApiClient,
        cmd: Option<TargetCmd>,
        json: bool,
        colored: bool,
    ) -> Result<Outcome, ApiError> {
        let dir = scratch();
        let queued = queued_at(api, &dir);
        dispatch(api, &queued, cmd, json, colored).await
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    fn target_json(id: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "axis": "domain",
            "scope": { "axis": "domain", "value": 7, "domain": { "id": 7, "name": "Distributed Systems" } },
            "hours_per_week": 6.0, "active": true, "retired": false
        })
    }

    #[test]
    fn resolve_scope_requires_exactly_one() {
        assert!(matches!(
            resolve_scope(Some(7), None, None),
            Ok(TargetScope::Domain(7))
        ));
        assert!(matches!(
            resolve_scope(None, Some("coding".into()), None),
            Ok(TargetScope::Kind(_))
        ));
        assert!(resolve_scope(None, None, None).is_err());
        assert!(resolve_scope(Some(7), Some("coding".into()), None).is_err());
    }

    #[tokio::test]
    async fn declare_posts_and_confirms() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/targets"))
            .and(body_partial_json(serde_json::json!({
                "target": { "axis": "domain", "hours_per_week": 6.0, "domain_id": 7 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(target_json(42)))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(
            &client(&server),
            Some(TargetCmd::Declare {
                domain: Some(7),
                kind: None,
                intent: None,
                hours: 6.0,
            }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 0);
        assert!(
            out.out[0].contains("declared Distributed Systems"),
            "{:?}",
            out.out
        );
        assert!(out.out[0].contains("6h/wk"));
        assert!(out.out[0].contains("target 42"));
    }

    #[tokio::test]
    async fn declare_without_scope_refuses_without_calling_api() {
        // No mock mounted — a refusal must short-circuit before any HTTP call.
        let server = MockServer::start().await;
        let out = run_dispatch(
            &client(&server),
            Some(TargetCmd::Declare {
                domain: None,
                kind: None,
                intent: None,
                hours: 6.0,
            }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("needs a scope"));
    }

    #[tokio::test]
    async fn list_json_is_an_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/targets"))
            .and(query_param("state", "active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ target_json(42) ], "meta": { "page": 1, "per_page": 25, "total": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), None, true, false)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed[0]["id"], 42);
        assert_eq!(parsed[0]["scope"]["name"], "Distributed Systems");
    }

    #[tokio::test]
    async fn adjust_notes_when_lineage_moves() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut b = target_json(99);
                b["hours_per_week"] = serde_json::json!(8.0);
                b
            }))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(
            &client(&server),
            Some(TargetCmd::Adjust { id: 42, hours: 8.0 }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("→ 8h/wk"));
        assert!(
            out.out[0].contains("lineage now at target 99"),
            "{:?}",
            out.out
        );
    }

    #[tokio::test]
    async fn closed_version_refuses_with_server_detail() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Target version is closed",
                "detail": "This version is closed. Fetch the live target for this axis and scope, then retry.",
                "status": 422
            })))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(
            &client(&server),
            Some(TargetCmd::Adjust { id: 42, hours: 8.0 }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("Fetch the live target"));
    }

    // ------------------------------------------------- offline writes (#111)

    #[tokio::test]
    async fn offline_declare_queues_and_says_so() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let out = dispatch(
            &api,
            &queued,
            Some(TargetCmd::Declare {
                domain: Some(7),
                kind: None,
                intent: None,
                hours: 6.0,
            }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 0, "a queued declare is a success");
        assert!(out.out[0].contains("queued (offline)"), "{}", out.out[0]);

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "declare");
    }

    #[tokio::test]
    async fn offline_adjust_and_retire_queue_with_json_flag() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let adjusted = dispatch(
            &api,
            &queued,
            Some(TargetCmd::Adjust { id: 9, hours: 4.0 }),
            true,
            false,
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&adjusted.out[0]).unwrap();
        assert_eq!(v["queued"], true);
        assert_eq!(v["hours_per_week"], 4.0);

        let retired = dispatch(
            &api,
            &queued,
            Some(TargetCmd::Retire { id: 9 }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(retired.code, 0);
        assert!(
            retired.out[0].contains("queued (offline)"),
            "{}",
            retired.out[0]
        );

        let store = crate::queue::QueueStore::at(dir.join("queue.json"));
        assert_eq!(store.pending().unwrap().len(), 2);
    }
}
