//! The segment audit (`Progress ▸ Segment audit`): flagged segments derived on
//! read — implausibly long, zero/near-zero, missing metadata — plus the
//! acknowledge action that stamps `audit_acknowledged_at`. Trim and delete are
//! ordinary segment PATCH/DELETE (`src/api/segments.rs`), not audit verbs.
#![allow(dead_code)]

use serde::Deserialize;

use super::{ApiClient, ApiError};

#[derive(Debug, Clone, Deserialize)]
pub struct AuditRead {
    /// The user's total flagged rows — the badge number.
    pub audit_count: u32,
    /// Newest-first flagged segments.
    pub segments: Vec<AuditSegment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuditSegment {
    pub id: i64,
    pub activity_id: i64,
    #[serde(default)]
    pub activity_title: Option<String>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub ended_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub duration_minutes: Option<u32>,
    #[serde(default)]
    pub formatted_duration: Option<String>,
    /// Any of `too_long`, `near_zero`, `missing_kind`, `missing_anchor`.
    #[serde(default)]
    pub flags: Vec<String>,
}

/// The acknowledge response: the segment's remaining flags (the duration
/// flags clear permanently; missing-metadata flags survive until fixed) and
/// the user's new flagged total.
#[derive(Debug, Clone, Deserialize)]
pub struct AuditAcknowledged {
    pub acknowledged: bool,
    pub segment_id: i64,
    #[serde(default)]
    pub flags: Vec<String>,
    pub audit_count: u32,
}

impl ApiClient {
    pub async fn progress_audit(&self) -> Result<AuditRead, ApiError> {
        self.get("/api/v1/progress/audit", &[]).await
    }

    /// "Looks right" — stamps the segment acknowledged, clearing its
    /// duration-shape flags for good.
    pub async fn acknowledge_audit_segment(
        &self,
        segment_id: i64,
    ) -> Result<AuditAcknowledged, ApiError> {
        self.patch_empty(&format!(
            "/api/v1/progress/audit/segments/{segment_id}/acknowledge"
        ))
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    #[tokio::test]
    async fn audit_read_decodes_rows_and_flags() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress/audit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "audit_count": 2,
                "segments": [
                    {
                        "id": 41, "activity_id": 9,
                        "activity_title": "Read DDIA ch.7",
                        "duration_minutes": 161,
                        "formatted_duration": "2h41m",
                        "flags": ["too_long"]
                    },
                    {
                        "id": 44, "activity_id": 12,
                        "activity_title": "Untitled timer",
                        "duration_minutes": 65,
                        "flags": ["missing_kind", "missing_anchor"]
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let audit = client(&server).progress_audit().await.unwrap();
        assert_eq!(audit.audit_count, 2);
        assert_eq!(audit.segments[0].flags, vec!["too_long"]);
        assert_eq!(audit.segments[1].flags.len(), 2);
    }

    #[tokio::test]
    async fn acknowledge_patches_the_member_and_returns_remaining_flags() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/progress/audit/segments/41/acknowledge"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "acknowledged": true, "segment_id": 41,
                "flags": [], "audit_count": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ack = client(&server).acknowledge_audit_segment(41).await.unwrap();
        assert!(ack.acknowledged);
        assert!(ack.flags.is_empty());
        assert_eq!(ack.audit_count, 1);
    }
}
