//! Headless `engineer inbox` — triage the assisted-capture drafts
//! (assisted-capture.brief.md). Bare `inbox` lists the pending drafts;
//! `accept` / `reject` / `ack` act on one; `show` reads one in full. `--json`
//! for machines, plain otherwise, exit 0 on success, 1 on refusal — the
//! `engineer timer`/`target` contract. Accepting writes the activity (the
//! server's `complete`), so a stale draft is a `422` surfaced as "already
//! moved on", not a crash.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;
use jiff::Timestamp;

use crate::api::{ApiClient, ApiError, Task};
use crate::auth::TokenProvider;
use crate::config::Config;

/// The past-tense outcome word each triage verb confirms. Shared with the TUI
/// inbox screen (`src/app/screens/inbox.rs`) so the headless verbs and the
/// screen speak one vocabulary — the accept/reject/ack copy is spelled once.
pub const ACCEPTED: &str = "accepted";
pub const REJECTED: &str = "rejected";
pub const ACKNOWLEDGED: &str = "acknowledged";

/// The stale-draft (`422`) line: a soft re-read, never a crash. The design's
/// §Inbox notify tile copy, shared so both surfaces render the same phrase.
pub const ALREADY_MOVED_ON: &str = "this draft already moved on — inbox re-read";

#[derive(Args)]
pub struct InboxArgs {
    /// Emit JSON (an array for the list, an object for a single task).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<InboxCmd>,
}

