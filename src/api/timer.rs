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
    /// `"stopwatch"` or `"focus"`. Absent on older payloads.
    #[serde(default)]
    pub mode: Option<String>,
    /// Focus only: `"work"` or `"break"`. Stopwatch timers carry no phase.
    #[serde(default)]
    pub phase: Option<String>,
    /// Focus only: work intervals banked so far this session.
    #[serde(default)]
    pub intervals_completed: Option<u32>,
    /// Server-side idle guard verdict: the clock has gone quiet and a reclaim
    /// decision is pending.
    #[serde(default)]
    pub idle: Option<bool>,
    /// The server's presence mark — reclaim verbs anchor to it (a reclaimed
    /// `stop` ends the segment here). The idle span is `now − this`.
    #[serde(default)]
    pub last_interacted_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub paused_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub paused_seconds: Option<i64>,
    /// Focus only: when the current work/break phase began.
    #[serde(default)]
    pub phase_started_at: Option<jiff::Timestamp>,
    /// Overrun contract — non-null only when the timer is bound, the activity
    /// has a plan, and the overrun ping is enabled.
    #[serde(default)]
    pub planned_minutes: Option<u32>,
    /// Minutes already logged on the bound activity before this session.
    #[serde(default)]
    pub logged_minutes: Option<u32>,
    /// True once `elapsed + logged_minutes` crosses `planned_minutes`.
    #[serde(default)]
    pub over: bool,
}

/// The per-user timer knobs from `GET /api/v1/timer/settings` — read-only in
/// the CLI (editing is web-only). The server always serves all twelve.
#[derive(Debug, Clone, Deserialize)]
pub struct TimerSettings {
    /// `"stopwatch"` or `"focus"`.
    pub timer_mode: String,
    pub focus_work_minutes: u32,
    pub focus_short_break_minutes: u32,
    pub focus_long_break_minutes: u32,
    /// Every Nth work interval earns the long break.
    pub focus_long_break_every: u32,
    pub idle_guard_enabled: bool,
    pub idle_threshold_minutes: u32,
    /// `"trim"` | `"keep"` | `"stop"` — the reclaim list's default selection.
    pub idle_default_reclaim: String,
    /// Flag segments longer than this many hours.
    pub audit_long_hours: u32,
    /// Flag segments shorter than this many seconds.
    pub audit_short_seconds: u32,
    pub audit_badge_enabled: bool,
    pub overrun_ping_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimerStopped {
    pub stopped: bool,
    pub activity_id: i64,
    pub segment_id: i64,
    pub minutes: u32,
}

/// The idle-tail reclaim verbs — one server verb per §Idle reclaim row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimVerb {
    /// Idle span becomes paused time; the timer keeps running.
    Trim,
    /// The tail counts as work; presence is re-marked, the timer keeps running.
    Keep,
    /// Save a segment ending at `last_interacted_at`; the timer ends.
    Stop,
}

