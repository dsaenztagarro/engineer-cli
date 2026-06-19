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
    let resp = req.send().await.map_err(|e| ApiError::Transport(e.to_string()))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| ApiError::Transport(e.to_string()))?;
    if status.is_success() {
        if bytes.is_empty() && std::any::type_name::<T>().contains("()") {
            // Caller expects unit; serde_json can't deserialize empty into ().
            return serde_json::from_str("null").map_err(|e| ApiError::Decode(e.to_string()));
        }
        return serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(e.to_string()));
    }
    Err(ApiError::from_response(status, &bytes))
}

#[allow(dead_code)]
pub(crate) const _: StatusCode = StatusCode::OK; // keep import if unused elsewhere
