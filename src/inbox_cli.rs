//! Headless `engineer inbox` — triage the assisted-capture drafts
//! (assisted-capture.brief.md). Bare `inbox` lists the pending drafts;
//! `accept` / `reject` / `ack` act on one; `show` reads one in full. `--json`
//! for machines, plain otherwise, exit 0 on success, 1 on refusal — the
//! `engineer timer`/`target` contract. Accepting writes the activity (the
//! server's `complete`), so a stale draft is a `422` surfaced as "already
//! moved on", not a crash.

use std::io::{IsTerminal, Write};

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;
use jiff::Timestamp;

use crate::api::{ApiClient, ApiError, CaptureSource, Task};
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
    /// List the capture sources (git / calendar) and their connect state.
    Sources,
    /// Connect a source — prints its trust statement first, then connects. Needs
    /// `--yes` non-interactively, or confirms on a TTY. The calendar takes
    /// `--feed-url`.
    Connect {
        /// The source key (`git` or `calendar`).
        key: String,
        #[arg(long = "feed-url")]
        feed_url: Option<String>,
        /// Skip the confirm — connect straight away (required off a TTY).
        #[arg(long)]
        yes: bool,
    },
    /// Disconnect a source — stops new drafts; captured drafts are kept.
    Disconnect { key: String },
    /// Enqueue a scan for a connected source.
    Sync { key: String },
}

pub async fn run(cfg: &Config, args: InboxArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    // The interactive connect confirm can't be buffered — the trust statement
    // must be shown *and flushed* before the y/N is read (capture-is-sacred: you
    // see what you're opting into first) — so it's handled ahead of `dispatch`.
    // Off a TTY, or with `--yes`, it falls through to the buffered `dispatch`.
    if let Some(InboxCmd::Connect {
        key,
        feed_url,
        yes: false,
    }) = &args.cmd
    {
        if std::io::stdin().is_terminal() {
            return connect_interactive(&api, key, feed_url.as_deref(), args.json, colored).await;
        }
    }

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

    /// Keep the informational `out` lines (e.g. the trust statement) but refuse
    /// with a reason on stderr and exit 1 — the honest "shown, but not done".
    fn refuse_after(out: Vec<String>, reason: impl Into<String>) -> Self {
        Self {
            out,
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
        Some(InboxCmd::Sources) => sources(api, json, colored).await,
        // `yes` is the resolved gate here: `run` routes an interactive TTY
        // confirm to `connect_interactive` and only reaches this arm
        // non-interactively (so `--yes` is the whole gate).
        Some(InboxCmd::Connect { key, feed_url, yes }) => {
            connect_dispatch(api, &key, feed_url.as_deref(), yes, json, colored).await
        }
        Some(InboxCmd::Disconnect { key }) => disconnect(api, &key, json, colored).await,
        Some(InboxCmd::Sync { key }) => sync(api, &key, json, colored).await,
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

// ---- the git-source connect flow (`/api/v1/capture/sources`, ADR 0035) -------

/// `engineer inbox sources` — the capture sources with their connect state and
/// the plain-language trust lines, so a terminal user sees what each source
/// reads before wiring anything.
async fn sources(api: &ApiClient, json: bool, colored: bool) -> Outcome {
    let sources = match api.list_capture_sources().await {
        Ok(s) => s,
        Err(e) => return Outcome::refuse(problem_reason(e)),
    };
    if json {
        return Outcome::ok(to_json(&sources));
    }
    if sources.is_empty() {
        return Outcome::ok("no capture sources");
    }
    let mut out = Vec::new();
    for s in &sources {
        out.push(format!(
            "{}  {}",
            paint(&source_state(s), state_color(s), colored),
            s.name
        ));
        // The trust `reads` line, or — for a git source that needs GitHub — the
        // requirement pointer, rendered honestly rather than as a bare "off".
        match &s.requirement {
            Some(req) => {
                let mut hint = format!("    {}", req.detail);
                if let Some(url) = &req.url {
                    hint.push_str(&format!(" → {url}"));
                }
                out.push(paint(&hint, COLOR_MUTED, colored));
            }
            None => out.push(paint(
                &format!("    reads {}", s.trust.reads),
                COLOR_MUTED,
                colored,
            )),
        }
    }
    Outcome::lines(out)
}

/// `engineer inbox connect <key>` (non-interactive) — prints the trust statement,
/// then connects if `proceed` (the `--yes` gate) is set and the source is
/// connectable. A git source with no GitHub connection prints the requirement
/// pointer and refuses; a bad calendar feed URL surfaces the server's `422`.
async fn connect_dispatch(
    api: &ApiClient,
    key: &str,
    feed_url: Option<&str>,
    proceed: bool,
    json: bool,
    colored: bool,
) -> Outcome {
    let source = match load_source(api, key).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Outcome::refuse(format!("no such source: {key} (try `git` or `calendar`)"))
        }
        Err(e) => return refuse_problem(e),
    };

    // The trust statement, rendered verbatim BEFORE connecting (human output;
    // `--json` callers read it from `sources --json`).
    let mut out = if json {
        Vec::new()
    } else {
        trust_lines(&source, colored)
    };

    if !source.connectable {
        out.extend(requirement_lines(&source, colored));
        return Outcome::refuse_after(
            out,
            format!(
                "{} can't connect yet — connect GitHub on the web first",
                source.name
            ),
        );
    }
    if !proceed {
        return Outcome::refuse_after(
            out,
            "not connected — re-run with --yes to confirm (trust statement above)",
        );
    }

    match api.connect_capture_source(key, feed_url).await {
        Ok(s) if json => Outcome::ok(to_json(&s)),
        Ok(s) => {
            out.push(format!(
                "{} connected · {} — drafts flow into your inbox",
                paint("●", COLOR_OK, colored),
                s.name
            ));
            Outcome::lines(out)
        }
        Err(e) => Outcome::refuse_after(out, connect_problem(e)),
    }
}

/// The interactive TTY confirm: show the trust statement, ask, then connect on a
/// yes. Kept out of `dispatch` because the trust must be flushed *before* the
/// read — a buffered outcome would prompt before showing what you're opting into.
async fn connect_interactive(
    api: &ApiClient,
    key: &str,
    feed_url: Option<&str>,
    json: bool,
    colored: bool,
) -> Result<i32> {
    let source = match load_source(api, key).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("no such source: {key} (try `git` or `calendar`)");
            return Ok(1);
        }
        Err(e) => {
            eprintln!("{}", problem_reason(e));
            return Ok(1);
        }
    };

    for line in trust_lines(&source, colored) {
        println!("{line}");
    }
    if !source.connectable {
        for line in requirement_lines(&source, colored) {
            println!("{line}");
        }
        eprintln!(
            "{} can't connect yet — connect GitHub on the web first",
            source.name
        );
        return Ok(1);
    }

    print!("Connect {}? [y/N] ", source.name);
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok();
    let yes = matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !yes {
        println!("cancelled — nothing connected");
        return Ok(0);
    }

    let outcome = match api.connect_capture_source(key, feed_url).await {
        Ok(s) if json => Outcome::ok(to_json(&s)),
        Ok(s) => Outcome::ok(format!(
            "{} connected · {} — drafts flow into your inbox",
            paint("●", COLOR_OK, colored),
            s.name
        )),
        Err(e) => Outcome::refuse(connect_problem(e)),
    };
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

