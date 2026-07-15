//! Headless `engineer week` (the planned-vs-done readout) and `engineer plan`
//! (declare a plan item) — week-planning.brief.md's unblocked core: a readout
//! and a one-liner, not a planning canvas. The retro reflection is read-only
//! until the server exposes a v1 week-note write.

use std::io::IsTerminal;

use clap::Args;
use color_eyre::eyre::Result;
use jiff::{civil::Date, Zoned};

use crate::api::{ActivityCreate, ApiClient, ApiError, Week};
use crate::auth::TokenProvider;
use crate::config::Config;

#[derive(Args)]
pub struct WeekArgs {
    /// ISO week id (`YYYY-Www`); defaults to the current study week.
    #[arg(long)]
    week: Option<String>,
    /// Emit the aggregate as JSON.
    #[arg(long)]
    json: bool,
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
    let api = client(cfg).await?;
    let colored = colored();
    let iso = args.week.unwrap_or_else(current_iso_week);
    let week = match api.get_week(&iso).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{}", problem_text(e));
            return Ok(1);
        }
    };
    if args.json {
        println!("{}", json_week(&week));
    } else {
        for line in human_week(&week, colored) {
            println!("{line}");
        }
    }
    Ok(0)
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
    let create = ActivityCreate {
        title: args.title.clone(),
        planned_on: Some(on),
        target_duration_minutes: args.size,
        kind: args.kind.clone(),
        domain_id: args.domain,
        ..Default::default()
    };
    match api.create_activity(&create).await {
        Ok(a) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({ "id": a.id, "title": a.title, "planned_on": on.to_string() })
                );
            } else {
                println!(
                    "{} planned \"{}\" on {on}",
                    paint("●", COLOR_OK, colored),
                    a.title
                );
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("{}", problem_text(e));
            Ok(1)
        }
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
        ApiError::Unauthorized => "not authenticated — run `engineer login`".into(),
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
}
