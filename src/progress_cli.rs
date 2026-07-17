//! Headless `engineer progress` (alias `pace`) — the one-shot twin of the pace
//! meters (progress.brief.md §8.2, cross-cutting.brief.md §C).
//!
//! Reuses the shipped `GET /api/v1/progress` read. Three shapes, like the timer:
//! the bare form prints one greppable line per target plus a summary; `--json`
//! emits the structured payload; `--short` is the single status-bar reduction.
//! Output is plain when piped (ANSI only on a TTY, `NO_COLOR` honoured). Quiet by
//! default — on-pace is calm, `behind` is as loud as it gets — mirrored in the
//! exit code: `0` on pace (or nothing declared) · `2` at least one target behind.

use std::io::IsTerminal;

use clap::Args;
use color_eyre::eyre::Result;

use crate::api::{ApiClient, PaceState, Progress, ProgressReading};
use crate::auth::TokenProvider;
use crate::config::Config;

#[derive(Args)]
pub struct ProgressArgs {
    /// Emit the structured payload as JSON.
    #[arg(long)]
    json: bool,
    /// Status-bar form: a single line — `✓ pace` on track, `⚠ pace behind Nh`
    /// when trailing, empty when no targets are declared.
    #[arg(long)]
    short: bool,
    /// An ISO week id (`YYYY-Www`); defaults to the current study week.
    #[arg(long)]
    week: Option<String>,
}

pub async fn run(cfg: &Config, args: ProgressArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    // A read error (network / auth) propagates as an eyre error → exit 1.
    let progress = api.get_progress(args.week.as_deref()).await?;

    if args.json {
        println!("{}", json_progress(&progress));
    } else if args.short {
        let line = short_line(&progress, colored);
        if !line.is_empty() {
            println!("{line}");
        }
    } else {
        for line in human_lines(&progress, colored) {
            println!("{line}");
        }
    }
    Ok(exit_code(&progress))
}

fn behind(progress: &Progress) -> Vec<&ProgressReading> {
    progress
        .targets
        .iter()
        .filter(|r| r.state == PaceState::Behind)
        .collect()
}

/// `0` on pace (or no targets) · `2` at least one target behind. (`1` is left to
/// the eyre error path — a failed read.)
fn exit_code(progress: &Progress) -> i32 {
    if behind(progress).is_empty() {
        0
    } else {
        2
    }
}

/// The bare form: a week header, one line per target, and a summary footer.
fn human_lines(progress: &Progress, colored: bool) -> Vec<String> {
    let mut out = Vec::new();
    let pct = (progress.week.now_fraction * 100.0).round() as i64;
    out.push(paint(
        &format!("{}  ·  now {pct}%", progress.week.id),
        COLOR_MUTED,
        colored,
    ));

    if progress.targets.is_empty() {
        out.push(paint(
            "no targets — declare one: engineer target declare --domain <id> --hours <n>",
            COLOR_MUTED,
            colored,
        ));
        return out;
    }

    for r in &progress.targets {
        out.push(target_line(r, colored));
    }
    out.push(summary_line(progress, colored));
    out
}

/// `distributed systems  2.2/6h  -2.1h behind` — the greppable per-target line.
fn target_line(r: &ProgressReading, colored: bool) -> String {
    let name = r.target.scope.name().to_lowercase();
    let nums = format!("{:.1}/{}h", r.actual_hours(), fmt_hours(r.hours_per_week));
    let state = match r.state {
        PaceState::Met => paint("met", COLOR_MUTED, colored),
        PaceState::OnPace => paint(
            &format!("{:+.1}h {}", r.delta_hours(), r.state.word()),
            COLOR_ON_PACE,
            colored,
        ),
        PaceState::Behind => paint(
            &format!("{:+.1}h {}", r.delta_hours(), r.state.word()),
            COLOR_BEHIND,
            colored,
        ),
    };
    format!("{name}  {nums}  {state}")
}

/// `behind 3.3h total · largest gap "systems"`, or a quiet on-pace confirmation.
fn summary_line(progress: &Progress, colored: bool) -> String {
    let behind = behind(progress);
    if behind.is_empty() {
        return paint("all targets on pace ✓", COLOR_ON_PACE, colored);
    }
    let total: f64 = behind.iter().map(|r| r.delta_hours().abs()).sum();
    // Readings arrive largest-gap-first, so the first behind row is the worst.
    let worst = behind[0].target.scope.name().to_lowercase();
    paint(
        &format!("behind {total:.1}h total · largest gap \"{worst}\""),
        COLOR_BEHIND,
        colored,
    )
}