/// `engineer inbox disconnect <key>` — turns the source off. Drafts already
/// captured survive (disconnect ≠ delete), and the confirmation says so.
async fn disconnect(api: &ApiClient, key: &str, json: bool, colored: bool) -> Outcome {
    match api.disconnect_capture_source(key).await {
        Ok(s) if json => Outcome::ok(to_json(&s)),
        Ok(s) => Outcome::ok(format!(
            "{} disconnected · {} — captured drafts kept",
            paint("■", COLOR_MUTED, colored),
            s.name
        )),
        Err(e) => refuse_problem(e),
    }
}

/// `engineer inbox sync <key>` — enqueues a scan for a connected source; a
/// disconnected source is the server's honest `422`.
async fn sync(api: &ApiClient, key: &str, json: bool, colored: bool) -> Outcome {
    match api.sync_capture_source(key).await {
        Ok(q) if json => Outcome::ok(to_json(&q)),
        Ok(q) => Outcome::ok(format!(
            "{} sync queued · {}",
            paint("●", COLOR_ACCENT, colored),
            q.key
        )),
        Err(e) => refuse_problem(e),
    }
}

/// Read one source by key from the index (`None` when the key isn't one the
/// server serves — the route fences `git|calendar`, but a typo still shouldn't
/// crash).
async fn load_source(api: &ApiClient, key: &str) -> Result<Option<CaptureSource>, ApiError> {
    let sources = api.list_capture_sources().await?;
    Ok(sources.into_iter().find(|s| s.key == key))
}

/// The plain-language trust statement — `reads` / `never_reads` / `promise`,
/// rendered verbatim from the payload (the promise is the feature; the client
/// never invents its own wording).
fn trust_lines(source: &CaptureSource, colored: bool) -> Vec<String> {
    vec![
        paint(
            &format!("{} · what it reads", source.name),
            COLOR_MUTED,
            colored,
        ),
        format!("  reads        {}", source.trust.reads),
        format!("  never reads  {}", source.trust.never_reads),
        format!("  promise      {}", source.trust.promise),
    ]
}

