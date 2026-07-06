//! Completed activity segments — the rows a stopped timer writes.
//!
//! The timer consumes the member calls: PATCH shortens a segment (the audit
//! "trim" preset) and DELETE removes one (the post-save undo, the audit
//! delete). The flagged-segment *list* is a server-side audit feature and gets
//! its client call when that endpoint ships.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError};

#[derive(Debug, Clone, Deserialize)]
pub struct Segment {
    pub id: i64,
    #[serde(default)]
    pub activity_id: Option<i64>,
    #[serde(default)]
    pub minutes: Option<u32>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub ended_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SegmentUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

impl ApiClient {
    /// Edit a segment in place — the timer's only use is shortening `minutes`.
    pub async fn update_segment(
        &self,
        id: i64,
        update: &SegmentUpdate,
    ) -> Result<Segment, ApiError> {
        self.patch(&format!("/api/v1/segments/{id}"), update).await
    }

    /// Delete a segment — the exact inverse of the save a stopped timer wrote.
    pub async fn delete_segment(&self, id: i64) -> Result<(), ApiError> {
        self.delete(&format!("/api/v1/segments/{id}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    #[tokio::test]
    async fn update_segment_patches_minutes() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/segments/41"))
            .and(body_json(serde_json::json!({ "minutes": 74 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 41, "activity_id": 9, "minutes": 74
            })))
            .expect(1)
            .mount(&server)
            .await;

        let segment = client(&server)
            .update_segment(41, &SegmentUpdate { minutes: Some(74) })
            .await
            .unwrap();
        assert_eq!(segment.minutes, Some(74));
    }

    #[tokio::test]
    async fn delete_segment_hits_member_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/segments/41"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).delete_segment(41).await.unwrap();
    }
}
