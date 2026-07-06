//! `GET /api/v1/progress` — the weekly pace aggregate (progress.html §F).
//!
//! One derived object the web dashboard, this CLI's `engineer pace` meters, and
//! the MCP weekly-review all read: the ISO study week, one reading per active
//! target (behind-first / largest-gap-first), plus kind-mix, Bloom, and totals
//! roll-ups. Nothing here is stored server-side — it is recomputed from segments
//! at read time, so a single object (not a paginated `List`) comes back.

use serde::Deserialize;

use super::{ApiClient, ApiError};

/// Top-level payload of `GET /api/v1/progress`.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub week: Week,
    /// Readings arrive behind-first, largest gap first — render in wire order.
    #[serde(default)]
    pub targets: Vec<ProgressReading>,
    #[serde(default)]
    pub kind_mix: Vec<KindMix>,
    // Bloom is parsed for parity with the web/MCP payload but the terminal screen
    // renders only the pace meters and kind-mix (a bar chart doesn't reduce to a
    // single scannable line); see the PR notes.
    #[allow(dead_code)]
    #[serde(default)]
    pub bloom: Vec<BloomLevel>,
    pub totals: Totals,
    /// Exactly 7 entries Monday→Sunday when the server serves it (0-minute
    /// days included); empty on older payloads — the rail degrades to the
    /// today-only block.
    #[serde(default)]
    pub by_day: Vec<DayMinutes>,
}

/// One day of the week's logged minutes, bucketed by completed segments on
/// the 4 AM day boundary.
#[derive(Debug, Clone, Deserialize)]
pub struct DayMinutes {
    pub date: jiff::civil::Date,
    pub minutes: u32,
}

/// The ISO study week frame. `now_fraction` (0.0..=1.0) is where the gray
/// now-tick sits: `elapsed_days / 7`, or 1.0 for any closed week.
#[derive(Debug, Clone, Deserialize)]
pub struct Week {
    pub id: String,
    pub monday: jiff::civil::Date,
    #[allow(dead_code)]
    pub sunday: jiff::civil::Date,
    pub elapsed_days: u32,
    pub now_fraction: f64,
}

/// One target's reading for the week: the target itself plus derived actuals.
// `hours_per_week` is duplicated at this level and on `target`; both mirror the
// wire format. Minute fields are `i64` because `delta_minutes` goes negative.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ProgressReading {
    pub target: TargetRef,
    pub hours_per_week: f64,
    pub actual_minutes: i64,
    pub expected_minutes: i64,
    pub delta_minutes: i64,
    pub state: PaceState,
}

impl ProgressReading {
    pub fn actual_hours(&self) -> f64 {
        self.actual_minutes as f64 / 60.0
    }

    pub fn delta_hours(&self) -> f64 {
        self.delta_minutes as f64 / 60.0
    }

    /// Fraction of the target reached (0.0..=1.0), for the meter fill.
    pub fn progress_fraction(&self) -> f64 {
        let target_minutes = self.hours_per_week * 60.0;
        if target_minutes <= 0.0 {
            return 0.0;
        }
        (self.actual_minutes as f64 / target_minutes).clamp(0.0, 1.0)
    }
}

/// The three pace states (progress.html §A.1). There is deliberately no red
/// state — `behind` is as loud as pace gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceState {
    Met,
    Behind,
    OnPace,
}

impl PaceState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Behind => "behind",
            Self::OnPace => "on pace",
        }
    }
}

/// A weekly time Target version row. `id` is a version id (adjusting hours mints
/// a successor), so a lineage is addressed by axis + scope, not by id.
// Only `scope` and `hours_per_week` drive the meter today; the rest mirror the
// wire format for parity with the web/MCP consumers.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct TargetRef {
    pub id: i64,
    pub axis: String,
    pub scope: Scope,
    pub hours_per_week: f64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub retired: bool,
}

/// The slice of the log a target measures. For a domain target `value` is the
/// domain id and `domain` is inlined; for kind/intent targets `value` is the
/// enum string and `domain` is absent.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Scope {
    pub axis: String,
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub domain: Option<ScopeDomain>,
}