impl ReclaimVerb {
    pub const NAMES: &'static [&'static str] = &["trim", "keep", "stop"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trim => "trim",
            Self::Keep => "keep",
            Self::Stop => "stop",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "trim" => Some(Self::Trim),
            "keep" => Some(Self::Keep),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

/// What a reclaim left behind: trim/keep return the still-running timer,
/// stop returns the written segment.
#[derive(Debug, Clone)]
pub enum Reclaimed {
    Running(Box<Timer>),
    Stopped(TimerStopped),
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

    /// The per-user timer knobs (view-only in the CLI; edit on the web).
    pub async fn timer_settings(&self) -> Result<TimerSettings, ApiError> {
        self.get("/api/v1/timer/settings", &[]).await
    }

    /// Mark presence — the CLI's honest "the user is working in the TUI" beat,
    /// the twin of the web pill's heartbeat. Keeps the idle guard from tripping
    /// on real in-TUI work; the caller throttles to at most once a minute.
    /// 404 when nothing is running.
    pub async fn heartbeat_timer(&self) -> Result<(), ApiError> {
        self.post_empty("/api/v1/timer/heartbeat").await
    }

    /// Drive a focus phase transition — transitions never fire on their own;
    /// the server validates and applies. `to` is `"work"` or `"break"`.
    /// 422 when the transition isn't available from the current phase.
    pub async fn timer_phase(&self, to: &str) -> Result<Timer, ApiError> {
        self.post("/api/v1/timer/phase", &serde_json::json!({ "to": to }))
            .await
    }

    /// Switch the running timer's mode in place — elapsed is preserved.
    /// Entering focus opens a work phase; leaving clears it. 422 when the
    /// timer is already in that mode.
    pub async fn timer_mode(&self, mode: &str) -> Result<Timer, ApiError> {
        self.post("/api/v1/timer/mode", &serde_json::json!({ "mode": mode }))
            .await
    }

    /// Apply an idle-tail reclaim decision. The response shape follows the
    /// verb: `trim`/`keep` return the running timer, `stop` the written
    /// segment. 422 on `stop` when the timer is unbound.
    pub async fn reclaim_timer(&self, verb: ReclaimVerb) -> Result<Reclaimed, ApiError> {
        let body = serde_json::json!({ "verb": verb.as_str() });
        match verb {
            ReclaimVerb::Stop => {
                let stopped: TimerStopped = self.post("/api/v1/timer/reclaim", &body).await?;
                Ok(Reclaimed::Stopped(stopped))
            }
            _ => {
                let timer: Timer = self.post("/api/v1/timer/reclaim", &body).await?;
                Ok(Reclaimed::Running(Box::new(timer)))
            }
        }
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
    async fn timer_read_decodes_focus_and_idle_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9,
                "elapsed_seconds": 1928,
                "mode": "focus", "phase": "work", "intervals_completed": 2,
                "idle": false, "last_interacted_at": "2026-07-05T13:22:58Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).timer().await.unwrap();
        assert_eq!(timer.mode.as_deref(), Some("focus"));
        assert_eq!(timer.phase.as_deref(), Some("work"));
        assert_eq!(timer.intervals_completed, Some(2));
        assert_eq!(timer.idle, Some(false));
        assert!(timer.last_interacted_at.is_some());
    }

    #[tokio::test]
    async fn timer_read_decodes_the_overrun_contract() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 9,
                "elapsed_seconds": 8320,
                "planned_minutes": 120, "logged_minutes": 18, "over": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).timer().await.unwrap();
        assert_eq!(timer.planned_minutes, Some(120));
        assert_eq!(timer.logged_minutes, Some(18));
        assert!(timer.over);
    }

    #[tokio::test]
    async fn phase_posts_the_target_and_returns_the_timer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/phase"))
            .and(body_json(serde_json::json!({ "to": "break" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "mode": "focus", "phase": "break",
                "intervals_completed": 3
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).timer_phase("break").await.unwrap();
        assert_eq!(timer.phase.as_deref(), Some("break"));
        assert_eq!(timer.intervals_completed, Some(3));
    }

    #[tokio::test]
    async fn mode_switch_posts_the_mode_in_place() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/mode"))
            .and(body_json(serde_json::json!({ "mode": "focus" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "mode": "focus", "phase": "work",
                "elapsed_seconds": 1928
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).timer_mode("focus").await.unwrap();
        assert_eq!(timer.mode.as_deref(), Some("focus"));
        assert_eq!(timer.elapsed_seconds, Some(1928), "elapsed preserved");
    }

    #[tokio::test]
    async fn reclaim_trim_posts_the_verb_and_keeps_running() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/reclaim"))
            .and(body_json(serde_json::json!({ "verb": "trim" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "paused_seconds": 5220, "idle": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .reclaim_timer(ReclaimVerb::Trim)
            .await
            .unwrap()
        {
            Reclaimed::Running(t) => {
                assert!(t.running);
                assert_eq!(t.paused_seconds, Some(5220));
            }
            other => panic!("expected a running timer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reclaim_stop_returns_the_written_segment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/reclaim"))
            .and(body_json(serde_json::json!({ "verb": "stop" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "stopped": true, "activity_id": 9, "segment_id": 41, "minutes": 74
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .reclaim_timer(ReclaimVerb::Stop)
            .await
            .unwrap()
        {
            Reclaimed::Stopped(s) => assert_eq!(s.minutes, 74),
            other => panic!("expected a written segment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heartbeat_posts_to_the_presence_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/heartbeat"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).heartbeat_timer().await.unwrap();
    }

    #[tokio::test]
    async fn timer_settings_decodes_all_twelve_knobs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "timer_mode": "stopwatch",
                "focus_work_minutes": 50,
                "focus_short_break_minutes": 10,
                "focus_long_break_minutes": 20,
                "focus_long_break_every": 4,
                "idle_guard_enabled": true,
                "idle_threshold_minutes": 15,
                "idle_default_reclaim": "trim",
                "audit_long_hours": 6,
                "audit_short_seconds": 60,
                "audit_badge_enabled": true,
                "overrun_ping_enabled": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let settings = client(&server).timer_settings().await.unwrap();
        assert_eq!(settings.focus_work_minutes, 50);
        assert_eq!(settings.focus_long_break_every, 4);
        assert_eq!(settings.idle_default_reclaim, "trim");
        assert!(settings.overrun_ping_enabled);
    }

    #[tokio::test]
    async fn timer_read_tolerates_v1_payload_without_new_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let timer = client(&server).timer().await.unwrap();
        assert!(timer.mode.is_none());
        assert!(timer.phase.is_none());
        assert!(timer.idle.is_none());
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
