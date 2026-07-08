//! Headless `engineer today` — the one-shot twin of the Home screen
//! (home.dc.html §ONE READ). One `GET /api/v1/today` read, emitted as the raw
//! payload (`--json`) or a compact, stable human summary: the date, the timer,
//! the pace line, today's plan counts, review triage, and the books mid-chapter.
//!
//! Home owns no write, so this verb is read-only: it exits `0` on success, and a
//! `401`/transport error surfaces through the shared `ApiError` path with a
//! non-zero exit, matching the `engineer timer`/`target` contract.

use clap::Args;
use color_eyre::eyre::Result;

use crate::api::{ApiClient, ApiError, Today};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::ui::widgets::fmt_elapsed;

#[derive(Args)]
pub struct TodayArgs {
    /// Emit the raw `/today` JSON payload instead of the human summary.
    #[arg(long)]
    json: bool,
}

pub async fn run(cfg: &Config, args: TodayArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);

    let outcome = dispatch(&api, args.json).await?;
    for line in &outcome.out {
        println!("{line}");
    }
    Ok(outcome.code)
}

#[derive(Debug)]
struct Outcome {
    out: Vec<String>,
    code: i32,
}

async fn dispatch(api: &ApiClient, json: bool) -> Result<Outcome, ApiError> {
    let today = api.today().await?;
    let out = if json {
        vec![json_today(&today).to_string()]
    } else {
        human_summary(&today)
    };
    Ok(Outcome { out, code: 0 })
}

/// A faithful projection of the decoded `/today` payload. Built by hand (rather
/// than `#[derive(Serialize)]`) because the embedded `api::Timer` is
/// deserialize-only; this mirrors the `engineer timer --json` precedent.
fn json_today(t: &Today) -> serde_json::Value {
    serde_json::json!({
        "date": {
            "day": t.date.day.to_string(),
            "weekday": t.date.weekday,
            "week": t.date.week,
        },
        "timer": {
            "running": t.timer.running,
            "mode": t.timer.mode,
            "phase": t.timer.phase,
            "elapsed_seconds": t.timer.elapsed_seconds,
            "label": t.timer.label,
            "bound": t.timer.bound,
            "idle": t.timer.idle,
            "over": t.timer.over,
        },
        "pace": t.pace.as_ref().map(|p| serde_json::json!({
            "behind_count": p.behind_count,
            "worst": {
                "target_id": p.worst.target_id,
                "axis": p.worst.axis,
                "scope_name": p.worst.scope_name,
                "delta_minutes": p.worst.delta_minutes,
            },
        })),
        "plan": {
            "items": t.plan.items.iter().map(|i| serde_json::json!({
                "id": i.id,
                "title": i.title,
                "status": i.status,
                "state": i.state,
                "kind": i.kind,
                "size_minutes": i.size_minutes,
                "logged_minutes": i.logged_minutes,
                "moved_from": i.moved_from,
            })).collect::<Vec<_>>(),
            "left_count": t.plan.left_count,
        },
        "totals": { "logged_minutes": t.totals.logged_minutes },
        "review": {
            "due_count": t.review.due_count,
            "stale_count": t.review.stale_count,
            "est_minutes": t.review.est_minutes,
        },
        "reading": t.reading.iter().map(|b| serde_json::json!({
            "id": b.id,
            "title": b.title,
            "author": b.author,
            "progress_percent": b.progress_percent,
            "chapters_total": b.chapters_total,
            "next_chapter": b.next_chapter.as_ref().map(|c| serde_json::json!({
                "number": c.number,
                "title": c.title,
            })),
        })).collect::<Vec<_>>(),
    })
}

