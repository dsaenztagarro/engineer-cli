//! `GET /api/v1/weeks/:iso_week` — the week aggregate (week-planning.brief.md).
//!
//! Plan, actuals, and the planned-vs-done comparison for one ISO week, derived
//! from the same `WeekStory` the web retro band reads. Read-only: planning
//! *writes* go through the activities API (a `planned` activity + `planned_on` =
//! a plan item — see [`super::activities::ActivityCreate`]). The `note` write is
//! not yet a v1 route, so the retro reflection is read-and-display only.

use serde::Deserialize;

use super::{ApiClient, ApiError};

#[derive(Debug, Clone, Deserialize)]
pub struct Week {
    pub week: WeekFrame,
    #[serde(default)]
    pub days: Vec<WeekDay>,
    pub planned_vs_done: PlannedVsDone,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeekFrame {
    pub id: String,
    /// True for any week fully in the past — the retro's render rule.
    #[serde(default)]
    pub closed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeekDay {
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

/// One plan item — a `planned` activity on this week's canvas. `state` is the
/// canvas appearance (`planned` | `live` | `done` | `left`); `done` is the
/// retro's planned→done judgment.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct PlanItem {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub state: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub size_minutes: Option<u32>,
    #[serde(default)]
    pub logged_minutes: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlannedVsDone {
    #[serde(default)]
    pub planned: u32,
    #[serde(default)]
    pub done: u32,
    #[serde(default)]
    pub logged_minutes: u32,
    #[serde(default)]
    pub planned_minutes: u32,
}

impl Week {
    /// The plan items across the week, in day order — the readout's rows.
    pub fn items(&self) -> impl Iterator<Item = &PlanItem> {
        self.days.iter().flat_map(|d| d.items.iter())
    }
}

impl ApiClient {
    /// Fetch one ISO week's aggregate (`iso_week` like `2026-W29`).
    pub async fn get_week(&self, iso_week: &str) -> Result<Week, ApiError> {
        self.get(&format!("/api/v1/weeks/{iso_week}"), &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_week_reads_the_aggregate_and_flattens_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/weeks/2026-W29"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "week": { "id": "2026-W29", "monday": "2026-07-13", "sunday": "2026-07-19", "closed": false },
                "days": [
                    { "date": "2026-07-13", "weekday": "Mon", "items": [
                        { "id": 1, "title": "SICP ch.3", "kind": "reading", "state": "done",
                          "done": true, "size_minutes": 90, "logged_minutes": 95 }
                    ]},
                    { "date": "2026-07-14", "weekday": "Tue", "items": [
                        { "id": 2, "title": "systems paper", "kind": "reading", "state": "left",
                          "done": false, "size_minutes": 60, "logged_minutes": 0 }
                    ]}
                ],
                "planned_vs_done": { "planned": 2, "done": 1, "logged_minutes": 95, "planned_minutes": 150 },
                "pace": [],
                "note": { "body": "" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
        let week = api.get_week("2026-W29").await.unwrap();
        assert_eq!(week.week.id, "2026-W29");
        assert!(!week.week.closed);
        let items: Vec<&PlanItem> = week.items().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "SICP ch.3");
        assert!(items[0].done);
        assert_eq!(items[1].state, "left");
        assert_eq!(week.planned_vs_done.planned, 2);
        assert_eq!(week.planned_vs_done.done, 1);
    }
}
