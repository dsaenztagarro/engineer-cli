//! HTTP client for the Engineer JSON API.
//!
//! All endpoints are protected by RFC 6750 Bearer tokens validated server-side
//! via RFC 7662 token introspection. Errors come back as RFC 7807 problem+json.

use reqwest::{header, Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::auth::TokenProvider;

mod activities;
mod audit;
mod automations;
mod books;
mod domains;
mod envelope;
mod error;
mod notes;
mod progress;
mod review;
mod segments;
mod targets;
mod timer;
mod today;
mod weeks;

pub use activities::{Activity, ActivityCreate, ActivityFilters, ActivityUpdate};
pub use audit::{AuditAcknowledged, AuditRead, AuditSegment};
pub use automations::Task;
pub use books::{Book, BookChapter, BookStatus, BookUpdate};
pub use domains::Domain;
pub use envelope::List;
pub use error::{codes, ApiError, ConflictInfo, FieldError};
// Re-exported for naming the `current` snapshot's type outside `api`; today's
// consumers reach it through `ConflictInfo::current`, so the name itself has
// no callers yet.
#[allow(unused_imports)]
pub use error::ConflictTimer;
pub use notes::{Anchor, Note, NoteFilters, NoteInput};
pub use progress::{DayMinutes, PaceState, Progress, ProgressReading, TargetRef};
pub use review::{Dashboard, RateResult, Topic, TopicFilters};
pub use segments::SegmentUpdate;
pub use targets::{TargetCreate, TargetScope, TargetState};
pub use timer::{ReclaimVerb, Reclaimed, Timer, TimerCandidate, TimerSettings, TimerStopped};
pub use today::Today;
pub use weeks::{PlanItem, PlanState, Week, WeekNote};

/// Current user from `GET /api/v1/me`. Fields mirror the API contract; not all
/// are consumed by the UI yet.
#[derive(serde::Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct Me {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub admin: bool,
}

/// The replay-dedupe header the server contract keys on (engineer#806).
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// Set by the server when a keyed write was answered byte-identically from the
/// stored first execution instead of re-running (engineer#806, ADR 0036).
const IDEMPOTENCY_REPLAYED_HEADER: &str = "Idempotency-Replayed";

/// A keyed write's response plus whether the server answered it from the
/// idempotency store (`Idempotency-Replayed: true`). The value is the normal
/// parsed body either way — a stored replay is indistinguishable from the
/// first ack by design; the flag exists for the replay pass's telemetry and
/// the dedupe tests.
#[derive(Debug)]
pub(crate) struct Keyed<T> {
    pub value: T,
    pub replayed: bool,
}

#[derive(Clone)]
pub struct ApiClient {
    base: Url,
    http: Client,
    auth: Auth,
}

// `Provider` is the runtime path; `Static` is only the CLI/test path. The size
// gap is irrelevant for a two-variant enum constructed once per client.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Auth {
    Provider(TokenProvider),
    Static(String),
}

impl ApiClient {
    pub fn new(base: Url, provider: TokenProvider) -> Self {
        Self {
            base,
            http: Client::new(),
            auth: Auth::Provider(provider),
        }
    }

    pub fn with_token(base: Url, token: String) -> Self {
        Self {
            base,
            http: Client::new(),
            auth: Auth::Static(token),
        }
    }

    async fn token(&self) -> Result<String, ApiError> {
        match &self.auth {
            Auth::Static(t) => Ok(t.clone()),
            Auth::Provider(p) => p
                .access_token()
                .await
                .map_err(|e| ApiError::Transport(e.to_string())),
        }
    }

    fn url(&self, path: &str) -> Result<Url, ApiError> {
        self.base
            .join(path)
            .map_err(|e| ApiError::Transport(e.to_string()))
    }

    async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ApiError> {
        let token = self.token().await?;
        Ok(self
            .http
            .request(method, self.url(path)?)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "application/json"))
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ApiError> {
        let req = self.request(Method::GET, path).await?.query(query);
        send(req).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.request(Method::POST, path).await?.json(body);
        send(req).await
    }

