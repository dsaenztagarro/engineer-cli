//! HTTP client for the Engineer JSON API.
//!
//! All endpoints are protected by RFC 6750 Bearer tokens validated server-side
//! via RFC 7662 token introspection. Errors come back as RFC 7807 problem+json.

use reqwest::{header, Client, Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::auth::TokenProvider;

mod activities;
mod books;
mod envelope;
mod error;

pub use activities::{Activity, ActivityCreate, ActivityFilters};
pub use books::{Book, BookChapter, BookStatus, BookUpdate};
pub use envelope::{List, Meta};
pub use error::{ApiError, FieldError};

/// Public for the `me` call during login (no token provider yet).
#[derive(serde::Deserialize, Debug, Clone)]
pub struct Me {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub admin: bool,
}

#[derive(Clone)]
pub struct ApiClient {
    base: Url,
    http: Client,
    auth: Auth,
}

#[derive(Clone)]
enum Auth {
    Provider(TokenProvider),
    Static(String),
}

impl ApiClient {
    pub fn new(base: Url, provider: TokenProvider) -> Self {
        Self { base, http: Client::new(), auth: Auth::Provider(provider) }
    }

    pub fn with_token(base: Url, token: String) -> Self {
        Self { base, http: Client::new(), auth: Auth::Static(token) }
    }

    async fn token(&self) -> Result<String, ApiError> {
        match &self.auth {
            Auth::Static(t) => Ok(t.clone()),
            Auth::Provider(p) => p.access_token().await.map_err(|e| ApiError::Transport(e.to_string())),
        }
    }

    fn url(&self, path: &str) -> Result<Url, ApiError> {
        self.base.join(path).map_err(|e| ApiError::Transport(e.to_string()))
    }

    async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ApiError> {
        let token = self.token().await?;
        Ok(self
            .http
            .request(method, self.url(path)?)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "application/json"))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, ApiError> {
        let req = self.request(Method::GET, path).await?.query(query);
        send(req).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ApiError> {
        let req = self.request(Method::POST, path).await?.json(body);
        send(req).await
    }

    async fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ApiError> {
        let req = self.request(Method::PATCH, path).await?.json(body);
        send(req).await
    }

    pub async fn me(&self) -> Result<Me, ApiError> {
        self.get("/api/v1/me", &[]).await
    }
}

async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ApiError> {
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
            tracing::warn!(target: "engineer_tui::api", %method, %url, error = %e, "api call failed");
            return Err(ApiError::Transport(e.to_string()));
        }
    };
    let status = resp.status();
    let latency_ms = started.elapsed().as_millis();
    let bytes = resp.bytes().await.map_err(|e| ApiError::Transport(e.to_string()))?;

    if status.is_success() {
        tracing::info!(target: "engineer_tui::api", %method, %url, status = status.as_u16(), latency_ms, "api call");
        if bytes.is_empty() && std::any::type_name::<T>().contains("()") {
            // Caller expects unit; serde_json can't deserialize empty into ().
            return serde_json::from_str("null").map_err(|e| ApiError::Decode(e.to_string()));
        }
        return serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(e.to_string()));
    }

    let detail = String::from_utf8_lossy(&bytes);
    tracing::warn!(target: "engineer_tui::api", %method, %url, status = status.as_u16(), latency_ms, %detail, "api call error");
    Err(ApiError::from_response(status, &bytes))
}

#[allow(dead_code)]
pub(crate) const _: StatusCode = StatusCode::OK; // keep import if unused elsewhere

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
