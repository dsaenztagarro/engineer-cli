//! Headless `engineer timer` — the one-shot twin of every timer verb
//! (docs/designs/timer.dc.html §Headless / §Headless contract).
//!
//! Exit codes answer "is the clock counting?": 0 counting (running, focus
//! work) · 1 nothing running · 3 idle, reclaim pending · 4 not counting
//! (paused / focus break). Write verbs exit 0 on success and 1 on refusal,
//! with the reason on stderr. Output is plain when piped: ANSI colour is
//! applied only on a TTY and never when NO_COLOR is set.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;

use crate::api::{ApiClient, ApiError, Timer};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::ui::widgets::fmt_elapsed;

#[derive(Args)]
pub struct TimerArgs {
    /// Emit the full read as JSON instead of the human line.
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    cmd: Option<TimerCmd>,
}

#[derive(Subcommand)]
enum TimerCmd {
    /// Plain one-line status: `<state> <elapsed_s> <mode> <id> <kind> "<title>"`.
    Status {
        /// Status-bar form: glyph + clock only (empty when nothing runs).
        #[arg(long)]
        short: bool,
    },
    /// Start a timer — fuzzy-binds to the best activity match, or starts
    /// unnamed when no query is given.
    Start {
        query: Option<String>,
        /// Stop & save the running timer first instead of refusing.
        #[arg(long)]
        switch: bool,
    },
    /// Pause ⇄ resume — the multiplexer-keybind form.
    Toggle,
    /// Pause the running timer.
    Pause,
    /// Resume the paused timer.
    Resume,
    /// Stop & save. Refuses on an unbound timer — bind or discard first.
    Stop,
    /// Bind the running unnamed timer to an existing activity.
    Bind { query: String },
    /// Throw the timer away, writing nothing. Past ~2 minutes requires --force.
    Discard {
        #[arg(long)]
        force: bool,
    },
}

