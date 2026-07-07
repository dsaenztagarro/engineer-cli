//! `GET /api/v1/today` — the composed daily-loop aggregate (home.dc.html §ONE READ).
//!
//! One read-only, unpaginated payload the TUI Home renders in a single pass
//! instead of N per-resource calls. Every block *composes* the derivation that
//! already owns it (the week story, the timer serializer, the review dashboard
//! counts, the next-unread chapter, the pace fold, today's segment sum), so it
//! can never disagree with the per-resource endpoints. Nothing is stored — a
//! single object (not a paginated `List`) comes back.
//!
//! The contract is additive-only (engineer ADR 0027): unknown keys are ignored
//! and absent keys `serde`-default, so the client survives the payload growing
//! under it. The `timer` block is byte-identical to `GET /api/v1/timer`, so the
//! shared [`Timer`] struct decodes it verbatim. Read `date.day`, not the
//! deprecated `study_day` alias (ADR 0032).
#![allow(dead_code)]

use serde::Deserialize;

use super::{ApiClient, ApiError, Timer};

/// Top-level payload of `GET /api/v1/today`.
///
/// `date` and `timer` are core and always present; the remaining blocks
/// `serde`-default so a minimal payload (idle timer, nothing planned, on pace)
/// still decodes. `pace` is `None` when nothing trails — silence is the on-pace
/// state, baked into the API rather than computed here.
#[derive(Debug, Clone, Deserialize)]
pub struct Today {
    pub date: DateBlock,
    /// The live timer, decoded by the shared [`Timer`] struct — `running: false`
    /// when idle. Never a second timer shape.
    pub timer: Timer,
    /// The worst-behind target, pre-folded; `None` when nothing trails.
    #[serde(default)]
    pub pace: Option<Pace>,
    #[serde(default)]
    pub plan: Plan,
    #[serde(default)]
    pub totals: Totals,
    #[serde(default)]
    pub review: Review,
    /// Mid-chapter books, most-recently-touched first.
    #[serde(default)]
    pub reading: Vec<ReadingItem>,
}

/// The one clock: `day` is the ISO date under engineer's 4 AM study-day
/// boundary, `week` a Monday-first `YYYY-Www` id — both computed server-side, so
/// Home agrees with the header cell and Progress to the minute.
#[derive(Debug, Clone, Deserialize)]
pub struct DateBlock {
    pub day: jiff::civil::Date,
    pub weekday: String,
    pub week: String,
}

/// The pace fold: the single most-behind target named by scope, plus how many
/// trail. Only ever present when the week is behind — `met`/`on_pace` collapse
/// to a `null` `pace` block. There is no red pace state.
#[derive(Debug, Clone, Deserialize)]
pub struct Pace {
    /// How many targets trail — the "N targets trailing" tail.
    pub behind_count: u32,
    pub worst: Worst,
}

/// The single worst-behind target. `delta_minutes` is how far behind (positive
/// minutes short); `scope_name` is the human label (a domain, path, or bloom
/// level) to render.
#[derive(Debug, Clone, Deserialize)]
pub struct Worst {
    pub target_id: i64,
    pub axis: String,
    /// The raw scope value (domain id or enum string); `scope_name` is what the
    /// meter shows.
    #[serde(default)]
    pub scope_value: serde_json::Value,
    pub scope_name: String,
    /// Minutes behind. Modeled `i64` for parity with the pace derivation, though
    /// a behind target is always positive here.
    pub delta_minutes: i64,
}

/// Today's plan slice — the same `WeekStory` items the week canvas reads.
/// Empty `items` means nothing is planned for today (a calm invitation, not a
/// blank panel).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub items: Vec<PlanItem>,
    /// How many items are still outstanding — the "N left" / "left to plan" tail.
    #[serde(default)]
    pub left_count: u32,
}

/// One planned item. `state` is the lifecycle word the row glyph keys off
/// (`planned` `○` / `live` `●` / `done` `✓` / `left` `·`); `moved_from` is set
/// when the item was carried over from an earlier day.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanItem {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub size_minutes: u32,
    #[serde(default)]
    pub logged_minutes: u32,
    /// A prior day this item was carried over from (`"Sun"`), else `None`.
    #[serde(default)]
    pub moved_from: Option<String>,
}

/// Today's completed-segment minutes, plan-agnostic (unplanned work counts).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Totals {
    #[serde(default)]
    pub logged_minutes: u32,
}

/// Review triage counts only — the queue itself stays on
/// `GET /api/v1/review/dashboard`. `due_count` includes the stale ones.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub due_count: u32,
    #[serde(default)]
    pub stale_count: u32,
    #[serde(default)]
    pub est_minutes: i64,
}