    async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.request(Method::PATCH, path).await?.json(body);
        send(req).await
    }

    async fn delete(&self, path: &str) -> Result<(), ApiError> {
        let req = self.request(Method::DELETE, path).await?;
        send(req).await
    }

    // POST for member actions that take no request body (pause, resume, stop, …)
    // but return the updated resource.
    async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let req = self.request(Method::POST, path).await?;
        send(req).await
    }

    // The `post`/`post_empty` twins that carry an `Idempotency-Key` header —
    // the offline queue's replay path (`crate::queue`) re-sends stored intents
    // through these so a lost ack can never double-write. They return `Keyed`
    // so the replay pass can see when the answer was a stored replay.
    pub(crate) async fn post_idempotent<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        idempotency_key: &str,
    ) -> Result<Keyed<T>, ApiError> {
        let req = self
            .request(Method::POST, path)
            .await?
            .header(IDEMPOTENCY_HEADER, idempotency_key)
            .json(body);
        send_keyed(req).await
    }

    pub(crate) async fn post_empty_idempotent<T: DeserializeOwned>(
        &self,
        path: &str,
        idempotency_key: &str,
    ) -> Result<Keyed<T>, ApiError> {
        let req = self
            .request(Method::POST, path)
            .await?
            .header(IDEMPOTENCY_HEADER, idempotency_key);
        send_keyed(req).await
    }

    // PATCH for member actions that take no request body (unlink, archive,
    // unarchive) but return the updated resource.
    async fn patch_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let req = self.request(Method::PATCH, path).await?;
        send(req).await
    }

    pub async fn me(&self) -> Result<Me, ApiError> {
        self.get("/api/v1/me", &[]).await
    }
}

async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ApiError> {
    Ok(send_keyed(req).await?.value)
}

// `send` plus the `Idempotency-Replayed` verdict — the keyed-write path reads
// the flag; everything else drops it via `send`.
async fn send_keyed<T: DeserializeOwned>(req: RequestBuilder) -> Result<Keyed<T>, ApiError> {
    // Split so we can log the method + URL of every call on a dedicated target.
    // We never log the Authorization header or token; URLs (incl. query params
    // like `status`/`q`) carry no secret. Response bodies are logged only on
    // error, since success bodies may include PII (e.g. the user's email).
    let (client, request) = req.build_split();
    let request = request.map_err(|e| ApiError::Transport(e.to_string()))?;
    let method = request.method().clone();
    let url = request.url().clone();

    let started = std::time::Instant::now();
    let resp = match client.execute(request).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(target: "engineer_cli::api", %method, %url, error = %e, "api call failed");
            return Err(ApiError::Transport(e.to_string()));
        }
    };
    let status = resp.status();
    let replayed = resp
        .headers()
        .get(IDEMPOTENCY_REPLAYED_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let latency_ms = started.elapsed().as_millis();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;

    if status.is_success() {
        tracing::info!(target: "engineer_cli::api", %method, %url, status = status.as_u16(), latency_ms, idempotency_replayed = replayed, "api call");
        if bytes.is_empty() && std::any::type_name::<T>().contains("()") {
            // Caller expects unit; serde_json can't deserialize empty into ().
            let value =
                serde_json::from_str("null").map_err(|e| ApiError::Decode(e.to_string()))?;
            return Ok(Keyed { value, replayed });
        }
        let value = serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(e.to_string()))?;
        return Ok(Keyed { value, replayed });
    }

    let detail = String::from_utf8_lossy(&bytes);
    tracing::warn!(target: "engineer_cli::api", %method, %url, status = status.as_u16(), latency_ms, %detail, "api call error");
    Err(ApiError::from_response(status, &bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn me_requests_api_v1_me() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7,
                "email": "alice@example.com",
                "name": "Alice",
                "admin": false
            })))
            .expect(1) // verified on drop: exactly one hit on /api/v1/me
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let me = api.me().await.expect("me() should succeed");

        assert_eq!(me.id, 7);
        assert_eq!(me.email, "alice@example.com");
    }
}
