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

use crate::api::{ApiClient, ApiError, ReclaimVerb, Reclaimed, Timer};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::queue::QueuedClient;
use crate::ui::widgets::fmt_elapsed;

#[derive(Args)]
pub struct TimerArgs {
    /// Emit JSON instead of the human line (valid on the bare read and on
    /// `status`/`settings`).
    #[arg(long, global = true)]
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
    Stop {
        /// With an idle tail pending: how to reclaim it first —
        /// trim | keep | stop (stop ends the segment at the last input).
        #[arg(long)]
        reclaim: Option<String>,
    },
    /// Apply an idle-tail decision: trim (idle → paused time, keeps
    /// running) · keep (the tail counts) · stop (save up to last input, ends).
    Reclaim { verb: String },
    /// Bind the running unnamed timer to an existing activity.
    Bind { query: String },
    /// Throw the timer away, writing nothing. Past ~2 minutes requires --force.
    Discard {
        #[arg(long)]
        force: bool,
    },
    /// The per-user timer knobs, read-only (edit on the web).
    Settings,
}

pub async fn run(cfg: &Config, args: TimerArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

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
    queued: &QueuedClient<'_>,
    cmd: Option<TimerCmd>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    match cmd {
        None => read(api, queued, json, colored).await,
        Some(TimerCmd::Status { short }) => status(api, queued, short, colored).await,
        Some(TimerCmd::Start { query, switch }) => start(api, query, switch, colored).await,
        Some(TimerCmd::Toggle) => toggle(api, queued, colored).await,
        Some(TimerCmd::Pause) => pause(queued, colored).await,
        Some(TimerCmd::Resume) => resume(queued, colored).await,
        Some(TimerCmd::Stop { reclaim }) => stop(api, reclaim).await,
        Some(TimerCmd::Reclaim { verb }) => reclaim(api, &verb, json).await,
        Some(TimerCmd::Bind { query }) => bind(api, &query).await,
        Some(TimerCmd::Discard { force }) => discard(api, force).await,
        Some(TimerCmd::Settings) => settings(api, json).await,
    }
}

/// `reclaim <verb>` — the §Idle reclaim rows, piped. Also the engine behind
/// `stop --reclaim=…`.
async fn reclaim(api: &ApiClient, verb_name: &str, json: bool) -> Result<Outcome, ApiError> {
    let Some(verb) = ReclaimVerb::from_name(verb_name) else {
        return Ok(Outcome::refuse(format!(
            "unknown reclaim verb \"{verb_name}\" — trim | keep | stop"
        )));
    };
    match api.reclaim_timer(verb).await {
        Ok(Reclaimed::Running(t)) => {
            if json {
                let value = serde_json::json!({
                    "verb": verb.as_str(),
                    "state": state_word(&t),
                    "elapsed_s": t.elapsed_seconds.unwrap_or(0),
                    "paused_s": t.paused_seconds.unwrap_or(0),
                });
                return Ok(Outcome::ok(value.to_string()));
            }
            Ok(Outcome::ok(match verb {
                ReclaimVerb::Trim => format!(
                    "● trimmed — idle moved to paused time · {} still running",
                    fmt_elapsed(t.elapsed_seconds.unwrap_or(0))
                ),
                _ => format!(
                    "● kept — the tail counts · {} still running",
                    fmt_elapsed(t.elapsed_seconds.unwrap_or(0))
                ),
            }))
        }
        Ok(Reclaimed::Stopped(s)) => {
            if json {
                let value = serde_json::json!({
                    "verb": "stop",
                    "saved_segment_minutes": s.minutes,
                    "segment_id": s.segment_id,
                });
                return Ok(Outcome::ok(value.to_string()));
            }
            Ok(Outcome::ok(format!(
                "■ saved {}m (segment {}) — ended at the last input",
                s.minutes, s.segment_id
            )))
        }
        Err(ApiError::Problem { status: 404, .. }) => Ok(Outcome::refuse("nothing running")),
        Err(ApiError::Problem { title, detail, .. }) => Ok(Outcome::refuse(format!(
            "{} — bind or discard first",
            problem_text(&title, &detail)
        ))),
        Err(e) => Err(e),
    }
}