pub async fn run(cfg: &Config, args: TimerArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let outcome = dispatch(&api, args.cmd, args.json, colored).await?;
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

async fn dispatch(
    api: &ApiClient,
    cmd: Option<TimerCmd>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    match cmd {
        None => read(api, json, colored).await,
        Some(TimerCmd::Status { short }) => status(api, short, colored).await,
        Some(TimerCmd::Start { query, switch }) => start(api, query, switch, colored).await,
        Some(TimerCmd::Toggle) => toggle(api, colored).await,
        Some(TimerCmd::Pause) => pause(api, colored).await,
        Some(TimerCmd::Resume) => resume(api, colored).await,
        Some(TimerCmd::Stop) => stop(api).await,
        Some(TimerCmd::Bind { query }) => bind(api, &query).await,
        Some(TimerCmd::Discard { force }) => discard(api, force).await,
    }
}

// ---------------------------------------------------------------- reads

async fn read(api: &ApiClient, json: bool, colored: bool) -> Result<Outcome, ApiError> {
    let timer = api.timer().await?;
    let code = exit_code(&timer);
    let line = if json {
        json_read(&timer).to_string()
    } else {
        human_line(&timer, colored)
    };
    Ok(Outcome {
        out: vec![line],
        err: vec![],
        code,
    })
}

async fn status(api: &ApiClient, short: bool, colored: bool) -> Result<Outcome, ApiError> {
    let timer = api.timer().await?;
    let code = exit_code(&timer);
    let line = if short {
        short_status(&timer, colored)
    } else {
        plain_status(&timer)
    };
    Ok(Outcome {
        out: if line.is_empty() { vec![] } else { vec![line] },
        err: vec![],
        code,
    })
}

// ---------------------------------------------------------------- writes

async fn start(
    api: &ApiClient,
    query: Option<String>,
    switch: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let activity_id = match &query {
        None => None,
        Some(q) => match api.timer_candidates(Some(q)).await?.first() {
            Some(candidate) => Some(candidate.id),
            None => return Ok(Outcome::refuse(format!("no activity matches \"{q}\""))),
        },
    };

    // The saved-segment details of a switch live server-side; read the current
    // timer first so the output can at least name what was stopped.
    let previous = if switch {
        let t = api.timer().await?;
        t.running
            .then(|| t.label.unwrap_or_else(|| "untitled".into()))
    } else {
        None
    };

    match api.start_timer(activity_id, switch).await {
        Ok(timer) => {
            let mut out = Vec::new();
            if let Some(label) = previous {
                out.push(format!("■ stopped & saved {label}"));
            }
            out.push(format!(
                "{} started  0:00:00  {}",
                paint("●", COLOR_RUNNING, colored),
                timer.label.as_deref().unwrap_or("untitled")
            ));
            Ok(Outcome {
                out,
                err: vec![],
                code: 0,
            })
        }
        Err(ApiError::Problem { status: 409, .. }) => {
            let current = api.timer().await?;
            Ok(Outcome::refuse(format!(
                "already tracking {}  {} — rerun with --switch",
                current.label.as_deref().unwrap_or("untitled"),
                fmt_elapsed(current.elapsed_seconds.unwrap_or(0)),
            )))
        }
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

async fn toggle(api: &ApiClient, colored: bool) -> Result<Outcome, ApiError> {
    let timer = api.timer().await?;
    if !timer.running {
        return Ok(Outcome::refuse("nothing running"));
    }
    if timer.paused {
        resume(api, colored).await
    } else {
        pause(api, colored).await
    }
}

async fn pause(api: &ApiClient, colored: bool) -> Result<Outcome, ApiError> {
    match api.pause_timer().await {
        Ok(t) => Ok(Outcome::ok(format!(
            "{} paused at {}",
            paint("‖", COLOR_ATTENTION, colored),
            fmt_elapsed(t.elapsed_seconds.unwrap_or(0))
        ))),
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

async fn resume(api: &ApiClient, colored: bool) -> Result<Outcome, ApiError> {
    match api.resume_timer().await {
        Ok(t) => Ok(Outcome::ok(format!(
            "{} resumed  {}  {}",
            paint("●", COLOR_RUNNING, colored),
            fmt_elapsed(t.elapsed_seconds.unwrap_or(0)),
            t.label.as_deref().unwrap_or("untitled")
        ))),
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

async fn stop(api: &ApiClient) -> Result<Outcome, ApiError> {
    match api.stop_timer().await {
        Ok(stopped) => Ok(Outcome::ok(format!(
            "■ saved {}m (segment {})",
            stopped.minutes, stopped.segment_id
        ))),
        Err(ApiError::Problem { title, detail, .. }) => Ok(Outcome::refuse(format!(
            "{} — bind or discard first",
            problem_text(&title, &detail)
        ))),
        Err(e) => Err(e),
    }
}

async fn bind(api: &ApiClient, query: &str) -> Result<Outcome, ApiError> {
    let Some(candidate) = api.timer_candidates(Some(query)).await?.first().cloned() else {
        return Ok(Outcome::refuse(format!("no activity matches \"{query}\"")));
    };
    match api.bind_timer(Some(candidate.id), None).await {
        Ok(_) => Ok(Outcome::ok(format!("⚑ bound to {}", candidate.title))),
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

/// Discards ask twice in the TUI past ~2 minutes; headless, the second ask is
/// `--force`.
const DISCARD_CONFIRM_SECS: i64 = 120;

async fn discard(api: &ApiClient, force: bool) -> Result<Outcome, ApiError> {
    let timer = api.timer().await?;
    if !timer.running {
        return Ok(Outcome::refuse("nothing running"));
    }
    let elapsed = timer.elapsed_seconds.unwrap_or(0);
    if elapsed > DISCARD_CONFIRM_SECS && !force {
        return Ok(Outcome::refuse(format!(
            "discard {} of work? rerun with --force",
            fmt_elapsed(elapsed)
        )));
    }
    api.discard_timer().await?;
    Ok(Outcome::ok(format!(
        "✗ discarded {} — nothing written",
        fmt_elapsed(elapsed)
    )))
}

// ---------------------------------------------------------------- shapes

/// One word per state, the first token of the status contract. Precedence:
/// gone > idle > paused > focus phase > running.
fn state_word(t: &Timer) -> &'static str {
    if !t.running {
        return "none";
    }
    if t.idle == Some(true) {
        return "idle";
    }
    if t.paused {
        return "paused";
    }
    match (t.mode.as_deref(), t.phase.as_deref()) {
        (Some("focus"), Some("break")) => "break",
        (Some("focus"), _) => "work",
        _ => "running",
    }
}

fn exit_code(t: &Timer) -> i32 {
    match state_word(t) {
        "none" => 1,
        "idle" => 3,
        "paused" | "break" => 4,
        _ => 0,
    }
}

/// `<state> <elapsed_s> <mode> <activity_id> <kind> "<title>" [extras]` —
/// field order never changes; unbound uses `-` placeholders. The API read
/// carries no activity kind, so that column is always `-` until it does.
fn plain_status(t: &Timer) -> String {
    let word = state_word(t);
    if word == "none" {
        return word.into();
    }
    let mut line = format!(
        "{word} {} {} {} - \"{}\"",
        t.elapsed_seconds.unwrap_or(0),
        t.mode.as_deref().unwrap_or("stopwatch"),
        t.activity_id
            .map_or_else(|| "-".into(), |id| id.to_string()),
        t.label.as_deref().unwrap_or_default(),
    );
    if let ("work" | "break", Some(n)) = (word, t.intervals_completed) {
        line.push_str(&format!(" intervals_completed={n}"));
    }
    line
}

fn short_status(t: &Timer, colored: bool) -> String {
    let word = state_word(t);
    if word == "none" {
        return String::new();
    }
    let (glyph, color) = glyph_for(word);
    format!(
        "{} {}",
        paint(glyph, color, colored),
        fmt_elapsed(t.elapsed_seconds.unwrap_or(0))
    )
}

fn human_line(t: &Timer, colored: bool) -> String {
    let word = state_word(t);
    let (glyph, color) = glyph_for(word);
    let glyph = paint(glyph, color, colored);
    if word == "none" {
        return format!("{glyph} nothing running");
    }
    let elapsed = fmt_elapsed(t.elapsed_seconds.unwrap_or(0));
    let title = t.label.as_deref().unwrap_or("untitled");
    let verb = match word {
        "running" => "tracking",
        "work" => "focus",
        "idle" => "idle — reclaim pending",
        other => other,
    };
    let since = t
        .started_at
        .map(|ts| {
            let local = ts.to_zoned(jiff::tz::TimeZone::system());
            format!("  (since {})", local.strftime("%H:%M"))
        })
        .unwrap_or_default();
    format!("{glyph} {verb}  {elapsed}  {title}{since}")
}

fn json_read(t: &Timer) -> serde_json::Value {
    let word = state_word(t);
    if word == "none" {
        return serde_json::json!({ "state": "none" });
    }
    serde_json::json!({
        "state": word,
        "mode": t.mode.as_deref().unwrap_or("stopwatch"),
        "phase": t.phase,
        "intervals_completed": t.intervals_completed,
        "activity": t.activity_id.map(|id| serde_json::json!({
            "id": id,
            "title": t.label,
        })),
        "started_at": t.started_at.map(|ts| ts.to_string()),
        "last_input_at": t.last_input_at.map(|ts| ts.to_string()),
        "elapsed_s": t.elapsed_seconds.unwrap_or(0),
        "idle": t.idle.unwrap_or(false),
    })
}

// Terminal-palette 256 colours (docs/designs/README.md palette mapping).
const COLOR_RUNNING: u8 = 108; // success green
const COLOR_FOCUS: u8 = 105; // accent indigo
const COLOR_ATTENTION: u8 = 179; // warn amber
const COLOR_MUTED: u8 = 244;

fn glyph_for(word: &str) -> (&'static str, u8) {
    match word {
        "paused" => ("‖", COLOR_ATTENTION),
        "idle" => ("◐", COLOR_ATTENTION),
        "work" => ("◆", COLOR_FOCUS),
        "break" | "none" => ("○", COLOR_MUTED),
        _ => ("●", COLOR_RUNNING),
    }
}

fn paint(s: &str, color: u8, colored: bool) -> String {
    if colored {
        format!("\x1b[38;5;{color}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn problem_text(title: &str, detail: &str) -> String {
    if detail.is_empty() {
        title.to_string()
    } else {
        detail.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn timer(json: serde_json::Value) -> Timer {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn state_words_cover_every_face() {
        assert_eq!(
            state_word(&timer(serde_json::json!({"running": false}))),
            "none"
        );
        assert_eq!(
            state_word(&timer(serde_json::json!({"running": true}))),
            "running"
        );
        assert_eq!(
            state_word(&timer(serde_json::json!({"running": true, "paused": true}))),
            "paused"
        );
        // Idle outranks paused: a reclaim decision is pending either way.
        assert_eq!(
            state_word(&timer(
                serde_json::json!({"running": true, "paused": true, "idle": true})
            )),
            "idle"
        );
        assert_eq!(
            state_word(&timer(
                serde_json::json!({"running": true, "mode": "focus", "phase": "work"})
            )),
            "work"
        );
        assert_eq!(
            state_word(&timer(
                serde_json::json!({"running": true, "mode": "focus", "phase": "break"})
            )),
            "break"
        );
    }

    #[test]
    fn exit_codes_answer_is_it_counting() {
        let code = |v| exit_code(&timer(v));
        assert_eq!(code(serde_json::json!({"running": false})), 1);
        assert_eq!(code(serde_json::json!({"running": true})), 0);
        assert_eq!(
            code(serde_json::json!({"running": true, "mode": "focus", "phase": "work"})),
            0
        );
        assert_eq!(code(serde_json::json!({"running": true, "idle": true})), 3);
        assert_eq!(
            code(serde_json::json!({"running": true, "paused": true})),
            4
        );
        assert_eq!(
            code(serde_json::json!({"running": true, "mode": "focus", "phase": "break"})),
            4
        );
    }

    #[test]
    fn plain_status_uses_placeholders_when_unbound() {
        let line = plain_status(&timer(serde_json::json!({
            "running": true, "elapsed_seconds": 431
        })));
        assert_eq!(line, "running 431 stopwatch - - \"\"");
    }

    #[test]
    fn plain_status_carries_focus_extras() {
        let line = plain_status(&timer(serde_json::json!({
            "running": true, "elapsed_seconds": 1928, "mode": "focus",
            "phase": "work", "intervals_completed": 2,
            "activity_id": 9, "label": "Implement Raft"
        })));
        assert_eq!(
            line,
            "work 1928 focus 9 - \"Implement Raft\" intervals_completed=2"
        );
    }

    #[test]
    fn short_status_is_empty_when_nothing_runs() {
        assert_eq!(
            short_status(&timer(serde_json::json!({"running": false})), false),
            ""
        );
        assert_eq!(
            short_status(
                &timer(serde_json::json!({"running": true, "elapsed_seconds": 3134})),
                false
            ),
            "● 52:14"
        );
    }

    #[tokio::test]
    async fn json_read_reports_state_and_elapsed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9,
                "label": "Read DDIA ch.7", "elapsed_seconds": 3134
            })))
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), None, true, false).await.unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["state"], "running");
        assert_eq!(v["elapsed_s"], 3134);
        assert_eq!(v["activity"]["id"], 9);
    }

    #[tokio::test]
    async fn start_fuzzy_binds_via_candidates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/candidates"))
            .and(query_param("q", "raft"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": 42, "title": "Implement Raft leader election" }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(serde_json::json!({ "activity_id": 42 })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 42,
                "label": "Implement Raft leader election"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(
            &client(&server),
            Some(TimerCmd::Start {
                query: Some("raft".into()),
                switch: false,
            }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("started"));
    }

    #[tokio::test]
    async fn start_conflict_refuses_and_names_the_running_timer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "title": "Conflict", "detail": "a timer is already running"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "label": "Read DDIA ch.7", "elapsed_seconds": 3134
            })))
            .mount(&server)
            .await;

        let outcome = dispatch(
            &client(&server),
            Some(TimerCmd::Start {
                query: None,
                switch: false,
            }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("already tracking Read DDIA ch.7"));
        assert!(outcome.err[0].contains("--switch"));
    }

    #[tokio::test]
    async fn toggle_resumes_a_paused_timer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": true, "elapsed_seconds": 600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/resume"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": false, "elapsed_seconds": 600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), Some(TimerCmd::Toggle), false, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("resumed"));
    }

    #[tokio::test]
    async fn discard_past_two_minutes_requires_force() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "elapsed_seconds": 2460
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0) // refused before the delete
            .mount(&server)
            .await;

        let outcome = dispatch(
            &client(&server),
            Some(TimerCmd::Discard { force: false }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("--force"));
    }

    #[tokio::test]
    async fn discard_force_deletes_the_timer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "elapsed_seconds": 2460
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(
            &client(&server),
            Some(TimerCmd::Discard { force: true }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.out[0].contains("discarded"));
    }

    #[tokio::test]
    async fn stop_unbound_refuses_with_a_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/stop"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Unprocessable", "detail": "timer is not bound to an activity"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), Some(TimerCmd::Stop), false, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("bind or discard first"));
    }
}
