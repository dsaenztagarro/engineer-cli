//! Headless `engineer log` — record a completed session without the timer
//! (activities.brief.md §8). Two shapes: log a **new** completed activity
//! (`engineer log '<title>' --minutes N --kind K`), or **append** the minutes to
//! an existing activity by fuzzy match (`engineer log --activity '<match>'
//! --minutes N`). `--json` for machines; plain otherwise; exit 0 on success, 1
//! on refusal — the `engineer timer`/`target` contract.

use std::io::IsTerminal;

use clap::Args;
use color_eyre::eyre::Result;
use jiff::Timestamp;

use crate::api::{ActivityCreate, ApiClient, ApiError};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::messages;
use crate::queue::QueuedClient;

#[derive(Args)]
pub struct LogArgs {
    /// Title of a new completed activity to log. Omit when using --activity.
    title: Option<String>,
    /// Instead of a new activity, append the minutes to an existing one
    /// (resolved by fuzzy match, like the timer bind).
    #[arg(long, conflicts_with = "title")]
    activity: Option<String>,
    /// Minutes worked (required).
    #[arg(long)]
    minutes: u32,
    /// Kind for a new log (e.g. reading, coding). Only for the new-activity form.
    #[arg(long)]
    kind: Option<String>,
    /// Domain id for a new log.
    #[arg(long)]
    domain: Option<i64>,
    /// Emit JSON instead of the human line.
    #[arg(long)]
    json: bool,
}