/// The knobs read, mirroring the server payload 1:1 in `--json` and as an
/// aligned table for humans.
async fn settings(api: &ApiClient, json: bool) -> Result<Outcome, ApiError> {
    let s = api.timer_settings().await?;
    if json {
        let value = serde_json::json!({
            "timer_mode": s.timer_mode,
            "focus_work_minutes": s.focus_work_minutes,
            "focus_short_break_minutes": s.focus_short_break_minutes,
            "focus_long_break_minutes": s.focus_long_break_minutes,
            "focus_long_break_every": s.focus_long_break_every,
            "idle_guard_enabled": s.idle_guard_enabled,
            "idle_threshold_minutes": s.idle_threshold_minutes,
            "idle_default_reclaim": s.idle_default_reclaim,
            "audit_long_hours": s.audit_long_hours,
            "audit_short_seconds": s.audit_short_seconds,
            "audit_badge_enabled": s.audit_badge_enabled,
            "overrun_ping_enabled": s.overrun_ping_enabled,
        });
        return Ok(Outcome::ok(value.to_string()));
    }
    let onoff = |b: bool| if b { "on" } else { "off" };
    Ok(Outcome {
        out: vec![
            format!("default mode          {}", s.timer_mode),
            format!(
                "focus                 {}m work · {}m short · {}m long · long every {}th",
                s.focus_work_minutes,
                s.focus_short_break_minutes,
                s.focus_long_break_minutes,
                s.focus_long_break_every
            ),
            format!(
                "idle guard            {} · {}m threshold · default reclaim {}",
                onoff(s.idle_guard_enabled),
                s.idle_threshold_minutes,
                s.idle_default_reclaim
            ),
            format!(
                "audit                 ≥ {}h · < {}s · badge {}",
                s.audit_long_hours,
                s.audit_short_seconds,
                onoff(s.audit_badge_enabled)
            ),
            format!("overrun ping          {}", onoff(s.overrun_ping_enabled)),
            "read-only here — edit at engineer › Settings on the web".to_string(),
        ],
        err: vec![],
        code: 0,
    })
}

// ---------------------------------------------------------------- reads

/// Read the live timer, caching it on success; on a *transport* failure fall
/// back to the last-known cached value (the offline status-bar case). Auth and
/// other errors propagate. Returns the age in seconds when the value is stale.
async fn fetch_timer(api: &ApiClient) -> Result<(Timer, Option<i64>), ApiError> {
    match api.timer().await {
        Ok(t) => {
            crate::timer_cache::store(&t);
            Ok((t, None))
        }
        Err(ApiError::Transport(msg)) => match crate::timer_cache::load() {
            Some(stale) => Ok((stale.timer, Some(stale.age_secs))),
            None => Err(ApiError::Transport(msg)),
        },
        Err(e) => Err(e),
    }
}

async fn read(
    api: &ApiClient,
    queued: &QueuedClient<'_>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let (timer, stale) = fetch_timer(api).await?;
    let depth = queued.queue_summary().depth;
    let code = exit_code(&timer);
    let line = if json {
        let mut v = json_read(&timer);
        if let Some(age) = stale {
            v["stale"] = true.into();
            v["stale_age_s"] = age.into();
        }
        // Unsynced local writes are a different honesty state than a stale
        // read — both machine fields ship on every read (#100).
        v["queued"] = (depth > 0).into();
        v["queue_depth"] = depth.into();
        v.to_string()
    } else {
        let mut l = human_line(&timer, colored);
        if let Some(age) = stale {
            l.push_str(&stale_suffix(age, colored));
        }
        if depth > 0 {
            l.push_str(&paint(&format!("  · {depth} queued"), COLOR_MUTED, colored));
        }
        l
    };
    Ok(Outcome {
        out: vec![line],
        err: vec![],
        code,
    })
}

async fn status(
    api: &ApiClient,
    queued: &QueuedClient<'_>,
    short: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let (timer, stale) = fetch_timer(api).await?;
    let depth = queued.queue_summary().depth;
    let code = exit_code(&timer);
    let line = if short {
        let mut l = short_status(&timer, colored);
        // A stale status-bar clock wears a `~` so a glance still reads "offline".
        if !l.is_empty() && stale.is_some() {
            l.push_str(" ~");
        }
        // `↑N` — unsynced local writes, the quiet queued complication.
        if !l.is_empty() && depth > 0 {
            l.push(' ');
            l.push_str(&paint(&format!("↑{depth}"), COLOR_FOCUS, colored));
        }
        l
    } else {
        let mut l = plain_status(&timer);
        if let Some(age) = stale {
            if l != "none" {
                l.push_str(&format!(" stale_age_s={age}"));
            }
        }
        if depth > 0 {
            l.push_str(&format!(" queued={depth}"));
        }
        l
    };
    Ok(Outcome {
        out: if line.is_empty() { vec![] } else { vec![line] },
        err: vec![],
        code,
    })
}