/// The single status-bar line. Empty when nothing is declared (like the timer's
/// `--short` when nothing runs).
fn short_line(progress: &Progress, colored: bool) -> String {
    if progress.targets.is_empty() {
        return String::new();
    }
    let behind = behind(progress);
    if behind.is_empty() {
        return format!("{} pace", paint("✓", COLOR_ON_PACE, colored));
    }
    let total: f64 = behind.iter().map(|r| r.delta_hours().abs()).sum();
    format!(
        "{} pace behind {total:.1}h",
        paint("⚠", COLOR_BEHIND, colored)
    )
}

fn json_progress(progress: &Progress) -> serde_json::Value {
    let targets: Vec<serde_json::Value> = progress
        .targets
        .iter()
        .map(|r| {
            serde_json::json!({
                "scope": r.target.scope.name(),
                "axis": r.target.axis,
                "hours_per_week": r.hours_per_week,
                "actual_minutes": r.actual_minutes,
                "expected_minutes": r.expected_minutes,
                "delta_minutes": r.delta_minutes,
                "state": state_machine(r.state),
            })
        })
        .collect();

    let behind = behind(progress);
    let behind_json = serde_json::json!({
        "count": behind.len(),
        "total_hours": behind.iter().map(|r| r.delta_hours().abs()).sum::<f64>(),
        "worst": behind.first().map(|r| r.target.scope.name()),
    });

    // The "where did the time go" rollup, for scripted slicing — the machine
    // twin of the on-screen fold (§Where it went; the time-went glance stays a
    // glance). Only `by_kind` is in the pace read today: the payload carries a
    // kind time-mix but no by-domain / by-intent split, so those two surface as
    // null rather than a client-derived second ledger (the backend-gap rule;
    // #122). The top-level `kind_mix` stays put — the `rollup` object is purely
    // additive, so existing scripted consumers are byte-stable.
    let kind_mix: Vec<serde_json::Value> = progress
        .kind_mix
        .iter()
        .map(|k| serde_json::json!({ "kind": k.kind, "minutes": k.minutes }))
        .collect();
    let rollup = serde_json::json!({
        "by_kind": kind_mix.clone(),
        "by_domain": serde_json::Value::Null,
        "by_intent": serde_json::Value::Null,
    });

    serde_json::json!({
        "week": {
            "id": progress.week.id,
            "monday": progress.week.monday.to_string(),
            "elapsed_days": progress.week.elapsed_days,
            "now_fraction": progress.week.now_fraction,
        },
        "targets": targets,
        "behind": behind_json,
        "kind_mix": kind_mix,
        "rollup": rollup,
        "totals": {
            "actual_minutes": progress.totals.actual_minutes,
            "activity_count": progress.totals.activity_count,
            "thin": progress.totals.thin,
        },
    })
}

/// Machine value for the pace state (`on pace` → `on_pace`).
fn state_machine(state: PaceState) -> &'static str {
    match state {
        PaceState::Met => "met",
        PaceState::Behind => "behind",
        PaceState::OnPace => "on_pace",
    }
}

/// Format target hours without a trailing `.0`: `6h`, but `2.5h` when fractional.
fn fmt_hours(hours: f64) -> String {
    if hours.fract().abs() < 1e-9 {
        format!("{hours:.0}")
    } else {
        format!("{hours:.1}")
    }
}