impl Scope {
    /// Human label for the meter: the domain name for domain targets, else the
    /// kind/intent enum string. Falls back to the axis when nothing resolves.
    pub fn name(&self) -> String {
        if let Some(name) = self.domain.as_ref().and_then(|d| d.name.as_deref()) {
            return name.to_string();
        }
        match &self.value {
            serde_json::Value::String(s) => s.clone(),
            v if !v.is_null() => v.to_string(),
            _ => self.axis.clone(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeDomain {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
}

/// A kind's logged minutes for the week (only kinds with time, largest first).
#[derive(Debug, Clone, Deserialize)]
pub struct KindMix {
    pub kind: String,
    pub minutes: i64,
}

/// A Bloom level's activity count for the week (all six levels, 0 where unlogged).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct BloomLevel {
    pub level: String,
    pub count: i64,
}

/// Week-wide aggregates. `thin` flags a week too sparse to read a trend (< 3).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Totals {
    pub actual_minutes: i64,
    pub activity_count: i64,
    #[serde(default)]
    pub thin: bool,
}

impl ApiClient {
    /// Fetch the pace aggregate. `week` is an ISO week id (`YYYY-Www`); `None`
    /// asks the server for the current week.
    pub async fn get_progress(&self, week: Option<&str>) -> Result<Progress, ApiError> {
        let mut query: Vec<(&str, String)> = vec![];
        if let Some(week) = week {
            query.push(("week", week.to_string()));
        }
        self.get("/api/v1/progress", &query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn sample_body() -> serde_json::Value {
        serde_json::json!({
            "week": {
                "id": "2026-W27",
                "monday": "2026-06-29",
                "sunday": "2026-07-05",
                "elapsed_days": 4,
                "now_fraction": 0.5714
            },
            "targets": [
                {
                    "target": {
                        "id": 42,
                        "axis": "domain",
                        "scope": {
                            "axis": "domain",
                            "value": 7,
                            "domain": { "id": 7, "name": "Distributed Systems" }
                        },
                        "hours_per_week": 10.0,
                        "active": true,
                        "retired": false
                    },
                    "hours_per_week": 10.0,
                    "actual_minutes": 180,
                    "expected_minutes": 343,
                    "delta_minutes": -163,
                    "state": "behind"
                },
                {
                    "target": {
                        "id": 51,
                        "axis": "kind",
                        "scope": { "axis": "kind", "value": "coding" },
                        "hours_per_week": 4.0,
                        "active": true,
                        "retired": false
                    },
                    "hours_per_week": 4.0,
                    "actual_minutes": 240,
                    "expected_minutes": 137,
                    "delta_minutes": 103,
                    "state": "met"
                }
            ],
            "kind_mix": [ { "kind": "coding", "minutes": 180 } ],
            "bloom": [ { "level": "remember", "count": 0 } ],
            "totals": { "actual_minutes": 275, "activity_count": 4, "thin": false }
        })
    }

    #[tokio::test]
    async fn current_week_omits_week_param_and_deserializes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress"))
            .and(query_param_is_missing("week"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_body()))
            .expect(1)
            .mount(&server)
            .await;

        let progress = client(&server).get_progress(None).await.unwrap();

        assert_eq!(progress.week.id, "2026-W27");
        assert_eq!(progress.week.elapsed_days, 4);
        assert_eq!(progress.targets.len(), 2);

        let first = &progress.targets[0];
        // Domain scope resolves its name from the inlined domain object.
        assert_eq!(first.target.scope.name(), "Distributed Systems");
        assert_eq!(first.state, PaceState::Behind);
        assert_eq!(first.actual_minutes, 180);
        assert_eq!(first.delta_minutes, -163);

        // Kind scope resolves its name from the enum string `value`.
        assert_eq!(progress.targets[1].target.scope.name(), "coding");
        assert_eq!(progress.targets[1].state, PaceState::Met);

        assert_eq!(progress.kind_mix[0].kind, "coding");
        assert_eq!(progress.totals.activity_count, 4);
        assert!(!progress.totals.thin);
    }

    #[tokio::test]
    async fn explicit_week_sends_week_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress"))
            .and(query_param("week", "2026-W26"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_body()))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .get_progress(Some("2026-W26"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn by_day_decodes_and_defaults_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "week": { "id": "2026-W28", "monday": "2026-07-06", "sunday": "2026-07-12",
                          "elapsed_days": 1, "now_fraction": 0.14 },
                "targets": [], "kind_mix": [], "bloom": [],
                "totals": { "actual_minutes": 227, "activity_count": 4 },
                "by_day": [
                    { "date": "2026-07-06", "minutes": 227 },
                    { "date": "2026-07-07", "minutes": 0 }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let progress = client(&server).get_progress(None).await.unwrap();
        assert_eq!(progress.by_day.len(), 2);
        assert_eq!(progress.by_day[0].minutes, 227);
        assert_eq!(progress.by_day[0].date, jiff::civil::date(2026, 7, 6));
    }

    #[tokio::test]
    async fn unauthorized_maps_to_unauthorized_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/progress"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/unauthorized",
                "title": "Unauthorized",
                "status": 401
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = client(&server).get_progress(None).await.unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn progress_fraction_clamps_and_derives_hours() {
        let body = sample_body();
        let progress: Progress = serde_json::from_value(body).unwrap();
        // 180 min / 10h target = 0.30.
        assert!((progress.targets[0].progress_fraction() - 0.30).abs() < 1e-6);
        // 240 min / 4h target = 1.0 (met, clamped).
        assert!((progress.targets[1].progress_fraction() - 1.0).abs() < 1e-6);
        assert!((progress.targets[0].actual_hours() - 3.0).abs() < 1e-6);
    }
}
