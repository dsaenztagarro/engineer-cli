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
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let outcome = dispatch(&api, args, colored).await;
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

async fn dispatch(api: &ApiClient, args: LogArgs, colored: bool) -> Outcome {
    if args.minutes == 0 {
        return Outcome::refuse("--minutes must be greater than 0");
    }
    if let Some(title) = args.title {
        log_new(
            api,
            &title,
            args.minutes,
            args.kind,
            args.domain,
            args.json,
            colored,
        )
        .await
    } else if let Some(query) = args.activity {
        log_segment(api, &query, args.minutes, args.json, colored).await
    } else {
        Outcome::refuse(
            "give a title to log a new activity, or --activity <match> to append to one",
        )
    }
}

/// Log a new completed activity — the exact write the `a` new-activity form makes.
async fn log_new(
    api: &ApiClient,
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
    match api.create_activity(&create).await {
        Ok(a) => {
            if json {
                return Outcome::ok(
                    serde_json::json!({
                        "id": a.id,
                        "title": a.title,
                        "duration_minutes": a.duration_minutes,
                        "kind": a.kind,
                    })
                    .to_string(),
                );
            }
            let extra = kind
                .as_deref()
                .map(|k| format!(" · {k}"))
                .unwrap_or_default();
            Outcome::ok(format!(
                "{} logged \"{}\" · {minutes}m{extra}",
                paint("●", COLOR_OK, colored),
                a.title,
            ))
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

/// Append minutes to an existing activity — resolve the query to its best match
/// (the timer bind candidates), then write a manual segment ending now.
async fn log_segment(
    api: &ApiClient,
    query: &str,
    minutes: u32,
    json: bool,
    colored: bool,
) -> Outcome {
    let candidate = match api.timer_candidates(Some(query)).await {
        Ok(list) => list.into_iter().next(),
        Err(e) => return Outcome::refuse(problem_text(e)),
    };
    let Some(candidate) = candidate else {
        return Outcome::refuse(format!("no activity matches \"{query}\""));
    };

    // The segment ends now and started `minutes` ago — the honest after-the-fact shape.
    let now = Timestamp::now();
    let started = Timestamp::from_second(now.as_second() - minutes as i64 * 60).unwrap_or(now);

    match api.create_segment(candidate.id, started, minutes).await {
        Ok(_) => {
            if json {
                return Outcome::ok(
                    serde_json::json!({
                        "activity_id": candidate.id,
                        "title": candidate.title,
                        "minutes": minutes,
                    })
                    .to_string(),
                );
            }
            Outcome::ok(format!(
                "{} logged {minutes}m on \"{}\"",
                paint("●", COLOR_OK, colored),
                candidate.title,
            ))
        }
        Err(e) => Outcome::refuse(problem_text(e)),
    }
}

fn problem_text(e: ApiError) -> String {
    match e {
        ApiError::Unauthorized => "not authenticated — run `engineer login`".into(),
        ApiError::Problem { detail, .. } if !detail.is_empty() => detail,
        ApiError::Problem { title, .. } => title,
        other => other.to_string(),
    }
}

const COLOR_OK: u8 = 108; // success green

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

        let out = dispatch(
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

        let out = dispatch(&client(&server), args(None, Some("sicp"), 30), false).await;
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

        let out = dispatch(&client(&server), args(None, Some("nope"), 30), false).await;
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("no activity matches"));
    }

    #[tokio::test]
    async fn zero_minutes_and_no_target_are_refused() {
        let server = MockServer::start().await;
        let zero = dispatch(&client(&server), args(Some("x"), None, 0), false).await;
        assert_eq!(zero.code, 1);
        let empty = dispatch(&client(&server), args(None, None, 30), false).await;
        assert_eq!(empty.code, 1);
    }
}