// Terminal-palette 256 colours (docs/designs/README.md palette mapping).
const COLOR_ON_PACE: u8 = 108; // success green
const COLOR_BEHIND: u8 = 179; // warn amber
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

    fn body(states: &[(&str, &str, f64, i64, i64)]) -> serde_json::Value {
        // (scope_name, state, hours_per_week, actual_minutes, delta_minutes)
        let targets: Vec<serde_json::Value> = states
            .iter()
            .enumerate()
            .map(|(i, (name, state, hpw, actual, delta))| {
                serde_json::json!({
                    "target": {
                        "id": i as i64 + 1, "axis": "domain",
                        "scope": { "axis": "domain", "value": i as i64 + 1,
                                   "domain": { "id": i as i64 + 1, "name": name } },
                        "hours_per_week": hpw, "active": true, "retired": false
                    },
                    "hours_per_week": hpw, "actual_minutes": actual,
                    "expected_minutes": actual - delta, "delta_minutes": delta, "state": state
                })
            })
            .collect();
        serde_json::json!({
            "week": { "id": "2026-W29", "monday": "2026-07-13", "sunday": "2026-07-19",
                      "elapsed_days": 2, "now_fraction": 0.28 },
            "targets": targets,
            "kind_mix": [ { "kind": "coding", "minutes": 180 } ], "bloom": [],
            "totals": { "actual_minutes": 132, "activity_count": 3, "thin": false }
        })
    }

    async fn fetch(server: &MockServer) -> Progress {
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
        api.get_progress(None).await.unwrap()
    }

    async fn serve(body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn behind_target_sets_exit_2_and_short_names_the_gap() {
        // 132 actual min = 2.2h against 6h, 108 min behind.
        let server = serve(body(&[("Distributed Systems", "behind", 6.0, 132, -108)])).await;
        let p = fetch(&server).await;
        assert_eq!(exit_code(&p), 2);
        assert!(
            short_line(&p, false).contains("behind 1.8h"),
            "{}",
            short_line(&p, false)
        );
        // Bare form: greppable, no ANSI when not coloured.
        let lines = human_lines(&p, false);
        assert!(lines
            .iter()
            .any(|l| l.contains("distributed systems  2.2/6h  -1.8h behind")));
        assert!(
            lines.iter().all(|l| !l.contains('\x1b')),
            "no ANSI when uncoloured"
        );
    }

    #[tokio::test]
    async fn on_pace_is_calm_and_exits_0() {
        let server = serve(body(&[("Coding", "met", 4.0, 240, 60)])).await;
        let p = fetch(&server).await;
        assert_eq!(exit_code(&p), 0);
        assert_eq!(short_line(&p, false), "✓ pace");
        assert!(human_lines(&p, false)
            .iter()
            .any(|l| l.contains("all targets on pace ✓")));
    }

    #[tokio::test]
    async fn no_targets_short_is_empty_and_exits_0() {
        let server = serve(body(&[])).await;
        let p = fetch(&server).await;
        assert_eq!(exit_code(&p), 0);
        assert_eq!(short_line(&p, false), "");
        assert!(human_lines(&p, false)
            .iter()
            .any(|l| l.contains("no targets")));
    }

    #[tokio::test]
    async fn json_carries_targets_behind_fold_and_totals() {
        let server = serve(body(&[
            ("Distributed Systems", "behind", 6.0, 132, -108),
            ("Coding", "met", 4.0, 240, 60),
        ]))
        .await;
        let p = fetch(&server).await;
        let v = json_progress(&p);
        assert_eq!(v["week"]["id"], "2026-W29");
        assert_eq!(v["targets"].as_array().unwrap().len(), 2);
        assert_eq!(v["targets"][0]["state"], "behind");
        assert_eq!(v["behind"]["count"], 1);
        assert_eq!(v["behind"]["worst"], "Distributed Systems");
        assert_eq!(v["kind_mix"][0]["kind"], "coding");
        assert_eq!(v["kind_mix"][0]["minutes"], 180);
        assert_eq!(v["totals"]["activity_count"], 3);
    }

    #[tokio::test]
    async fn json_rollup_carries_by_kind_and_marks_absent_facets() {
        let server = serve(body(&[("Coding", "met", 4.0, 240, 60)])).await;
        let p = fetch(&server).await;
        let v = json_progress(&p);
        // The new rollup object: `by_kind` is what the pace read supports.
        assert_eq!(v["rollup"]["by_kind"][0]["kind"], "coding");
        assert_eq!(v["rollup"]["by_kind"][0]["minutes"], 180);
        // The payload carries no by-domain / by-intent rollup — absent, not
        // derived client-side (the backend-gap rule).
        assert!(v["rollup"]["by_domain"].is_null());
        assert!(v["rollup"]["by_intent"].is_null());
        // Additive: the shipped top-level `kind_mix` stays byte-stable.
        assert_eq!(v["kind_mix"][0]["kind"], "coding");
        assert_eq!(v["kind_mix"][0]["minutes"], 180);
    }

    #[tokio::test]
    async fn piped_plain_form_is_byte_stable() {
        // The fold + rollup are the `--json` / on-screen glance; the bare
        // greppable twin must not gain a line. Pin it exactly.
        let server = serve(body(&[("Coding", "met", 4.0, 240, 60)])).await;
        let p = fetch(&server).await;
        let lines = human_lines(&p, false);
        assert_eq!(
            lines,
            vec![
                "2026-W29  ·  now 28%".to_string(),
                "coding  4.0/4h  met".to_string(),
                "all targets on pace ✓".to_string(),
            ]
        );
        assert!(lines.iter().all(|l| !l.contains("where it went")));
        assert!(lines.iter().all(|l| !l.contains("rollup")));
    }
}
