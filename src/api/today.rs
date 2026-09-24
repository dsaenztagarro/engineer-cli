//! `GET /api/v1/today` — Home's composed daily-loop aggregate, additive-only (engineer ADR 0027).
#![allow(dead_code)]

use serde::Deserialize;

use super::{ApiClient, ApiError, Timer};

#[derive(Debug, Clone, Deserialize)]
pub struct Today {
    pub date: DateBlock,
    /// Byte-identical to `GET /api/v1/timer`, so the shared [`Timer`] decodes it —
    /// never a second timer shape.
    pub timer: Timer,
    #[serde(default)]
    pub pace: Option<Pace>,
    #[serde(default)]
    pub plan: Plan,
    #[serde(default)]
    pub totals: Totals,
    #[serde(default)]
    pub review: Review,
    /// Most-recently-touched first, as served.
    #[serde(default)]
    pub reading: Vec<ReadingItem>,
}

/// Computed server-side (the 4 AM study-day boundary, Monday-first weeks), so
/// Home agrees with the header cell and Progress — never derive them locally.
#[derive(Debug, Clone, Deserialize)]
pub struct DateBlock {
    /// Not the deprecated `study_day` alias (engineer ADR 0032).
    pub day: jiff::civil::Date,
    pub weekday: String,
    pub week: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pace {
    pub behind_count: u32,
    pub worst: Worst,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Worst {
    pub target_id: i64,
    pub axis: String,
    /// A domain id or an enum string, by `axis`.
    #[serde(default)]
    pub scope_value: serde_json::Value,
    pub scope_name: String,
    /// `i64` for parity with the pace derivation, though always positive here.
    pub delta_minutes: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub items: Vec<PlanItem>,
    #[serde(default)]
    pub left_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanItem {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub size_minutes: u32,
    #[serde(default)]
    pub logged_minutes: u32,
    #[serde(default)]
    pub moved_from: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Totals {
    #[serde(default)]
    pub logged_minutes: u32,
}

/// `due_count` includes the stale ones.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub due_count: u32,
    #[serde(default)]
    pub stale_count: u32,
    #[serde(default)]
    pub est_minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadingItem {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub progress_percent: Option<f32>,
    #[serde(default)]
    pub chapters_total: Option<u32>,
    #[serde(default)]
    pub next_chapter: Option<NextChapter>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NextChapter {
    pub number: u32,
    pub title: String,
}

impl ApiClient {
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

    /// The `timer` block uses the real `GET /api/v1/timer` field names.
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
                        "status": "in_progress", "state": "live", "kind": "build",
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
                    "author": "Kleppmann",
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
        assert!(today.timer.running);
        assert_eq!(today.timer.elapsed_seconds, Some(1453));

        let pace = today.pace.as_ref().expect("pace present when behind");
        assert_eq!(pace.behind_count, 2);
        assert_eq!(pace.worst.scope_name, "systems");
        assert_eq!(pace.worst.delta_minutes, 108);

        assert_eq!(today.plan.items.len(), 2);
        assert_eq!(today.plan.items[0].state, "live");
        assert_eq!(today.plan.items[0].kind.as_deref(), Some("build"));
        assert_eq!(today.plan.items[0].logged_minutes, 34);
        assert_eq!(today.plan.items[1].moved_from.as_deref(), Some("Sun"));
        assert_eq!(today.plan.left_count, 2);

        assert_eq!(today.totals.logged_minutes, 95);
        assert_eq!(today.review.due_count, 4);
        assert_eq!(today.review.stale_count, 1);

        let book = &today.reading[0];
        assert_eq!(book.author.as_deref(), Some("Kleppmann"));
        assert_eq!(book.progress_percent, Some(42.0));
        assert_eq!(book.next_chapter.as_ref().unwrap().number, 7);
    }

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

    #[tokio::test]
    async fn the_day_is_date_day_not_the_study_day_alias_and_unknown_keys_are_ignored() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/today"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "date": { "day": "2026-07-06", "study_day": "2026-07-05",
                          "weekday": "mon", "week": "2026-W28" },
                "timer": { "running": false },
                "a_block_added_later": { "anything": [1, 2, 3] }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let today = client(&server).today().await.unwrap();
        assert_eq!(today.date.day, jiff::civil::date(2026, 7, 6));
    }
}
