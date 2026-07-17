//! Weekly time Targets — declare / adjust / retire (`/api/v1/targets`).
//!
//! A target is one axis (domain | kind | intent) + a scope on that axis + hours
//! per week; actuals and pace are never stored here — `GET /api/v1/progress`
//! derives them on read (see [`super::progress`]).
//!
//! Rows are append-only VERSIONS of a lineage (engineer ADR 0026):
//! - `update` adjusts the hours and returns the LIVE row, whose `id` may differ
//!   from the one addressed (an edit past the same day mints a successor). A
//!   stale/closed version id is a `422` — so callers address a lineage by its
//!   axis + scope, re-reading after an adjust rather than trusting a cached id.
//! - there is deliberately NO delete: `retire` closes the lineage while keeping
//!   its history, so past weeks still read it.
//!
//! The response is the bare target object (a superset of [`TargetRef`], which the
//! progress read already reuses); the extra timestamp fields are ignored on decode.

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, Keyed, List, TargetRef};

/// The slice of the log a new target measures — the axis and its scope value.
// `Serialize`/`Deserialize`/`PartialEq` so a deferred declare persists verbatim
// on an `IntentKind::TargetCreate` in `queue.json` (the queue never re-derives a
// gesture — it re-sends exactly what the user made).
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

/// A target to declare: its scope plus the weekly hours.
// Same round-trip contract as [`TargetScope`]: an offline declare rides this
// whole body into the queue and re-sends it verbatim on replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetCreate {
    pub scope: TargetScope,
    pub hours_per_week: f64,
}

/// Which lifecycle slice `list_targets` asks for (one row per lineage).
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

// The server permits `target: { axis, hours_per_week, domain_id | kind | intent }`
// and slices to the scope column matching the axis, so we send exactly the one
// scope field for the chosen axis.
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

/// Build the create request body for a declare, sending exactly the one scope
/// field the chosen axis needs. Shared by the live and idempotent-replay paths.
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
    /// List targets in one lifecycle slice — one row per lineage.
    pub async fn list_targets(&self, state: TargetState) -> Result<List<TargetRef>, ApiError> {
        self.get(
            "/api/v1/targets",
            &[("state", state.as_param().to_string())],
        )
        .await
    }

    /// Declare a weekly target. Returns the created row.
    pub async fn create_target(&self, create: &TargetCreate) -> Result<TargetRef, ApiError> {
        self.post("/api/v1/targets", &create_body(create)).await
    }

    /// The `create_target` twin carrying an `Idempotency-Key` — the queue's
    /// replay path re-sends a deferred declare through this so a lost ack can
    /// never mint the target twice. Keyed (not plain) is the safe default: the
    /// server dedupes on the key where targets-create is in engineer#809's ADR
    /// 0036 opt-in set, and where it is not the header is simply ignored — keyed
    /// can only ever prevent a double-write, never cause one (a replay re-sends
    /// the identical body under the identical key, so a key-reuse conflict cannot
    /// arise), so it strictly dominates a plain re-send. The activity/timer
    /// creates replay under the same contract.
    pub(crate) async fn create_target_idempotent(
        &self,
        create: &TargetCreate,
        idempotency_key: &str,
    ) -> Result<Keyed<TargetRef>, ApiError> {
        self.post_idempotent("/api/v1/targets", &create_body(create), idempotency_key)
            .await
    }

    /// Adjust a target's weekly hours. Returns the LIVE row — its `id` may differ
    /// from `id` when the edit minted a successor version, so callers should treat
    /// the returned target as authoritative rather than re-using `id`.
    pub async fn update_target(&self, id: i64, hours_per_week: f64) -> Result<TargetRef, ApiError> {
        let body = AdjustBody {
            target: HoursOnly { hours_per_week },
        };
        self.patch(&format!("/api/v1/targets/{id}"), &body).await
    }

    /// Retire a target — closes the lineage (never deletes). Returns the closed row.
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