#[derive(Subcommand)]
enum InboxCmd {
    /// Accept a draft — writes the activity (the server's `complete`).
    Accept { id: i64 },
    /// Reject a draft, with an optional reason.
    Reject {
        id: i64,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Mark a draft seen — keep it for later.
    Ack { id: i64 },
    /// Show one draft in full.
    Show { id: i64 },
}

pub async fn run(cfg: &Config, args: InboxArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let outcome = dispatch(&api, args.cmd, args.json, colored).await;
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

async fn dispatch(api: &ApiClient, cmd: Option<InboxCmd>, json: bool, colored: bool) -> Outcome {
    match cmd {
        None => list(api, json, colored).await,
        Some(InboxCmd::Accept { id }) => {
            act(api.complete_task(id).await, ACCEPTED, id, json, colored)
        }
        Some(InboxCmd::Reject { id, reason }) => act(
            api.reject_task(id, reason).await,
            REJECTED,
            id,
            json,
            colored,
        ),
        Some(InboxCmd::Ack { id }) => act(
            api.acknowledge_task(id).await,
            ACKNOWLEDGED,
            id,
            json,
            colored,
        ),
        Some(InboxCmd::Show { id }) => match api.get_task(id).await {
            Ok(t) if json => Outcome::ok(to_json(&t)),
            Ok(t) => Outcome::lines(show_lines(&t, colored)),
            Err(e) => refuse_problem(e),
        },
    }
}

async fn list(api: &ApiClient, json: bool, colored: bool) -> Outcome {
    let tasks = match api.list_pending_tasks().await {
        Ok(t) => t,
        Err(e) => return refuse_problem(e),
    };
    if json {
        return Outcome::ok(to_json(&tasks));
    }
    if tasks.is_empty() {
        return Outcome::ok(paint("inbox clear ✓", COLOR_OK, colored));
    }
    let mut out = vec![paint(
        &format!("{} pending", tasks.len()),
        COLOR_MUTED,
        colored,
    )];
    out.extend(tasks.iter().map(|t| task_line(t, colored)));
    Outcome::lines(out)
}

/// The result of a triage verb — a one-line confirmation, or a graceful refusal
/// when the draft already moved on (`422`).
fn act(result: Result<Task, ApiError>, verb: &str, id: i64, json: bool, colored: bool) -> Outcome {
    match result {
        Ok(t) if json => Outcome::ok(to_json(&t)),
        Ok(_) => {
            let (glyph, color) = match verb {
                "rejected" => ("■", COLOR_MUTED),
                "accepted" => ("●", COLOR_OK),
                _ => ("●", COLOR_ACCENT),
            };
            Outcome::ok(format!("{} {verb} #{id}", paint(glyph, color, colored)))
        }
        Err(ApiError::Problem { status: 422, .. }) => {
            Outcome::refuse(format!("#{id} already moved on — re-run `engineer inbox`"))
        }
        Err(e) => refuse_problem(e),
    }
}

/// `#42  Log commit "fix parser"?  · Crafting Interpreters · expires 8h`
fn task_line(t: &Task, colored: bool) -> String {
    let prompt = t.prompt.as_deref().unwrap_or("(draft)");
    let who = t
        .entity
        .as_ref()
        .and_then(|e| e.name.as_deref())
        .map(|n| format!("  · {n}"))
        .unwrap_or_default();
    let exp = expires_badge(t, colored);
    format!(
        "{}  {prompt}{who}{exp}",
        paint(&format!("#{}", t.id), COLOR_ACCENT, colored)
    )
}

fn show_lines(t: &Task, colored: bool) -> Vec<String> {
    let mut out = vec![paint(
        &format!("#{} · {}", t.id, t.status),
        COLOR_MUTED,
        colored,
    )];
    if let Some(p) = &t.prompt {
        out.push(p.clone());
    }
    if let Some(name) = t.entity.as_ref().and_then(|e| e.name.as_deref()) {
        out.push(paint(&format!("entity: {name}"), COLOR_MUTED, colored));
    }
    if !t.context.is_null() {
        out.push(paint(
            &format!("context: {}", t.context),
            COLOR_MUTED,
            colored,
        ));
    }
    let exp = expires_badge(t, colored);
    if !exp.is_empty() {
        out.push(exp.trim_start().to_string());
    }
    out
}

/// ` · expires 8h` / ` · expired`, or empty when the draft has no expiry.
fn expires_badge(t: &Task, colored: bool) -> String {
    let Some(expires) = t.expires_at else {
        return String::new();
    };
    let secs = expires.as_second() - Timestamp::now().as_second();
    if secs <= 0 {
        return format!(" · {}", paint("expired", COLOR_WARN, colored));
    }
    let text = if secs >= 48 * 3600 {
        format!("expires {}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("expires {}h", secs / 3600)
    } else {
        format!("expires {}m", (secs / 60).max(1))
    };
    format!(" · {}", paint(&text, COLOR_MUTED, colored))
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

fn refuse_problem(e: ApiError) -> Outcome {
    match e {
        ApiError::Unauthorized => Outcome::refuse("not authenticated — run `engineer login`"),
        ApiError::Problem { status: 404, .. } => Outcome::refuse("no such task"),
        ApiError::Problem { detail, .. } if !detail.is_empty() => Outcome::refuse(detail),
        ApiError::Problem { title, .. } => Outcome::refuse(title),
        other => Outcome::refuse(other.to_string()),
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
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into())
    }

    #[tokio::test]
    async fn list_shows_a_count_and_a_line_per_draft() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/automations/tasks/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "id": 42, "status": "pending", "prompt": "Log commit \"fix parser\"?",
                      "entity": { "name": "Crafting Interpreters" } }
                ], "page": 1, "per_page": 25, "total": 1
            })))
            .mount(&server)
            .await;

        let out = dispatch(&client(&server), None, false, false).await;
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("1 pending"));
        assert!(out.out[1].contains("#42") && out.out[1].contains("fix parser"));
        assert!(out.out[1].contains("Crafting Interpreters"));
    }

    #[tokio::test]
    async fn empty_inbox_reads_clear() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/automations/tasks/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "page": 1, "per_page": 25, "total": 0
            })))
            .mount(&server)
            .await;
        let out = dispatch(&client(&server), None, false, false).await;
        assert!(out.out[0].contains("inbox clear"));
    }

    #[tokio::test]
    async fn accept_confirms_and_stale_draft_refuses() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/7/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "status": "completed"
            })))
            .mount(&server)
            .await;
        let ok = dispatch(
            &client(&server),
            Some(InboxCmd::Accept { id: 7 }),
            false,
            false,
        )
        .await;
        assert_eq!(ok.code, 0);
        assert!(ok.out[0].contains("accepted #7"));

        let server2 = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/8/complete"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Cannot complete task", "status": 422
            })))
            .mount(&server2)
            .await;
        let stale = dispatch(
            &client(&server2),
            Some(InboxCmd::Accept { id: 8 }),
            false,
            false,
        )
        .await;
        assert_eq!(stale.code, 1);
        assert!(stale.err[0].contains("already moved on"));
    }
}
