use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, Keyed, List};

// API model: fields mirror the wire format; the UI reads only a subset today.
// `Default` seeds the provisional stand-in an offline `create`/`update`/`archive`
// returns (`queue::QueuedClient`) — a negative-id row the board renders `◔ queued`.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Activity {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub intent: Option<String>,
    /// Lifecycle status the activities table renders as a semantic pill
    /// (planned / started / completed …). Free-form on the wire so an
    /// unrecognised value still renders literally instead of failing to decode.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub bloom_level: Option<String>,
    #[serde(default)]
    pub domain_id: Option<i64>,
    #[serde(default)]
    pub subdomain_id: Option<i64>,
    /// The domain's display name, when the server side-loads it — the table
    /// signals domain by name (the terminal palette has no per-domain colours).
    #[serde(default)]
    pub domain_name: Option<String>,
    #[serde(default)]
    pub duration_minutes: Option<u32>,
    #[serde(default)]
    pub segments_count: Option<u32>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub ended_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub archived_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub notes_generated: Option<String>,
}

impl Activity {
    /// Whether this activity is archived (a reversible, quiet state — the table
    /// toggles it without a confirm).
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ActivityFilters {
    pub started_after: Option<jiff::Timestamp>,
    pub started_before: Option<jiff::Timestamp>,
    pub book_id: Option<i64>,
    /// Lifecycle status filter (server-side); the table cycles this with `f`.
    pub status: Option<String>,
    /// Kind filter (server-side); unused by the table today (kind is folded into
    /// the client-side `/` filter), kept for parity with the server contract.
    pub kind: Option<String>,
    /// "all" folds archived rows back in; None is active-only.
    pub archived: Option<String>,
    /// 1-based page — the first surface to drive `meta.page` pagination.
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

// `Clone + PartialEq + Deserialize` so the whole body can ride an
// `IntentKind::ActivityCreate` into `queue.json` and re-send verbatim on replay
// (the plan-write offline seam — `queue::intent`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
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
    /// A plan item is a planned activity carrying this `planned_on` day (status
    /// defaults to `planned` server-side). Set by `engineer plan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_on: Option<jiff::civil::Date>,
    /// The plan item's rough size — the retro's planned-vs-done judges "done" as
    /// logged ≥ half of this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_duration_minutes: Option<u32>,
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

/// The subset of an activity a plan-item adjust (`e` on the board) edits in
/// place via `PATCH /api/v1/activities/:id`. Only set fields serialize, so a
/// title-only edit sends `{ activity: { title } }` and leaves the rest alone.
#[derive(Debug, Default, Serialize)]
pub struct ActivityUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Serialize)]
struct ActivityUpdateBody<'a> {
    activity: &'a ActivityUpdate,
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
        if let Some(s) = &filters.status {
            if !s.is_empty() {
                query.push(("status", s.clone()));
            }
        }
        if let Some(k) = &filters.kind {
            if !k.is_empty() {
                query.push(("kind", k.clone()));
            }
        }
        if let Some(a) = &filters.archived {
            query.push(("archived", a.clone()));
        }
        if let Some(p) = filters.page {
            query.push(("page", p.to_string()));
        }
        if let Some(pp) = filters.per_page {
            query.push(("per_page", pp.to_string()));
        }
        self.get("/api/v1/activities", &query).await
    }

    pub async fn get_activity(&self, id: i64) -> Result<Activity, ApiError> {
        self.get(&format!("/api/v1/activities/{id}"), &[]).await
    }

    pub async fn create_activity(&self, body: &ActivityCreate) -> Result<Activity, ApiError> {
        self.post("/api/v1/activities", &ActivityCreateBody { activity: body })
            .await
    }

    /// The `create_activity` twin carrying an `Idempotency-Key` — the plan-write
    /// queue's replay path re-sends a deferred declare through this so a lost ack
    /// can never mint the plan item twice (engineer#806, the same contract the
    /// timer verbs replay under).
    pub(crate) async fn create_activity_idempotent(
        &self,
        body: &ActivityCreate,
        idempotency_key: &str,
    ) -> Result<Keyed<Activity>, ApiError> {
        self.post_idempotent(
            "/api/v1/activities",
            &ActivityCreateBody { activity: body },
            idempotency_key,
        )
        .await
    }

    /// Edit an activity in place — the plan-item adjust (`e` on the board) and
    /// its replay path. A plain PATCH: update/archive replay idempotently on the
    /// server without a key (re-sending the same title is naturally idempotent).
    pub async fn update_activity(
        &self,
        id: i64,
        body: &ActivityUpdate,
    ) -> Result<Activity, ApiError> {
        self.patch(
            &format!("/api/v1/activities/{id}"),
            &ActivityUpdateBody { activity: body },
        )
        .await
    }

    /// Mark the activity done — a member action that returns the updated record.
    pub async fn complete_activity(&self, id: i64) -> Result<Activity, ApiError> {
        self.post_empty(&format!("/api/v1/activities/{id}/complete"))
            .await
    }

    /// Archive / unarchive — reversible, so the table toggles quietly (no confirm),
    /// mirroring the notes resource's PATCH member routes.
    pub async fn archive_activity(&self, id: i64) -> Result<Activity, ApiError> {
        self.patch_empty(&format!("/api/v1/activities/{id}/archive"))
            .await
    }

    pub async fn unarchive_activity(&self, id: i64) -> Result<Activity, ApiError> {
        self.patch_empty(&format!("/api/v1/activities/{id}/unarchive"))
            .await
    }

    /// "Do this again" — the server mints a planned copy and returns it.
    pub async fn duplicate_activity(&self, id: i64) -> Result<Activity, ApiError> {
        self.post_empty(&format!("/api/v1/activities/{id}/duplicate"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn page_body() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{ "id": 1, "title": "Read SICP", "status": "completed" }],
            "meta": { "page": 2, "per_page": 25, "total": 42 }
        }))
    }

    #[tokio::test]
    async fn list_sends_status_and_page_and_parses_meta() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/activities"))
            .and(query_param("status", "completed"))
            .and(query_param("page", "2"))
            .respond_with(page_body())
            .expect(1)
            .mount(&server)
            .await;

        let filters = ActivityFilters {
            status: Some("completed".into()),
            page: Some(2),
            ..Default::default()
        };
        let list = client(&server).list_activities(&filters).await.unwrap();
        assert_eq!(list.data.len(), 1);
        assert_eq!(list.meta.page, 2);
        assert_eq!(list.meta.total, 42);
        assert_eq!(list.meta.per_page, 25);
    }

    #[tokio::test]
    async fn complete_posts_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/7/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "T", "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = client(&server).complete_activity(7).await.unwrap();
        assert_eq!(a.status.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn archive_patches_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/9/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 9, "title": "T", "archived_at": "2026-07-01T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = client(&server).archive_activity(9).await.unwrap();
        assert!(a.is_archived());
    }

    #[tokio::test]
    async fn create_posts_a_wrapped_planned_activity() {
        let server = MockServer::start().await;
        // A plan item: a planned activity carrying `planned_on` and its rough
        // size, wrapped under `activity` — the exact body `engineer plan` and the
        // board's `a` gesture send.
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .and(body_json(serde_json::json!({
                "activity": {
                    "title": "one systems paper",
                    "kind": "reading",
                    "planned_on": "2026-07-13",
                    "target_duration_minutes": 60
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7, "title": "one systems paper", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let create = ActivityCreate {
            title: "one systems paper".into(),
            kind: Some("reading".into()),
            planned_on: Some("2026-07-13".parse().unwrap()),
            target_duration_minutes: Some(60),
            ..Default::default()
        };
        let a = client(&server).create_activity(&create).await.unwrap();
        assert_eq!(a.id, 7);
    }

    #[tokio::test]
    async fn update_patches_a_wrapped_title() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/9"))
            .and(body_json(serde_json::json!({
                "activity": { "title": "revised" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 9, "title": "revised", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let update = ActivityUpdate {
            title: Some("revised".into()),
        };
        let a = client(&server).update_activity(9, &update).await.unwrap();
        assert_eq!(a.title, "revised");
    }

    #[tokio::test]
    async fn duplicate_posts_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/3/duplicate"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "title": "Read SICP", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = client(&server).duplicate_activity(3).await.unwrap();
        assert_eq!(a.id, 88);
        assert_eq!(a.status.as_deref(), Some("planned"));
    }
}