pub async fn run(cfg: &Config, args: LogArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let outcome = dispatch(&api, &queued, args, colored).await;
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

async fn dispatch(api: &ApiClient, queued: &QueuedClient, args: LogArgs, colored: bool) -> Outcome {
    if args.minutes == 0 {
        return Outcome::refuse("--minutes must be greater than 0");
    }
    if let Some(title) = args.title {
        log_new(
            queued,
            &title,
            args.minutes,
            args.kind,
            args.domain,
            args.json,
            colored,
        )
        .await
    } else if let Some(query) = args.activity {
        log_segment(api, queued, &query, args.minutes, args.json, colored).await
    } else {
        Outcome::refuse(
            "give a title to log a new activity, or --activity <match> to append to one",
        )
    }
}

/// Log a new completed activity — the exact write the `a` new-activity form
/// makes. Routes through [`QueuedClient`] like every write: a single create
/// carrying `duration_minutes` (the server mints the activity *and* its opening
/// segment from it), so offline it queues one `ActivityCreate` and prints a
/// provisional line, replaying when the wire returns.
async fn log_new(
    queued: &QueuedClient,
    title: &str,
    minutes: u32,
    kind: Option<String>,
    domain: Option<i64>,
    json: bool,
    colored: bool,
) -> Outcome {
    let create = ActivityCreate {
        title: title.to_string(),
        duration_minutes: Some(minutes),
        kind: kind.clone(),
        domain_id: domain,
        ..Default::default()
    };
    match queued.create_activity(&create).await {
        Ok(out) => {
            let provisional = out.is_provisional();
            let a = out.value();
            if json {
                // A queued create's `id` is the negative provisional stand-in
                // (`-(intent.id)`) — carried honestly so a script reads "not yet
                // server-minted" straight off the sign, with `queued: true` to
                // name it.
                let mut v = serde_json::json!({
                    "id": a.id,
                    "title": a.title,
                    "duration_minutes": a.duration_minutes,
                    "kind": a.kind,
                });
                if provisional {
                    v["queued"] = true.into();
                }
                return Outcome::ok(v.to_string());
            }
            let extra = kind
                .as_deref()
                .map(|k| format!(" · {k}"))
                .unwrap_or_default();
            let mut line = format!(
                "{} logged \"{}\" · {minutes}m{extra}",
                paint("●", COLOR_OK, colored),
                a.title,
            );
            if provisional {
                line.push_str(&paint("  · queued (offline)", COLOR_MUTED, colored));
            }
            Outcome::ok(line)
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

/// Append minutes to an existing activity — resolve the query to its best match
/// (the timer bind candidates, a *live* read), then write a manual segment
/// ending now through [`QueuedClient`]. The resolve is the offline boundary:
/// with no wire it can't fuzzy-match, and guessing would log against the wrong
/// activity, so it refuses with the way forward — the one spelling `timer start`
/// and `note capture --book` already use.
async fn log_segment(
    api: &ApiClient,
    queued: &QueuedClient,
    query: &str,
    minutes: u32,
    json: bool,
    colored: bool,
) -> Outcome {
    let candidate = match api.timer_candidates(Some(query)).await {
        Ok(list) => list.into_iter().next(),
        Err(ApiError::Transport(_)) => {
            return Outcome::refuse(format!(
                "offline — can't resolve activity \"{query}\"; log a new one, or retry online"
            ));
        }
        Err(e) => return Outcome::refuse(problem_text(e)),
    };
    let Some(candidate) = candidate else {
        return Outcome::refuse(format!("no activity matches \"{query}\""));
    };

    // The segment ends now and started `minutes` ago — the honest after-the-fact shape.
    let now = Timestamp::now();
    let started = Timestamp::from_second(now.as_second() - minutes as i64 * 60).unwrap_or(now);

    match queued.create_segment(candidate.id, started, minutes).await {
        Ok(out) => {
            let provisional = out.is_provisional();
            if json {
                let mut v = serde_json::json!({
                    "activity_id": candidate.id,
                    "title": candidate.title,
                    "minutes": minutes,
                });
                if provisional {
                    v["queued"] = true.into();
                }
                return Outcome::ok(v.to_string());
            }
            let mut line = format!(
                "{} logged {minutes}m on \"{}\"",
                paint("●", COLOR_OK, colored),
                candidate.title,
            );
            if provisional {
                line.push_str(&paint("  · queued (offline)", COLOR_MUTED, colored));
            }
            Outcome::ok(line)
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

fn problem_text(e: ApiError) -> String {
    match e {
        ApiError::Unauthorized => messages::not_authenticated().into(),
        ApiError::Problem { detail, .. } if !detail.is_empty() => detail,
        ApiError::Problem { title, .. } => title,
        other => other.to_string(),
    }
}

const COLOR_OK: u8 = 108; // success green
const COLOR_MUTED: u8 = 244; // the queued/offline tail

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
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    /// A per-test scratch dir so the queue never touches the shared XDG state.
    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-log-cli-{}-{}",
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

    fn args(title: Option<&str>, activity: Option<&str>, minutes: u32) -> LogArgs {
        LogArgs {
            title: title.map(str::to_string),
            activity: activity.map(str::to_string),
            minutes,
            kind: None,
            domain: None,
            json: false,
        }
    }

    /// `dispatch` with an isolated, empty queue — what the live tests need.
    async fn run_dispatch(api: &ApiClient, args: LogArgs, json: bool) -> Outcome {
        let dir = scratch();
        let queued = queued_at(api, &dir);
        let mut args = args;
        args.json = json;
        dispatch(api, &queued, args, false).await
    }

    #[tokio::test]
    async fn log_new_posts_a_completed_activity() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .and(body_partial_json(serde_json::json!({
                "activity": { "title": "Crafting Interpreters ch.4", "duration_minutes": 45 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 5, "title": "Crafting Interpreters ch.4", "duration_minutes": 45
            })))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(
            &client(&server),
            args(Some("Crafting Interpreters ch.4"), None, 45),
            false,
        )
        .await;
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("logged \"Crafting Interpreters ch.4\" · 45m"));
    }

    #[tokio::test]
    async fn log_activity_resolves_a_candidate_then_appends_a_segment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/candidates"))
            .and(query_param("q", "sicp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": 9, "title": "SICP ch.3" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(
                serde_json::json!({ "segment": { "duration_minutes": 30 } }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 71, "activity_id": 9, "minutes": 30
            })))
            .expect(1)
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), args(None, Some("sicp"), 30), false).await;
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("logged 30m on \"SICP ch.3\""));
    }

    #[tokio::test]
    async fn log_activity_with_no_match_refuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/candidates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), args(None, Some("nope"), 30), false).await;
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("no activity matches"));
    }

    #[tokio::test]
    async fn zero_minutes_and_no_target_are_refused() {
        let server = MockServer::start().await;
        let zero = run_dispatch(&client(&server), args(Some("x"), None, 0), false).await;
        assert_eq!(zero.code, 1);
        let empty = run_dispatch(&client(&server), args(None, None, 30), false).await;
        assert_eq!(empty.code, 1);
    }

    // --- offline (#108): the create shape queues, the append shape refuses ---

    #[tokio::test]
    async fn offline_log_title_enqueues_a_provisional_activity() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let out = dispatch(
            &api,
            &queued,
            args(Some("Raft leader election"), None, 20),
            false,
        )
        .await;
        assert_eq!(out.code, 0, "a title create is never refused offline");
        assert!(out.out[0].contains("logged \"Raft leader election\" · 20m"));
        assert!(out.out[0].contains("queued (offline)"), "{}", out.out[0]);

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "plan");
        assert_eq!(intents[0].stream, "activity");
        match &intents[0].kind {
            IntentKind::ActivityCreate { body } => {
                assert_eq!(body.title, "Raft leader election");
                assert_eq!(
                    body.duration_minutes,
                    Some(20),
                    "the server mints the segment"
                );
            }
            other => panic!("expected an ActivityCreate intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offline_log_title_json_carries_the_negative_provisional_id() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let mut args = args(Some("Raft"), None, 20);
        args.json = true;
        let out = dispatch(&api, &queued, args, false).await;
        assert_eq!(out.code, 0);
        let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
        assert!(
            v["id"].as_i64().unwrap() < 0,
            "not yet server-minted: {}",
            out.out[0]
        );
        assert_eq!(v["duration_minutes"], 20);
        assert_eq!(v["queued"], true);
    }

    #[tokio::test]
    async fn offline_log_activity_refuses_it_cannot_resolve() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let out = dispatch(&api, &queued, args(None, Some("sicp"), 30), false).await;
        assert_eq!(out.code, 1, "the fuzzy resolve is a live read");
        assert!(out.err[0].contains("offline"), "{}", out.err[0]);
        assert!(out.err[0].contains("can't resolve activity \"sicp\""));
        assert_eq!(
            queued.queue_summary().depth,
            0,
            "an unresolved append queues nothing"
        );
    }
}
