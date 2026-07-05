//! The Review pillar — spaced repetition over topics. A topic is keyed by its subdomain_id;
//! freshness is derived on read and a rating is the single write path.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

#[derive(Debug, Clone, Deserialize)]
pub struct Topic {
    pub subdomain_id: i64,
    pub domain_id: i64,
    #[serde(default)]
    pub domain_name: Option<String>,
    #[serde(default)]
    pub subdomain_name: Option<String>,
    pub state: String,
    #[serde(default)]
    pub freshness_fraction: f64,
    #[serde(default)]
    pub reviewed: bool,
    #[serde(default)]
    pub last_reviewed_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub due_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub interval_days: Option<u32>,
    #[serde(default)]
    pub review_count: i64,
    #[serde(default)]
    pub note_count: Option<i64>,
    /// Interval (days) each rating would set: forgot / fuzzy / solid / instant.
    #[serde(default)]
    pub forecasts: BTreeMap<String, u32>,
    /// The prompts — present on the single-topic (show) response.
    #[serde(default)]
    pub notes: Vec<TopicNote>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopicNote {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dashboard {
    pub stats: ReviewStats,
    #[serde(default)]
    pub est_minutes: i64,
    pub heatmap: Heatmap,
    #[serde(default)]
    pub queue: Vec<Topic>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewStats {
    #[serde(default)]
    pub current_streak: i64,
    #[serde(default)]
    pub longest_streak: i64,
    #[serde(default)]
    pub this_month: i64,
    #[serde(default)]
    pub avg_interval: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Heatmap {
    #[serde(default)]
    pub max: i64,
    /// Week columns of 7 cells; a cell is null for days after today.
    #[serde(default)]
    pub weeks: Vec<Vec<Option<HeatCell>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeatCell {
    pub date: String,
    pub count: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateResult {
    pub topic: Topic,
    #[serde(default)]
    pub next_topic: Option<Topic>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewSession {
    pub id: i64,
    pub rating: String,
    #[serde(default)]
    pub reviewed_at: Option<jiff::Timestamp>,
    pub topic: SessionTopic,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionTopic {
    pub subdomain_id: i64,
    pub domain_id: i64,
    #[serde(default)]
    pub domain_name: Option<String>,
    #[serde(default)]
    pub subdomain_name: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct TopicFilters {
    pub domain_id: Option<i64>,
    pub state: Option<String>,
    pub q: Option<String>,
    /// urgency | most_reviewed | least_reviewed | longest_interval | recent | az
    pub sort: Option<String>,
    pub page: Option<u32>,
}

#[derive(Serialize)]
struct RateBody<'a> {
    rating: &'a str,
}

impl ApiClient {
    pub async fn review_dashboard(&self) -> Result<Dashboard, ApiError> {
        self.get("/api/v1/review/dashboard", &[]).await
    }

    pub async fn list_topics(&self, f: &TopicFilters) -> Result<List<Topic>, ApiError> {
        let mut q: Vec<(&str, String)> = vec![];
        if let Some(id) = f.domain_id {
            q.push(("domain_id", id.to_string()));
        }
        if let Some(s) = &f.state {
            q.push(("state", s.clone()));
        }
        if let Some(s) = &f.q {
            if !s.is_empty() {
                q.push(("q", s.clone()));
            }
        }
        if let Some(s) = &f.sort {
            q.push(("sort", s.clone()));
        }
        if let Some(p) = f.page {
            q.push(("page", p.to_string()));
        }
        self.get("/api/v1/review/topics", &q).await
    }

    pub async fn get_topic(&self, subdomain_id: i64) -> Result<Topic, ApiError> {
        self.get(&format!("/api/v1/review/topics/{subdomain_id}"), &[])
            .await
    }

    /// Record a rating (forgot / fuzzy / solid / instant); returns the topic + next due one.
    pub async fn rate_topic(
        &self,
        subdomain_id: i64,
        rating: &str,
    ) -> Result<RateResult, ApiError> {
        self.post(
            &format!("/api/v1/review/topics/{subdomain_id}/rate"),
            &RateBody { rating },
        )
        .await
    }

    pub async fn list_review_sessions(&self) -> Result<List<ReviewSession>, ApiError> {
        self.get("/api/v1/review/sessions", &[]).await
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
    async fn rate_topic_posts_rating_and_parses_next() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/review/topics/5/rate"))
            .and(body_json(serde_json::json!({ "rating": "solid" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "topic": { "subdomain_id": 5, "domain_id": 1, "state": "fresh", "review_count": 2 },
                "next_topic": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(&server).rate_topic(5, "solid").await.unwrap();
        assert_eq!(result.topic.subdomain_id, 5);
        assert_eq!(result.topic.state, "fresh");
        assert!(result.next_topic.is_none());
    }

    #[tokio::test]
    async fn dashboard_parses_stats_and_heatmap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/review/dashboard"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "stats": { "current_streak": 3, "longest_streak": 9, "this_month": 12, "avg_interval": 21 },
                "est_minutes": 5,
                "heatmap": { "max": 4, "weeks": [[{ "date": "2026-06-01", "count": 2 }, null]] },
                "queue": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dash = client(&server).review_dashboard().await.unwrap();
        assert_eq!(dash.stats.current_streak, 3);
        assert_eq!(dash.heatmap.max, 4);
        assert!(dash.heatmap.weeks[0][1].is_none());
    }
}
