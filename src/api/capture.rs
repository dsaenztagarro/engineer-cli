//! Assisted-capture source connection (`/api/v1/capture/sources`, ADR 0035).
//!
//! The producers (git commit bursts, calendar events) turn activity into draft
//! tasks the inbox triages (`automations.rs`); this is the other half — opting a
//! *source* in so those drafts appear. Connecting is a settings-backed opt-in
//! dispatched through the server's `Capture::Source` registry: `connect` flips a
//! per-user flag (the calendar also stores a feed URL), `disconnect` flips it off
//! **without deleting captured drafts** (disconnect ≠ delete), and `sync`
//! enqueues a scan.
//!
//! Two contract shapes the client must render honestly:
//!   - **trust** — every source read carries the plain-language `reads` /
//!     `never_reads` / `promise` strings verbatim (the promise is the feature).
//!     A terminal client states them *before* connecting; it never invents its own.
//!   - **requirement** — GitHub OAuth is web-only (ADR 0018), so a git source
//!     with no GitHub connection is not `connectable`: its read carries a
//!     `requirement` pointer at the web connect page instead of `null`, and a
//!     `connect` attempt returns `422` with a distinct problem type. The honest
//!     move is to render the pointer, not retry.

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

/// One capture source (`git` or `calendar`) with its connect state, the
/// plain-language trust copy, and the `requirement` pointer that stands in for
/// `null` when a prerequisite (the git source's web-only GitHub OAuth) is unmet.
/// `params` lists the body keys `connect` accepts (`["feed_url"]` for the
/// calendar, `[]` for git).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CaptureSource {
    pub key: String,
    pub name: String,
    pub connected: bool,
    pub connectable: bool,
    #[serde(default)]
    pub requirement: Option<Requirement>,
    pub trust: Trust,
    #[serde(default)]
    pub params: Vec<String>,
}

impl CaptureSource {
    /// True when this source's `connect` body expects a feed URL (the calendar).
    pub fn wants_feed_url(&self) -> bool {
        self.params.iter().any(|p| p == "feed_url")
    }
}

/// The web-only prerequisite a source can't satisfy over the API (ADR 0018): the
/// git source's GitHub connection. `detail` is the plain-language reason and
/// `url` points at the web connect page — the client renders both instead of
/// failing opaquely.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Requirement {
    pub kind: String,
    pub detail: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// The plain-language trust copy, part of the contract (ADR 0035): what the
/// source reads, what it never reads, and the shared promise that nothing
/// auto-logs. Rendered verbatim before connecting.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Trust {
    pub reads: String,
    pub never_reads: String,
    pub promise: String,
}

/// The `202 Accepted` body from `sync` — an enqueue, not a completed scan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SyncQueued {
    pub queued: bool,
    pub key: String,
}

#[derive(Serialize)]
struct ConnectBody<'a> {
    feed_url: &'a str,
}

impl ApiClient {
    /// List the capture sources with their connect state, trust copy, and
    /// requirement pointers.
    pub async fn list_capture_sources(&self) -> Result<Vec<CaptureSource>, ApiError> {
        let list: List<CaptureSource> = self.get("/api/v1/capture/sources", &[]).await?;
        Ok(list.data)
    }

    /// Opt a source in. The calendar carries `feed_url`; git sends no body. On a
    /// git source with no GitHub connection the server returns `422` with the
    /// `capture-source-requirement` problem type; a bad feed URL is a field-level
    /// `422`. Either way the caller renders the problem honestly.
    pub async fn connect_capture_source(
        &self,
        key: &str,
        feed_url: Option<&str>,
    ) -> Result<CaptureSource, ApiError> {
        let path = format!("/api/v1/capture/sources/{key}/connect");
        match feed_url {
            Some(url) => self.post(&path, &ConnectBody { feed_url: url }).await,
            None => self.post_empty(&path).await,
        }
    }

    /// Turn a source off. Returns the fresh (disconnected) source. Captured
    /// drafts already in the inbox **survive** — disconnect is not delete.
    pub async fn disconnect_capture_source(&self, key: &str) -> Result<CaptureSource, ApiError> {
        self.delete_json(&format!("/api/v1/capture/sources/{key}/connect"))
            .await
    }

