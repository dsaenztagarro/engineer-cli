//! Assisted-capture — Human-in-the-Loop automation tasks (`/api/v1/automations`).
//!
//! The pipeline turns activity (e.g. a git commit) into a *draft* task the user
//! triages: **acknowledge** (seen), **complete** (accept — fires the automation's
//! `on_complete`, the write that mints the activity), or **reject** (discard).
//! The CLI consumes tasks; it never authors them (no create/destroy).

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

/// A draft task awaiting triage. `prompt` is the human question, `entity` the
/// thing it targets, `expires_at` the due-badge source.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub id: i64,
    #[serde(default)]
    pub automation: Option<String>,
    pub status: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub context: serde_json::Value,
    #[serde(default)]
    pub entity: Option<Entity>,
    #[serde(default)]
    pub expires_at: Option<jiff::Timestamp>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entity {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Serialize)]
struct RejectBody {
    reason: String,
}

impl ApiClient {
    /// The inbox default — pending drafts, most recent first.
    pub async fn list_pending_tasks(&self) -> Result<Vec<Task>, ApiError> {
        let list: List<Task> = self.get("/api/v1/automations/tasks/pending", &[]).await?;
        Ok(list.data)
    }

    pub async fn get_task(&self, id: i64) -> Result<Task, ApiError> {
        self.get(&format!("/api/v1/automations/tasks/{id}"), &[])
            .await
    }

    /// Mark seen (keep for later).
    pub async fn acknowledge_task(&self, id: i64) -> Result<Task, ApiError> {
        self.patch_empty(&format!("/api/v1/automations/tasks/{id}/acknowledge"))
            .await
    }

    /// Accept — the server's `complete`, which fires `on_complete` (mints the
    /// activity). Resolution defaults to `completed` when no body is sent.
    pub async fn complete_task(&self, id: i64) -> Result<Task, ApiError> {
        self.patch_empty(&format!("/api/v1/automations/tasks/{id}/complete"))
            .await
    }

    /// Discard, with an optional reason.
    pub async fn reject_task(&self, id: i64, reason: Option<String>) -> Result<Task, ApiError> {
        let path = format!("/api/v1/automations/tasks/{id}/reject");
        match reason {
            Some(reason) => self.patch(&path, &RejectBody { reason }).await,
            None => self.patch_empty(&path).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into())
    }

    fn task(id: i64, status: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "automation": "git_commit", "status": status,
            "prompt": "Log commit \"fix parser\"?", "context": { "sha": "abc" },
            "entity": { "id": 3, "type": "Activity", "name": "Crafting Interpreters" },
            "expires_at": "2026-07-16T00:00:00Z"
        })
    }

    #[tokio::test]
    async fn pending_unwraps_the_flat_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/automations/tasks/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ task(42, "pending") ], "page": 1, "per_page": 25, "total": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tasks = client(&server).list_pending_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 42);
        assert_eq!(
            tasks[0].entity.as_ref().unwrap().name.as_deref(),
            Some("Crafting Interpreters")
        );
    }

    #[tokio::test]
    async fn complete_patches_the_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/42/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(task(42, "completed")))
            .expect(1)
            .mount(&server)
            .await;
        let t = client(&server).complete_task(42).await.unwrap();
        assert_eq!(t.status, "completed");
    }

    #[tokio::test]
    async fn reject_with_reason_sends_it() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/42/reject"))
            .and(body_json(serde_json::json!({ "reason": "not real work" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(task(42, "rejected")))
            .expect(1)
            .mount(&server)
            .await;
        let t = client(&server)
            .reject_task(42, Some("not real work".into()))
            .await
            .unwrap();
        assert_eq!(t.status, "rejected");
    }
}