/// The requirement pointer — GitHub isn't connected, so the source can't connect
/// over the API; render the server's detail and the web URL, not a bare failure.
fn requirement_lines(source: &CaptureSource, colored: bool) -> Vec<String> {
    let Some(req) = &source.requirement else {
        return Vec::new();
    };
    let mut lines = vec![paint(
        &format!("  needs {}", req.detail),
        COLOR_WARN,
        colored,
    )];
    if let Some(url) = &req.url {
        lines.push(paint(
            &format!("  connect it on the web → {url}"),
            COLOR_MUTED,
            colored,
        ));
    }
    lines
}

/// A connect `422`'s honest one-liner: the problem detail, plus any field errors
/// (a bad calendar feed URL), or the title as a fallback.
fn connect_problem(e: ApiError) -> String {
    if let ApiError::Problem { detail, errors, .. } = &e {
        let fields: Vec<String> = errors
            .iter()
            .map(|f| format!("{}: {}", f.field, f.detail))
            .collect();
        if !fields.is_empty() {
            let head = if detail.is_empty() { "refused" } else { detail };
            return format!("{head} ({})", fields.join(", "));
        }
    }
    problem_reason(e)
}

/// `● connected` / `⚠ needs GitHub` / `○ not connected` — the one-word source
/// state, in the state colour.
fn source_state(s: &CaptureSource) -> String {
    if s.connected {
        "● connected".to_string()
    } else if !s.connectable {
        "⚠ needs GitHub".to_string()
    } else {
        "○ not connected".to_string()
    }
}

