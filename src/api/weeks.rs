//! The week aggregate (`GET /api/v1/weeks/:iso_week`) and its one stored write,
//! the week note.

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError};

#[derive(Debug, Clone, Deserialize)]
pub struct Week {
    pub week: WeekFrame,
    #[serde(default)]
    pub days: Vec<WeekDay>,
    pub planned_vs_done: PlannedVsDone,
    #[serde(default)]
    pub note: WeekNote,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeekFrame {
    pub id: String,
    /// Absent on older payloads.
    #[serde(default)]
    pub monday: Option<Date>,
    /// True for any week fully in the past.
    #[serde(default)]
    pub closed: bool,
}

/// One type for two wire shapes: the aggregate embeds `{ body }`, the note-write
/// route returns `{ iso_week, body, updated_at }`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WeekNote {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iso_week: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<jiff::Timestamp>,
}

#[derive(Serialize)]
struct WeekNoteBody<'a> {
    note: WeekNoteFields<'a>,
}

#[derive(Serialize)]
struct WeekNoteFields<'a> {
    body: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeekDay {
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

/// `state` is the server's `planned` | `live` | `done` | `left`; `done` is its
/// planned→done judgment.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanState {
    Done,
    Live,
    Hold,
    Untouched,
}

impl PlanItem {
    pub fn retro_state(&self) -> PlanState {
        if self.done {
            PlanState::Done
        } else if self.state == "live" {
            PlanState::Live
        } else if self.logged_minutes.unwrap_or(0) == 0 {
            PlanState::Untouched
        } else {
            PlanState::Hold
        }
    }
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
    pub fn items(&self) -> impl Iterator<Item = &PlanItem> {
        self.days.iter().flat_map(|d| d.items.iter())
    }
}

impl ApiClient {
    pub async fn get_week(&self, iso_week: &str) -> Result<Week, ApiError> {
        self.get(&format!("/api/v1/weeks/{iso_week}"), &[]).await
    }

    /// Upserts the week's single note row, so a re-sent write is idempotent.
    pub async fn update_week_note(&self, iso_week: &str, body: &str) -> Result<WeekNote, ApiError> {
        self.patch(
            &format!("/api/v1/weeks/{iso_week}/note"),
            &WeekNoteBody {
                note: WeekNoteFields { body },
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
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
        assert_eq!(week.week.monday, Some(jiff::civil::date(2026, 7, 13)));
        let items: Vec<&PlanItem> = week.items().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "SICP ch.3");
        assert!(items[0].done);
        assert_eq!(items[1].state, "left");
        assert_eq!(week.planned_vs_done.planned, 2);
        assert_eq!(week.planned_vs_done.done, 1);
    }

    #[test]
    fn retro_state_folds_status_and_actuals() {
        let item = |json: serde_json::Value| -> PlanItem { serde_json::from_value(json).unwrap() };
        assert_eq!(
            item(
                serde_json::json!({ "id": 1, "title": "a", "state": "done", "done": true,
                "logged_minutes": 190 })
            )
            .retro_state(),
            PlanState::Done
        );
        assert_eq!(
            item(
                serde_json::json!({ "id": 2, "title": "b", "state": "live", "done": false,
                "logged_minutes": 30 })
            )
            .retro_state(),
            PlanState::Live
        );
        assert_eq!(
            item(
                serde_json::json!({ "id": 3, "title": "c", "state": "planned", "done": false,
                "size_minutes": 180, "logged_minutes": 115 })
            )
            .retro_state(),
            PlanState::Hold
        );
        assert_eq!(
            item(
                serde_json::json!({ "id": 4, "title": "d", "state": "left", "done": false,
                "logged_minutes": 0 })
            )
            .retro_state(),
            PlanState::Untouched
        );
    }

    #[test]
    fn note_and_monday_default_when_absent() {
        let week: Week = serde_json::from_value(serde_json::json!({
            "week": { "id": "2026-W30" },
            "planned_vs_done": {}
        }))
        .unwrap();
        assert_eq!(week.week.monday, None);
        assert!(week.note.body.is_empty());
    }

    #[tokio::test]
    async fn update_week_note_patches_the_wrapped_body_and_reads_back_the_note() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/weeks/2026-W29/note"))
            .and(body_json(serde_json::json!({
                "note": { "body": "Read the paper first, build second." }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "iso_week": "2026-W29",
                "body": "Read the paper first, build second.",
                "updated_at": "2026-07-17T09:30:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
        let note = api
            .update_week_note("2026-W29", "Read the paper first, build second.")
            .await
            .unwrap();
        assert_eq!(note.iso_week, "2026-W29");
        assert_eq!(note.body, "Read the paper first, build second.");
        assert!(note.updated_at.is_some());
    }

    #[tokio::test]
    async fn update_week_note_sends_an_empty_body_to_clear() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/weeks/2026-W29/note"))
            .and(body_json(serde_json::json!({ "note": { "body": "" } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "iso_week": "2026-W29", "body": ""
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into());
        let note = api.update_week_note("2026-W29", "").await.unwrap();
        assert!(note.body.is_empty());
    }
}
