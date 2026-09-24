//! Weekly time targets — declare / adjust / retire (`/api/v1/targets`).

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, Keyed, List, TargetRef};

// `Serialize`/`Deserialize`/`PartialEq` so a deferred declare persists verbatim
// on an `IntentKind::TargetCreate` — the queue re-sends exactly what the user
// made, never a re-derivation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TargetScope {
    /// A domain, addressed by its id.
    Domain(i64),
    /// An activity kind (the enum string, e.g. `coding`).
    Kind(String),
    /// An intent (the enum string).
    Intent(String),
}

impl TargetScope {
    fn axis(&self) -> &'static str {
        match self {
            TargetScope::Domain(_) => "domain",
            TargetScope::Kind(_) => "kind",
            TargetScope::Intent(_) => "intent",
        }
    }
}

// Same round-trip contract as [`TargetScope`]: an offline declare rides this
// whole body into the queue and re-sends it verbatim on replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetCreate {
    pub scope: TargetScope,
    pub hours_per_week: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum TargetState {
    /// Live rows, adjustable now (the server default).
    Active,
    /// The closing row of each retired lineage.
    Retired,
    /// Active + retired (intermediate superseded versions omitted).
    All,
}

impl TargetState {
    fn as_param(self) -> &'static str {
        match self {
            TargetState::Active => "active",
            TargetState::Retired => "retired",
            TargetState::All => "all",
        }
    }
}

#[derive(Serialize)]
struct CreateBody<'a> {
    target: CreateTarget<'a>,
}

#[derive(Serialize)]
struct CreateTarget<'a> {
    axis: &'static str,
    hours_per_week: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    intent: Option<&'a str>,
}

fn create_body(create: &TargetCreate) -> CreateBody<'_> {
    let (domain_id, kind, intent) = match &create.scope {
        TargetScope::Domain(id) => (Some(*id), None, None),
        TargetScope::Kind(k) => (None, Some(k.as_str()), None),
        TargetScope::Intent(i) => (None, None, Some(i.as_str())),
    };
    CreateBody {
        target: CreateTarget {
            axis: create.scope.axis(),
            hours_per_week: create.hours_per_week,
            domain_id,
            kind,
            intent,
        },
    }
}

#[derive(Serialize)]
struct AdjustBody {
    target: HoursOnly,
}

#[derive(Serialize)]
struct HoursOnly {
    hours_per_week: f64,
}

impl ApiClient {
    pub async fn list_targets(&self, state: TargetState) -> Result<List<TargetRef>, ApiError> {
        self.get(
            "/api/v1/targets",
            &[("state", state.as_param().to_string())],
        )
        .await
    }

    pub async fn create_target(&self, create: &TargetCreate) -> Result<TargetRef, ApiError> {
        self.post("/api/v1/targets", &create_body(create)).await
    }

    /// The queue's replay path, keyed so a lost ack can never mint the target
    /// twice. A server outside the engineer ADR 0036 opt-in set ignores the
    /// header, and a replay re-sends the identical body under the identical key,
    /// so keyed strictly dominates a plain re-send.
    pub(crate) async fn create_target_idempotent(
        &self,
        create: &TargetCreate,
        idempotency_key: &str,
    ) -> Result<Keyed<TargetRef>, ApiError> {
        self.post_idempotent("/api/v1/targets", &create_body(create), idempotency_key)
            .await
    }

    /// Returns the live row, whose id may differ from `id` when the edit minted a
    /// successor version — treat it as authoritative.
    pub async fn update_target(&self, id: i64, hours_per_week: f64) -> Result<TargetRef, ApiError> {
        let body = AdjustBody {
            target: HoursOnly { hours_per_week },
        };
        self.patch(&format!("/api/v1/targets/{id}"), &body).await
    }

    /// Closes the lineage and keeps its history; there is deliberately no delete
    /// (engineer ADR 0026).
    pub async fn retire_target(&self, id: i64) -> Result<TargetRef, ApiError> {
        self.patch_empty(&format!("/api/v1/targets/{id}/retire"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn target_body(id: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "axis": "domain",
            "scope": {
                "axis": "domain",
                "value": 7,
                "domain": { "id": 7, "name": "Distributed Systems" }
            },
            "hours_per_week": 6.0,
            "active": true,
            "retired": false,
            "active_from": "2026-06-29",
            "active_until": null,
            "retired_at": null,
            "created_at": "2026-06-29T09:00:00Z",
            "updated_at": "2026-06-29T09:00:00Z"
        })
    }

    #[tokio::test]
    async fn create_posts_domain_scope_body_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/targets"))
            .and(body_partial_json(serde_json::json!({
                "target": { "axis": "domain", "hours_per_week": 6.0, "domain_id": 7 }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(target_body(42)))
            .expect(1)
            .mount(&server)
            .await;

        let created = client(&server)
            .create_target(&TargetCreate {
                scope: TargetScope::Domain(7),
                hours_per_week: 6.0,
            })
            .await
            .unwrap();

        assert_eq!(created.id, 42);
        assert_eq!(created.scope.name(), "Distributed Systems");
        assert!((created.hours_per_week - 6.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn create_kind_scope_sends_kind_not_domain() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/targets"))
            .and(body_partial_json(serde_json::json!({
                "target": { "axis": "kind", "kind": "coding" }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 51, "axis": "kind",
                "scope": { "axis": "kind", "value": "coding" },
                "hours_per_week": 4.0, "active": true, "retired": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let created = client(&server)
            .create_target(&TargetCreate {
                scope: TargetScope::Kind("coding".into()),
                hours_per_week: 4.0,
            })
            .await
            .unwrap();
        assert_eq!(created.scope.name(), "coding");
    }

    #[tokio::test]
    async fn update_patches_hours_and_returns_live_row() {
        let server = MockServer::start().await;
        // The addressed id (42) may return a successor with a new id (99).
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .and(body_partial_json(serde_json::json!({
                "target": { "hours_per_week": 8.0 }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut b = target_body(99);
                b["hours_per_week"] = serde_json::json!(8.0);
                b
            }))
            .expect(1)
            .mount(&server)
            .await;

        let live = client(&server).update_target(42, 8.0).await.unwrap();
        assert_eq!(live.id, 99, "adjust returns the live row, id may change");
        assert!((live.hours_per_week - 8.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn retire_patches_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42/retire"))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut b = target_body(42);
                b["active"] = serde_json::json!(false);
                b["retired"] = serde_json::json!(true);
                b
            }))
            .expect(1)
            .mount(&server)
            .await;

        let retired = client(&server).retire_target(42).await.unwrap();
        assert!(retired.retired);
        assert!(!retired.active);
    }

    #[tokio::test]
    async fn list_sends_state_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/targets"))
            .and(query_param("state", "all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [ target_body(42) ],
                "meta": { "page": 1, "per_page": 25, "total": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let list = client(&server)
            .list_targets(TargetState::All)
            .await
            .unwrap();
        assert_eq!(list.data.len(), 1);
        assert_eq!(list.data[0].id, 42);
    }

    #[tokio::test]
    async fn closed_version_maps_to_unprocessable_error() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/targets/42"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "type": "https://engineer.example/problems/unprocessable",
                "title": "Target version is closed",
                "status": 422
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = client(&server).update_target(42, 8.0).await.unwrap_err();
        // A closed/stale version id surfaces as an RFC 7807 problem (422), not a panic.
        assert!(matches!(err, ApiError::Problem { status: 422, .. }));
    }
}