fn state_color(s: &CaptureSource) -> u8 {
    if s.connected {
        COLOR_OK
    } else if !s.connectable {
        COLOR_WARN
    } else {
        COLOR_MUTED
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

/// The honest one-line reason for a failed capture-source call — the problem
/// detail (the connect requirement, the sync-not-connected `422`), or a clear
/// offline line for a transport failure (the flow is live-only).
fn problem_reason(e: ApiError) -> String {
    match e {
        ApiError::Unauthorized => "not authenticated — run `engineer login`".to_string(),
        ApiError::Transport(_) => "offline — the server is unreachable; retry online".to_string(),
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
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into())
    }

    fn git_source(connected: bool, connectable: bool) -> serde_json::Value {
        let requirement = if connectable {
            serde_json::Value::Null
        } else {
            serde_json::json!({
                "kind": "github_connection",
                "detail": "Connect GitHub first — the scan uses your own connection.",
                "url": "https://engineer.example/github/connect"
            })
        };
        serde_json::json!({
            "key": "git", "name": "Git activity",
            "connected": connected, "connectable": connectable, "requirement": requirement,
            "trust": {
                "reads": "Commit times and counts on repositories your activities anchor.",
                "never_reads": "Never messages, never code.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": []
        })
    }

    fn calendar_source(connected: bool) -> serde_json::Value {
        serde_json::json!({
            "key": "calendar", "name": "Study calendar",
            "connected": connected, "connectable": true, "requirement": null,
            "trust": {
                "reads": "The titles and times of past events on this one calendar.",
                "never_reads": "Never descriptions, never attendees.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": ["feed_url"]
        })
    }

    /// Mount the sources index returning the given sources.
    async fn mount_sources(server: &MockServer, sources: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/capture/sources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": sources
            })))
            .mount(server)
            .await;
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

    // ---- the git-source connect flow ----

    #[tokio::test]
    async fn sources_lists_state_and_the_trust_reads_line() {
        let server = MockServer::start().await;
        mount_sources(
            &server,
            serde_json::json!([git_source(false, false), calendar_source(true)]),
        )
        .await;
        let out = dispatch(&client(&server), Some(InboxCmd::Sources), false, false).await;
        assert_eq!(out.code, 0);
        let text = out.out.join("\n");
        // The un-connectable git source shows the requirement pointer, not a bare
        // "reads" line; the connected calendar shows its state + trust.
        assert!(text.contains("needs GitHub"), "git state: {text}");
        assert!(text.contains("Connect GitHub first"), "requirement: {text}");
        assert!(
            text.contains("engineer.example/github/connect"),
            "url: {text}"
        );
        assert!(
            text.contains("● connected") && text.contains("Study calendar"),
            "cal: {text}"
        );
        assert!(
            text.contains("reads The titles and times"),
            "cal trust: {text}"
        );
    }

    #[tokio::test]
    async fn connect_without_yes_prints_the_trust_and_refuses_without_posting() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([git_source(false, true)])).await;
        // No POST mock: a hit would 404 and change the assertions — the gate must
        // stop before any write.
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "git".into(),
                feed_url: None,
                yes: false,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 1);
        // The trust statement is printed before the gate refuses.
        let trust = out.out.join("\n");
        assert!(trust.contains("Commit times and counts"), "reads: {trust}");
        assert!(
            trust.contains("Never messages, never code"),
            "never_reads: {trust}"
        );
        assert!(
            trust.contains("nothing counts until you say so"),
            "promise: {trust}"
        );
        assert!(out.err[0].contains("--yes"), "gate: {:?}", out.err);
    }

    #[tokio::test]
    async fn connect_with_yes_prints_trust_then_connects() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([git_source(false, true)])).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(git_source(true, true)))
            .expect(1)
            .mount(&server)
            .await;
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "git".into(),
                feed_url: None,
                yes: true,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 0);
        let text = out.out.join("\n");
        assert!(
            text.contains("Commit times and counts"),
            "trust still shown: {text}"
        );
        assert!(text.contains("connected · Git activity"), "confirm: {text}");
    }

    #[tokio::test]
    async fn connect_git_without_github_shows_the_requirement_and_refuses() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([git_source(false, false)])).await;
        // Even with --yes, an un-connectable source never POSTs — it points at
        // the web. No POST mock: a hit would surface as an unexpected request.
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "git".into(),
                feed_url: None,
                yes: true,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 1);
        let text = out.out.join("\n");
        assert!(
            text.contains("Connect GitHub first"),
            "requirement detail: {text}"
        );
        assert!(
            text.contains("engineer.example/github/connect"),
            "url: {text}"
        );
        assert!(
            out.err[0].contains("connect GitHub on the web first"),
            "refusal: {:?}",
            out.err
        );
    }

    #[tokio::test]
    async fn connect_calendar_sends_the_feed_url() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([calendar_source(false)])).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/connect"))
            .and(body_json(
                serde_json::json!({ "feed_url": "https://cal.example/basic.ics" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(calendar_source(true)))
            .expect(1)
            .mount(&server)
            .await;
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "calendar".into(),
                feed_url: Some("https://cal.example/basic.ics".into()),
                yes: true,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 0);
        assert!(out.out.join("\n").contains("connected · Study calendar"));
    }

    #[tokio::test]
    async fn connect_calendar_bad_feed_url_surfaces_the_validation_detail() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([calendar_source(false)])).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/connect"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/validation",
                "title": "Validation failed", "status": 422,
                "detail": "Feed url is invalid",
                "errors": [ { "field": "calendar_feed_url", "detail": "must be https" } ]
            })))
            .mount(&server)
            .await;
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "calendar".into(),
                feed_url: Some("http://insecure.example/cal.ics".into()),
                yes: true,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 1);
        assert!(
            out.err[0].contains("calendar_feed_url") && out.err[0].contains("must be https"),
            "field error surfaced: {:?}",
            out.err
        );
    }

    #[tokio::test]
    async fn connect_unknown_key_refuses() {
        let server = MockServer::start().await;
        mount_sources(&server, serde_json::json!([git_source(false, true)])).await;
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Connect {
                key: "slack".into(),
                feed_url: None,
                yes: true,
            }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("no such source"), "{:?}", out.err);
    }

    #[tokio::test]
    async fn disconnect_confirms_drafts_are_kept() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(git_source(false, true)))
            .expect(1)
            .mount(&server)
            .await;
        let out = dispatch(
            &client(&server),
            Some(InboxCmd::Disconnect { key: "git".into() }),
            false,
            false,
        )
        .await;
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("disconnected") && out.out[0].contains("kept"));
    }

    #[tokio::test]
    async fn sync_connected_queues_and_disconnected_refuses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/sync"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "queued": true, "key": "calendar"
            })))
            .mount(&server)
            .await;
        let ok = dispatch(
            &client(&server),
            Some(InboxCmd::Sync {
                key: "calendar".into(),
            }),
            false,
            false,
        )
        .await;
        assert_eq!(ok.code, 0);
        assert!(ok.out[0].contains("sync queued"));

        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/sync"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Source not connected", "status": 422,
                "detail": "Connect the Git activity source before syncing it."
            })))
            .mount(&server2)
            .await;
        let refused = dispatch(
            &client(&server2),
            Some(InboxCmd::Sync { key: "git".into() }),
            false,
            false,
        )
        .await;
        assert_eq!(refused.code, 1);
        assert!(
            refused.err[0].contains("before syncing"),
            "{:?}",
            refused.err
        );
    }
}
