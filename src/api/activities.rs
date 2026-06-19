use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

// API model: fields mirror the wire format; the UI reads only a subset today.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub bloom_level: Option<String>,
    #[serde(default)]
    pub domain_id: Option<i64>,
    #[serde(default)]
    pub subdomain_id: Option<i64>,
    #[serde(default)]
    pub duration_minutes: Option<u32>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub ended_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub notes_generated: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ActivityFilters {
    pub started_after: Option<jiff::Timestamp>,
    pub started_before: Option<jiff::Timestamp>,
    pub book_id: Option<i64>,
}

#[derive(Debug, Default, Serialize)]
pub struct ActivityCreate {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bloom_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_generated: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub book_ids: Vec<i64>,
}

#[derive(Serialize)]
struct ActivityCreateBody<'a> {
    activity: &'a ActivityCreate,
}

impl ApiClient {
    pub async fn list_activities(
        &self,
        filters: &ActivityFilters,
    ) -> Result<List<Activity>, ApiError> {
        let mut query: Vec<(&str, String)> = vec![];
        if let Some(t) = filters.started_after {
            query.push(("started_after", t.to_string()));
        }
        if let Some(t) = filters.started_before {
            query.push(("started_before", t.to_string()));
        }
        if let Some(id) = filters.book_id {
            query.push(("book_id", id.to_string()));
        }
        self.get("/api/v1/activities", &query).await
    }

    pub async fn create_activity(&self, body: &ActivityCreate) -> Result<Activity, ApiError> {
        self.post("/api/v1/activities", &ActivityCreateBody { activity: body })
            .await
    }
}