    /// Enqueue a scan for a connected source (`202`). A disconnected source is a
    /// `422` and enqueues nothing.
    pub async fn sync_capture_source(&self, key: &str) -> Result<SyncQueued, ApiError> {
        self.post_empty(&format!("/api/v1/capture/sources/{key}/sync"))
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
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "t".into())
    }

    fn git_source(connected: bool, connectable: bool) -> serde_json::Value {
        let requirement = if connectable {
            serde_json::Value::Null
        } else {
            serde_json::json!({
                "kind": "github_connection",
                "detail": "Connect GitHub first — the scan uses your own connection.",
                "url": "http://www.example.com/github/connect"
            })
        };
        serde_json::json!({
            "key": "git", "name": "Git activity",
            "connected": connected, "connectable": connectable,
            "requirement": requirement,
            "trust": {
                "reads": "Commit times and counts on repositories your activities anchor.",
                "never_reads": "Never messages, never code.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": []
        })
    }

    fn calendar_source(connected: bool) -> serde_json::Value {
        serde_json::json!({
            "key": "calendar", "name": "Study calendar",
            "connected": connected, "connectable": true, "requirement": null,
            "trust": {
                "reads": "The titles and times of past events on this one calendar.",
                "never_reads": "Never descriptions, never attendees, never anything upcoming.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": ["feed_url"]
        })
    }

    #[tokio::test]
    async fn list_unwraps_the_envelope_and_the_requirement_pointer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/capture/sources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ git_source(false, false), calendar_source(false) ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let sources = client(&server).list_capture_sources().await.unwrap();
        assert_eq!(sources.len(), 2);
        let git = &sources[0];
        assert_eq!(git.key, "git");
        assert!(!git.connectable);
        // The requirement pointer stands in for `null` when GitHub isn't connected.
        let req = git.requirement.as_ref().expect("git carries a requirement");
        assert_eq!(req.kind, "github_connection");
        assert_eq!(
            req.url.as_deref(),
            Some("http://www.example.com/github/connect")
        );
        assert_eq!(
            git.trust.never_reads, "Never messages, never code.",
            "trust strings ride in-payload verbatim"
        );
        // The calendar is connectable and takes a feed URL.
        let cal = &sources[1];
        assert!(cal.connectable && cal.requirement.is_none());
        assert!(cal.wants_feed_url());
    }

    #[tokio::test]
    async fn connect_git_sends_no_body_and_returns_the_fresh_source() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(git_source(true, true)))
            .expect(1)
            .mount(&server)
            .await;
        let src = client(&server)
            .connect_capture_source("git", None)
            .await
            .unwrap();
        assert!(src.connected);
    }

    #[tokio::test]
    async fn connect_calendar_sends_the_feed_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/connect"))
            .and(body_json(
                serde_json::json!({ "feed_url": "https://calendar.example.com/basic.ics" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(calendar_source(true)))
            .expect(1)
            .mount(&server)
            .await;
        let src = client(&server)
            .connect_capture_source("calendar", Some("https://calendar.example.com/basic.ics"))
            .await
            .unwrap();
        assert!(src.connected);
    }

    #[tokio::test]
    async fn connect_git_without_github_is_the_requirement_problem_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/capture-source-requirement",
                "title": "Source requirement not met",
                "status": 422,
                "detail": "Connect GitHub first — the scan uses your own connection."
            })))
            .mount(&server)
            .await;
        let err = client(&server)
            .connect_capture_source("git", None)
            .await
            .unwrap_err();
        match err {
            ApiError::Problem {
                status,
                type_uri,
                detail,
                ..
            } => {
                assert_eq!(status, 422);
                assert_eq!(
                    type_uri.as_deref(),
                    Some("https://engineer.example/problems/capture-source-requirement")
                );
                assert!(detail.contains("Connect GitHub first"));
            }
            _ => panic!("expected a Problem"),
        }
    }

    #[tokio::test]
    async fn connect_calendar_with_a_bad_feed_url_is_a_field_validation_422() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/connect"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/validation",
                "title": "Validation failed",
                "status": 422,
                "detail": "Feed url is not a valid https URL",
                "errors": [ { "field": "calendar_feed_url", "detail": "must be https" } ]
            })))
            .mount(&server)
            .await;
        let err = client(&server)
            .connect_capture_source("calendar", Some("http://insecure.example/cal.ics"))
            .await
            .unwrap_err();
        assert_eq!(err.field_errors().len(), 1);
        assert_eq!(err.field_errors()[0].field, "calendar_feed_url");
    }

    #[tokio::test]
    async fn disconnect_returns_the_fresh_source_flag_off() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(git_source(false, true)))
            .expect(1)
            .mount(&server)
            .await;
        let src = client(&server)
            .disconnect_capture_source("git")
            .await
            .unwrap();
        assert!(!src.connected);
    }

    #[tokio::test]
    async fn sync_a_connected_source_queues_the_scan() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/sync"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "queued": true, "key": "calendar"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let queued = client(&server)
            .sync_capture_source("calendar")
            .await
            .unwrap();
        assert!(queued.queued);
        assert_eq!(queued.key, "calendar");
    }

    #[tokio::test]
    async fn sync_a_disconnected_source_is_a_422() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/sync"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/unprocessable-content",
                "title": "Source not connected",
                "status": 422,
                "detail": "Connect the Git activity source before syncing it."
            })))
            .mount(&server)
            .await;
        let err = client(&server)
            .sync_capture_source("git")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Problem { status: 422, .. }));
    }
}
