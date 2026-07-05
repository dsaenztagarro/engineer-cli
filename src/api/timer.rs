//! The user's single live study timer. Stopping a bound timer writes an ActivitySegment.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError};

#[derive(Debug, Clone, Deserialize)]
pub struct Timer {
    pub running: bool,
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub bound: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub activity_id: Option<i64>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub elapsed_seconds: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimerStopped {
    pub stopped: bool,
    pub activity_id: i64,
    pub segment_id: i64,
    pub minutes: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimerCandidate {
    pub id: i64,
    pub title: String,
}

#[derive(Serialize)]
struct StartBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    switch: Option<bool>,
}

#[derive(Serialize)]
struct BindBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

impl ApiClient {
    pub async fn timer(&self) -> Result<Timer, ApiError> {
        self.get("/api/v1/timer", &[]).await
    }

    /// Start a timer, optionally bound to an activity. `switch` stops the running timer first.
    pub async fn start_timer(
        &self,
        activity_id: Option<i64>,
        switch: bool,
    ) -> Result<Timer, ApiError> {
        self.post(
            "/api/v1/timer",
            &StartBody {
                activity_id,
                switch: switch.then_some(true),
            },
        )
        .await
    }

    pub async fn pause_timer(&self) -> Result<Timer, ApiError> {
        self.post_empty("/api/v1/timer/pause").await
    }

    pub async fn resume_timer(&self) -> Result<Timer, ApiError> {
        self.post_empty("/api/v1/timer/resume").await
    }

    /// Stop the timer, writing a segment on the bound activity. Fails if unbound.
    pub async fn stop_timer(&self) -> Result<TimerStopped, ApiError> {
        self.post_empty("/api/v1/timer/stop").await
    }

    /// Bind an unnamed timer to an existing activity or a new one created from `title`.
    pub async fn bind_timer(
        &self,
        activity_id: Option<i64>,
        title: Option<String>,
    ) -> Result<Timer, ApiError> {
        self.post("/api/v1/timer/bind", &BindBody { activity_id, title })
            .await
    }

    pub async fn timer_candidates(&self, q: Option<&str>) -> Result<Vec<TimerCandidate>, ApiError> {
        let query: Vec<(&str, String)> = q
            .filter(|s| !s.is_empty())
            .map(|s| ("q", s.to_string()))
            .into_iter()
            .collect();
        self.get("/api/v1/timer/candidates", &query).await
    }

    pub async fn discard_timer(&self) -> Result<(), ApiError> {
        self.delete("/api/v1/timer").await
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
    async fn start_timer_bound_posts_activity_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(serde_json::json!({ "activity_id": 9 })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).start_timer(Some(9), false).await.unwrap();
        assert!(timer.running);
        assert!(timer.bound);
    }

    #[tokio::test]
    async fn stop_timer_parses_written_segment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/stop"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "stopped": true, "activity_id": 9, "segment_id": 41, "minutes": 25
            })))
            .expect(1)
            .mount(&server)
            .await;

        let stopped = client(&server).stop_timer().await.unwrap();
        assert_eq!(stopped.segment_id, 41);
        assert_eq!(stopped.minutes, 25);
    }
}