/// `  · offline (last known 2m ago)` — the muted staleness tail on the human read.
fn stale_suffix(age_secs: i64, colored: bool) -> String {
    let ago = if age_secs >= 60 {
        format!("{}m", age_secs / 60)
    } else {
        format!("{age_secs}s")
    };
    paint(
        &format!("  · offline (last known {ago} ago)"),
        COLOR_MUTED,
        colored,
    )
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

async fn toggle(
    api: &ApiClient,
    queued: &QueuedClient<'_>,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let timer = api.timer().await?;
    if !timer.running {
        return Ok(Outcome::refuse("nothing running"));
    }
    if timer.paused {
        resume(queued, colored).await
    } else {
        pause(queued, colored).await
    }
}

/// `· queued (offline)` — the provisional tail on a write that landed in the
/// queue instead of on the server. Same honesty family as `stale_suffix`.
fn queued_suffix(colored: bool) -> String {
    paint("  · queued (offline)", COLOR_MUTED, colored)
}

async fn pause(queued: &QueuedClient<'_>, colored: bool) -> Result<Outcome, ApiError> {
    match queued.pause_timer().await {
        Ok(out) => {
            let t = out.value();
            let mut line = format!(
                "{} paused at {}",
                paint("‖", COLOR_ATTENTION, colored),
                fmt_elapsed(t.elapsed_seconds.unwrap_or(0))
            );
            if out.is_provisional() {
                line.push_str(&queued_suffix(colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

async fn resume(queued: &QueuedClient<'_>, colored: bool) -> Result<Outcome, ApiError> {
    match queued.resume_timer().await {
        Ok(out) => {
            let t = out.value();
            let mut line = format!(
                "{} resumed  {}  {}",
                paint("●", COLOR_RUNNING, colored),
                fmt_elapsed(t.elapsed_seconds.unwrap_or(0)),
                t.label.as_deref().unwrap_or("untitled")
            );
            if out.is_provisional() {
                line.push_str(&queued_suffix(colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(ApiError::Problem { title, detail, .. }) => {
            Ok(Outcome::refuse(problem_text(&title, &detail)))
        }
        Err(e) => Err(e),
    }
}

async fn stop(api: &ApiClient, reclaim_verb: Option<String>) -> Result<Outcome, ApiError> {
    // `--reclaim` decides the idle tail first: `stop` maps straight to the
    // reclaim endpoint (segment ends at last input); `trim`/`keep` settle the
    // tail, then the plain stop below saves to now.
    match reclaim_verb.as_deref() {
        None => {}
        Some("stop") => return reclaim(api, "stop", false).await,
        Some(other) => {
            let settled = reclaim(api, other, false).await?;
            if settled.code != 0 {
                return Ok(settled);
            }
        }
    }
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
/// `--force`. One shared fence with the Timer screen.
use crate::app::screens::timer::DISCARD_CONFIRM_SECS;

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
        _ if t.over => "over",
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
    if word == "idle" {
        if let Some(mark) = t.last_interacted_at {
            let idle_s = (jiff::Timestamp::now().as_second() - mark.as_second()).max(0);
            line.push_str(&format!(" idle_s={idle_s}"));
        }
    }
    if word == "over" {
        if let Some(planned) = t.planned_minutes {
            line.push_str(&format!(" planned_s={}", planned as i64 * 60));
        }
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
        "over" => "over — past the plan",
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
        "last_interacted_at": t.last_interacted_at.map(|ts| ts.to_string()),
        "elapsed_s": t.elapsed_seconds.unwrap_or(0),
        "idle": t.idle.unwrap_or(false),
        "over": t.over,
        "planned_minutes": t.planned_minutes,
        "logged_minutes": t.logged_minutes,
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
        "over" => ("●", COLOR_ATTENTION),
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

    /// A per-test scratch dir so the queue and read cache never touch the
    /// shared XDG state.
    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-timer-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn queued_at<'a>(api: &'a ApiClient, dir: &std::path::Path) -> QueuedClient<'a> {
        QueuedClient::with_paths(
            api,
            crate::queue::QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        )
    }

    /// `dispatch` with an isolated, empty queue — what most tests need.
    async fn run_dispatch(
        api: &ApiClient,
        cmd: Option<TimerCmd>,
        json: bool,
        colored: bool,
    ) -> Result<Outcome, ApiError> {
        let dir = scratch();
        let queued = queued_at(api, &dir);
        dispatch(api, &queued, cmd, json, colored).await
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
    fn over_outranks_running_but_not_breaks() {
        let over = timer(serde_json::json!({
            "running": true, "bound": true, "over": true,
            "planned_minutes": 120, "elapsed_seconds": 8320
        }));
        assert_eq!(state_word(&over), "over");
        assert_eq!(exit_code(&over), 0, "over is still counting");
        assert!(plain_status(&over).contains("planned_s=7200"));

        // A break isn't counting — it wins over the amber alarm.
        let on_break = timer(serde_json::json!({
            "running": true, "mode": "focus", "phase": "break", "over": true
        }));
        assert_eq!(state_word(&on_break), "break");
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

        let outcome = run_dispatch(&client(&server), None, true, false)
            .await
            .unwrap();
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

        let outcome = run_dispatch(
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

        let outcome = run_dispatch(
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

        let outcome = run_dispatch(&client(&server), Some(TimerCmd::Toggle), false, false)
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

        let outcome = run_dispatch(
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

        let outcome = run_dispatch(
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
    async fn settings_json_mirrors_the_server_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "timer_mode": "stopwatch",
                "focus_work_minutes": 50,
                "focus_short_break_minutes": 10,
                "focus_long_break_minutes": 20,
                "focus_long_break_every": 4,
                "idle_guard_enabled": true,
                "idle_threshold_minutes": 15,
                "idle_default_reclaim": "trim",
                "audit_long_hours": 6,
                "audit_short_seconds": 60,
                "audit_badge_enabled": true,
                "overrun_ping_enabled": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = run_dispatch(&client(&server), Some(TimerCmd::Settings), true, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["focus_work_minutes"], 50);
        assert_eq!(v["idle_default_reclaim"], "trim");
        assert_eq!(v["overrun_ping_enabled"], true);
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

        let outcome = run_dispatch(
            &client(&server),
            Some(TimerCmd::Stop { reclaim: None }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcome.code, 1);
        assert!(outcome.err[0].contains("bind or discard first"));
    }

    // ------------------------------------------------- offline writes (#100)

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    #[tokio::test]
    async fn offline_pause_queues_and_says_so() {
        let api = dead_api();
        let dir = scratch();
        crate::timer_cache::store_at(
            &dir.join("timer-cache.json"),
            &timer(serde_json::json!({
                "running": true, "bound": true, "elapsed_seconds": 2832,
                "label": "systems"
            })),
        );
        let queued = queued_at(&api, &dir);

        let outcome = dispatch(&api, &queued, Some(TimerCmd::Pause), false, false)
            .await
            .unwrap();
        assert_eq!(outcome.code, 0, "the keystroke is never refused offline");
        assert!(outcome.out[0].contains("paused"));
        assert!(outcome.out[0].contains("queued (offline)"));
        assert_eq!(queued.queue_summary().depth, 1);
    }

    #[tokio::test]
    async fn json_read_carries_the_queue_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "elapsed_seconds": 3134
            })))
            .mount(&server)
            .await;

        let api = client(&server);
        let dir = scratch();
        let queued = queued_at(&api, &dir);
        queued_seed(&dir, 2);

        let outcome = dispatch(&api, &queued, None, true, false).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["queued"], true);
        assert_eq!(v["queue_depth"], 2);
        assert_eq!(
            v["stale"],
            serde_json::Value::Null,
            "live read is not stale"
        );
    }

    #[tokio::test]
    async fn short_status_wears_the_queued_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "elapsed_seconds": 3134
            })))
            .mount(&server)
            .await;

        let api = client(&server);
        let dir = scratch();
        let queued = queued_at(&api, &dir);
        queued_seed(&dir, 3);

        let short = dispatch(
            &api,
            &queued,
            Some(TimerCmd::Status { short: true }),
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(short.out[0], "● 52:14 ↑3");

        let plain = dispatch(
            &api,
            &queued,
            Some(TimerCmd::Status { short: false }),
            false,
            false,
        )
        .await
        .unwrap();
        assert!(plain.out[0].ends_with(" queued=3"), "{}", plain.out[0]);
    }

    fn queued_seed(dir: &std::path::Path, n: usize) {
        let store = crate::queue::QueueStore::at(dir.join("queue.json"));
        for _ in 0..n {
            store
                .enqueue(crate::queue::IntentKind::TimerPause {
                    at: jiff::Timestamp::now(),
                })
                .unwrap();
        }
    }
}
