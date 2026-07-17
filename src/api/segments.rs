//! Completed activity segments — the rows a stopped timer writes.
//!
//! The timer consumes the member calls: PATCH shortens a segment (the audit
//! "trim" preset) and DELETE removes one (the post-save undo, the audit
//! delete). The flagged-segment *list* is a server-side audit feature and gets
//! its client call when that endpoint ships.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, Keyed};

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

#[derive(Serialize)]
struct SegmentCreate {
    started_at: jiff::Timestamp,
    duration_minutes: u32,
}

#[derive(Serialize)]
struct SegmentCreateBody {
    segment: SegmentCreate,
}

impl ApiClient {
    /// Append a manual segment to an existing activity — the `engineer log
    /// --activity` write (after-the-fact time on work already recorded). The
    /// server derives `ended_at` from `started_at + duration_minutes`.
    pub async fn create_segment(
        &self,
        activity_id: i64,
        started_at: jiff::Timestamp,
        minutes: u32,
    ) -> Result<Segment, ApiError> {
        let body = SegmentCreateBody {
            segment: SegmentCreate {
                started_at,
                duration_minutes: minutes,
            },
        };
        self.post(&format!("/api/v1/activities/{activity_id}/segments"), &body)
            .await
    }

    /// The `create_segment` twin carrying an `Idempotency-Key` — the offline
    /// queue's replay path re-sends a deferred segment through this so a lost ack
    /// can never write the segment twice (segment-create is in the server's
    /// opt-in set, ADR 0036, alongside the timer and activity creates). Returns
    /// `Keyed` so the replay pass can see a stored replay.
    pub(crate) async fn create_segment_idempotent(
        &self,
        activity_id: i64,
        started_at: jiff::Timestamp,
        minutes: u32,
        idempotency_key: &str,
    ) -> Result<Keyed<Segment>, ApiError> {
        let body = SegmentCreateBody {
            segment: SegmentCreate {
                started_at,
                duration_minutes: minutes,
            },
        };
        self.post_idempotent(
            &format!("/api/v1/activities/{activity_id}/segments"),
            &body,
            idempotency_key,
        )
        .await
    }

    /// Edit a segment in place — shortening `minutes` is the trim preset.
    /// Segments are nested under their activity on the wire.
    pub async fn update_segment(
        &self,
        activity_id: i64,
        id: i64,
        update: &SegmentUpdate,
    ) -> Result<Segment, ApiError> {
        self.patch(
            &format!("/api/v1/activities/{activity_id}/segments/{id}"),
            update,
        )
        .await
    }

    /// Delete a segment — the exact inverse of the save a stopped timer wrote.
    pub async fn delete_segment(&self, activity_id: i64, id: i64) -> Result<(), ApiError> {
        self.delete(&format!("/api/v1/activities/{activity_id}/segments/{id}"))
            .await
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
            .and(path("/api/v1/activities/9/segments/41"))
            .and(body_json(serde_json::json!({ "minutes": 74 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 41, "activity_id": 9, "minutes": 74
            })))
            .expect(1)
            .mount(&server)
            .await;

        let segment = client(&server)
            .update_segment(9, 41, &SegmentUpdate { minutes: Some(74) })
            .await
            .unwrap();
        assert_eq!(segment.minutes, Some(74));
    }

    #[tokio::test]
    async fn create_segment_posts_wrapped_body_to_nested_route() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/9/segments"))
            .and(body_partial_json(
                serde_json::json!({ "segment": { "duration_minutes": 45 } }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 88, "activity_id": 9, "minutes": 45
            })))
            .expect(1)
            .mount(&server)
            .await;

        let started = jiff::Timestamp::from_second(1_780_000_000).unwrap();
        let seg = client(&server)
            .create_segment(9, started, 45)
            .await
            .unwrap();
        assert_eq!(seg.id, 88);
        assert_eq!(seg.minutes, Some(45));
    }

    #[tokio::test]
    async fn delete_segment_hits_member_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/activities/9/segments/41"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).delete_segment(9, 41).await.unwrap();
    }
}