/// A mid-chapter book with where-you-are: a superset of the reading list, adding
/// the next unread chapter. `progress_percent`/`chapters_total` default when the
/// server can't derive them; `next_chapter` is `None` once the book is finished.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadingItem {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub progress_percent: Option<f32>,
    #[serde(default)]
    pub chapters_total: Option<u32>,
    #[serde(default)]
    pub next_chapter: Option<NextChapter>,
}

/// The next unread chapter — where the reader left off.
#[derive(Debug, Clone, Deserialize)]
pub struct NextChapter {
    pub number: u32,
    pub title: String,
}

impl ApiClient {
    /// Fetch the composed daily-loop aggregate that powers Home in one pass.
    pub async fn today(&self) -> Result<Today, ApiError> {
        self.get("/api/v1/today", &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    /// A full payload. The `timer` block uses the real `GET /api/v1/timer` field
    /// names (`elapsed_seconds`/`label`) it is byte-identical to — not the
    /// design mock's `elapsed_s`/`kind` shorthand.
    fn sample_body() -> serde_json::Value {
        serde_json::json!({
            "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
            "timer": {
                "running": true,
                "bound": true,
                "elapsed_seconds": 1453,
                "label": "Implement Raft leader election",
                "mode": "stopwatch"
            },
            "pace": {
                "behind_count": 2,
                "worst": {
                    "target_id": 42,
                    "axis": "domain",
                    "scope_value": 7,
                    "scope_name": "systems",
                    "delta_minutes": 108
                }
            },
            "plan": {
                "items": [
                    {
                        "id": 1, "title": "Implement Raft leader election",
                        "status": "in_progress", "state": "live",
                        "size_minutes": 120, "logged_minutes": 34
                    },
                    {
                        "id": 2, "title": "Spaced-rep drills",
                        "status": "pending", "state": "left",
                        "size_minutes": 0, "logged_minutes": 0, "moved_from": "Sun"
                    }
                ],
                "left_count": 2
            },
            "totals": { "logged_minutes": 95 },
            "review": { "due_count": 4, "stale_count": 1, "est_minutes": 25 },
            "reading": [
                {
                    "id": 10, "title": "Designing Data-Intensive Applications",
                    "progress_percent": 42, "chapters_total": 12,
                    "next_chapter": { "number": 7, "title": "Transactions" }
                }
            ]
        })
    }

    #[tokio::test]
    async fn today_requests_api_v1_today_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .and(query_param_is_missing("week"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_body()))
            .expect(1) // verified on drop: exactly one hit, no query params
            .mount(&server)
            .await;

        let today = client(&server).today().await.unwrap();

        assert_eq!(today.date.day, jiff::civil::date(2026, 7, 6));
        assert_eq!(today.date.week, "2026-W28");
        // The timer block is the shared `Timer` struct, decoded verbatim.
        assert!(today.timer.running);
        assert_eq!(today.timer.elapsed_seconds, Some(1453));

        let pace = today.pace.as_ref().expect("pace present when behind");
        assert_eq!(pace.behind_count, 2);
        assert_eq!(pace.worst.scope_name, "systems");
        assert_eq!(pace.worst.delta_minutes, 108);

        assert_eq!(today.plan.items.len(), 2);
        assert_eq!(today.plan.items[0].state, "live");
        assert_eq!(today.plan.items[0].logged_minutes, 34);
        assert_eq!(today.plan.items[1].moved_from.as_deref(), Some("Sun"));
        assert_eq!(today.plan.left_count, 2);

        assert_eq!(today.totals.logged_minutes, 95);
        assert_eq!(today.review.due_count, 4);
        assert_eq!(today.review.stale_count, 1);

        let book = &today.reading[0];
        assert_eq!(book.progress_percent, Some(42.0));
        assert_eq!(book.next_chapter.as_ref().unwrap().number, 7);
    }

    /// Additive-only contract (ADR 0027): a minimal payload — idle timer, `pace`
    /// null, every optional block absent — must still decode via serde-defaults.
    #[tokio::test]
    async fn minimal_payload_decodes_via_defaults() {
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

        let today = client(&server).today().await.unwrap();

        assert!(!today.timer.running);
        assert!(today.pace.is_none()); // silence = on pace
        assert!(today.plan.items.is_empty());
        assert_eq!(today.plan.left_count, 0);
        assert_eq!(today.totals.logged_minutes, 0);
        assert_eq!(today.review.due_count, 0);
        assert!(today.reading.is_empty());
    }

    #[tokio::test]
    async fn unauthorized_maps_to_unauthorized_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/unauthorized",
                "title": "Unauthorized",
                "status": 401
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = client(&server).today().await.unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }
}
