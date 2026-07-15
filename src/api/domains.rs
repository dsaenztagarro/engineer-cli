//! `GET /api/v1/domains` — the domain list, for the target-declare scope picker.
//!
//! A target scoped to a domain is addressed by the domain's id; the picker shows
//! the name. The index carries more (`description`, `color`, `slug`, subdomains),
//! but the declare flow needs only id + name, and serde ignores the rest.

use serde::Deserialize;

use super::{ApiClient, ApiError, List};

#[derive(Debug, Clone, Deserialize)]
pub struct Domain {
    pub id: i64,
    pub name: String,
}

impl ApiClient {
    /// List the user's domains (one page; the set is small).
    pub async fn list_domains(&self) -> Result<Vec<Domain>, ApiError> {
        let list: List<Domain> = self.get("/api/v1/domains", &[]).await?;
        Ok(list.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_domains_unwraps_the_envelope_and_ignores_extra_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "id": 7, "name": "Distributed Systems", "color": "#abc", "slug": "ds",
                      "subdomains": [ { "id": 1, "name": "consensus" } ] },
                    { "id": 9, "name": "Compilers" }
                ],
                "meta": { "page": 1, "per_page": 25, "total": 2 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let domains = api.list_domains().await.unwrap();
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0].id, 7);
        assert_eq!(domains[0].name, "Distributed Systems");
        assert_eq!(domains[1].name, "Compilers");
    }
}