/// A compact, greppable summary — one aligned line per block, mirroring the Home
/// screen's reading order (date → timer → pace → plan → review → reading).
fn human_summary(t: &Today) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} ({}) · {}",
        t.date.day, t.date.weekday, t.date.week
    ));

    lines.push(if t.timer.running {
        let elapsed = fmt_elapsed(t.timer.elapsed_seconds.unwrap_or(0));
        let label = t.timer.label.as_deref().unwrap_or("untitled");
        format!("timer    {elapsed} · {label}")
    } else {
        "timer    no timer".to_string()
    });

    lines.push(match t.pace.as_ref() {
        Some(p) => format!(
            "pace     behind {:.1}h — {} ({} trailing)",
            p.worst.delta_minutes as f64 / 60.0,
            p.worst.scope_name,
            p.behind_count
        ),
        None => "pace     on pace".to_string(),
    });

    let done = t.plan.items.iter().filter(|i| i.state == "done").count();
    let live = t.plan.items.iter().filter(|i| i.state == "live").count();
    let logged: u32 = t.plan.items.iter().map(|i| i.logged_minutes).sum();
    let planned: u32 = t.plan.items.iter().map(|i| i.size_minutes).sum();
    lines.push(format!(
        "plan     {done} done · {live} live · {} left · {logged}m/{planned}m",
        t.plan.left_count
    ));

    lines.push(format!(
        "review   {} due · {} stale · ~{}m",
        t.review.due_count, t.review.stale_count, t.review.est_minutes
    ));

    for b in &t.reading {
        let next = b
            .next_chapter
            .as_ref()
            .map(|c| format!(" · next ch.{} {}", c.number, c.title))
            .unwrap_or_default();
        lines.push(format!(
            "reading  {} {:.0}%{}",
            b.title,
            b.progress_percent.unwrap_or(0.0),
            next
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
            "timer": { "running": true, "bound": true,
                       "label": "Raft leader election", "elapsed_seconds": 1453 },
            "pace": { "behind_count": 2, "worst": {
                "target_id": 42, "axis": "domain", "scope_value": 7,
                "scope_name": "systems", "delta_minutes": 108
            } },
            "plan": { "items": [
                { "id": 1, "title": "Raft leader election", "status": "in_progress",
                  "state": "live", "kind": "build", "size_minutes": 120, "logged_minutes": 34 }
            ], "left_count": 2 },
            "totals": { "logged_minutes": 95 },
            "review": { "due_count": 4, "stale_count": 1, "est_minutes": 25 },
            "reading": [
                { "id": 10, "title": "Designing Data-Intensive Applications", "author": "Kleppmann",
                  "progress_percent": 42, "chapters_total": 12,
                  "next_chapter": { "number": 7, "title": "Transactions" } }
            ]
        })
    }

    #[tokio::test]
    async fn json_emits_the_today_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), true).await.unwrap();
        assert_eq!(outcome.code, 0);
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert_eq!(v["date"]["week"], "2026-W28");
        assert_eq!(v["timer"]["running"], true);
        assert_eq!(v["pace"]["worst"]["scope_name"], "systems");
        assert_eq!(v["plan"]["items"][0]["state"], "live");
        assert_eq!(v["review"]["due_count"], 4);
        assert_eq!(v["reading"][0]["next_chapter"]["number"], 7);
    }

    #[tokio::test]
    async fn json_pace_is_null_when_nothing_trails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
                "timer": { "running": false },
                "pace": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), true).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&outcome.out[0]).unwrap();
        assert!(v["pace"].is_null());
        assert_eq!(v["timer"]["running"], false);
    }

    #[tokio::test]
    async fn human_summary_reads_the_day_top_to_bottom() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = dispatch(&client(&server), false).await.unwrap();
        assert_eq!(outcome.code, 0);
        let text = outcome.out.join("\n");
        assert!(text.contains("2026-W28"), "date: {text}");
        assert!(text.contains("Raft leader election"), "timer: {text}");
        assert!(text.contains("behind 1.8h — systems"), "pace: {text}");
        assert!(text.contains("4 due"), "review: {text}");
        assert!(text.contains("next ch.7 Transactions"), "reading: {text}");
    }

    #[tokio::test]
    async fn unauthorized_propagates_as_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/unauthorized",
                "title": "Unauthorized",
                "status": 401
            })))
            .mount(&server)
            .await;

        let err = dispatch(&client(&server), true).await.unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }
}
