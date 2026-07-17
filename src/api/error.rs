use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The stable conflict-code vocabulary (engineer#806, ADR 0036). Codes are
/// contract: the server renames none of these, so matching on them is safe
/// where matching on `title`/`detail` prose would be brittle. Recorded in
/// full even though only the timer codes have consumers today — the target
/// and idempotency codes are parsed and awaiting their adoption (the
/// progress epic), hence the dead-code allowance.
#[allow(dead_code)]
pub mod codes {
    /// 409 on a replayed start: a timer is already running server-side. Comes
    /// with `current` (the running session snapshot) and `resolutions`.
    pub const TIMER_ALREADY_RUNNING: &str = "timer-already-running";
    /// 404 on a replayed pause/resume/stop: the session vanished server-side
    /// (distinguished from a routing 404). No extensions.
    pub const NO_LIVE_TIMER: &str = "no-live-timer";
    /// 422 on a write against a closed target version; `live_target_id` points
    /// at the lineage's live row while one exists.
    pub const TARGET_VERSION_CLOSED: &str = "target-version-closed";
    /// 409 while the first execution of the same `Idempotency-Key` is still
    /// running; `locked_at` says since when.
    pub const REQUEST_IN_FLIGHT: &str = "request-in-flight";
    /// 422 when an `Idempotency-Key` is reused with a different request.
    pub const IDEMPOTENCY_KEY_REUSE: &str = "idempotency-key-reuse";
}

/// RFC 7807 problem+json with the Engineer-specific `errors[]` extension for
/// 422s, plus the coded-conflict members (ADR 0036): the stable `code` and the
/// typed extensions, which flatten into [`ConflictInfo`] since they all live at
/// the problem's top level on the wire. `code` is its own field — orthogonal to
/// the extensions, so a code with no extensions still parses to an empty
/// [`ConflictInfo`].
#[derive(Debug, Deserialize, Clone)]
struct Problem {
    #[serde(rename = "type")]
    type_uri: Option<String>,
    title: Option<String>,
    status: Option<u16>,
    detail: Option<String>,
    #[serde(default)]
    errors: Vec<FieldError>,
    /// The stable conflict code (see [`codes`]), when the server sent one.
    #[serde(default)]
    code: Option<String>,
    #[serde(flatten)]
    conflict: ConflictInfo,
}

// `Serialize` lets the offline write queue persist a rejection verbatim on a
// diverged intent (`crate::queue`), the way the read cache serializes `Timer`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FieldError {
    pub field: String,
    pub detail: String,
}

/// A coded conflict's RFC 7807 §3.2 extension members (ADR 0036) — the server
/// state a client resolves with, no second read. The `code` itself is not
/// here: it rides alongside as its own field (on `ApiError::Problem` /
/// `IntentState::Diverged`), so a code carrying no extensions still captures as
/// empty (see [`is_empty`](ConflictInfo::is_empty)). Every field is
/// optional/defaulted so code-less problems and unknown codes parse to an empty
/// capture, never an error. `Serialize` for the same reason as [`FieldError`]:
/// a diverged intent persists its conflict verbatim.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ConflictInfo {
    /// `timer-already-running`: the running server session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<ConflictTimer>,
    /// `timer-already-running`: the server's resolution hints, e.g.
    /// `["switch", "keep-remote"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolutions: Vec<String>,
    /// `target-version-closed`: the lineage's live row, while one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_target_id: Option<i64>,
    /// `request-in-flight`: when the first execution took its lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_at: Option<jiff::Timestamp>,
}

impl ConflictInfo {
    /// True when no extension member is present — the empty capture a code-less
    /// problem, or a code that carries no extensions, parses to.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The `current` snapshot inside `timer-already-running` — a lean shape, not
/// `api::Timer`: the conflict carries exactly five members (ADR 0036) and no
/// `running` flag, so reusing the full read struct would misparse. All
/// optional: the panel renders what arrived, never refuses the whole problem
/// over one missing member.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ConflictTimer {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub activity_id: Option<i64>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Error, Clone)]
pub enum ApiError {
    #[error("not authenticated — run `engineer login`")]
    Unauthorized,
    #[error("{title} ({status}): {detail}")]
    Problem {
        status: u16,
        title: String,
        detail: String,
        type_uri: Option<String>,
        errors: Vec<FieldError>,
        /// The stable conflict code (see [`codes`]), when the server sent one.
        /// `None` on legacy problems and non-conflict errors — every consumer
        /// must keep working without it.
        code: Option<String>,
        /// The coded conflict's extension members; empty when there are none.
        /// Boxed so a coded problem's ~200-byte payload doesn't bloat every
        /// `Result<_, ApiError>` on the hot read/write paths (clippy's
        /// `result_large_err`); the box is on the cold error path only.
        conflict: Box<ConflictInfo>,
    },
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
}

impl ApiError {
    pub fn from_response(status: StatusCode, body: &[u8]) -> Self {
        if status == StatusCode::UNAUTHORIZED {
            return Self::Unauthorized;
        }
        match serde_json::from_slice::<Problem>(body) {
            Ok(p) => Self::Problem {
                status: p.status.unwrap_or(status.as_u16()),
                title: p
                    .title
                    .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").into()),
                detail: p.detail.unwrap_or_default(),
                type_uri: p.type_uri,
                errors: p.errors,
                code: p.code,
                conflict: Box::new(p.conflict),
            },
            Err(_) => Self::Problem {
                status: status.as_u16(),
                title: status.canonical_reason().unwrap_or("error").into(),
                detail: String::from_utf8_lossy(body).chars().take(200).collect(),
                type_uri: None,
                errors: vec![],
                code: None,
                conflict: Box::default(),
            },
        }
    }

    pub fn field_errors(&self) -> &[FieldError] {
        match self {
            Self::Problem { errors, .. } => errors,
            _ => &[],
        }
    }

    /// The stable conflict code, when this is a coded problem. The queue path
    /// reads the code from the persisted `IntentState::Diverged` instead; this
    /// accessor is for callers that hold the live `ApiError` — the Progress
    /// target writes route a `target-version-closed` (Self::live_target_id) here,
    /// and the replay re-addresses a diverged adjust by it.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Problem { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// The live lineage row a `target-version-closed` conflict points at (ADR
    /// 0026) — the id a replayed adjust re-addresses to, so the gesture (this
    /// many hours on this lineage) still lands rather than diverging. `None` on
    /// every other error, and on a closed version whose lineage is fully retired
    /// (no live row left — a genuine divergence).
    pub fn live_target_id(&self) -> Option<i64> {
        match self {
            Self::Problem { conflict, .. } => conflict.live_target_id,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc7807_validation_error() {
        let body = br#"{
            "type":"https://engineer.example/problems/validation",
            "title":"Validation failed",
            "status":422,
            "detail":"Title can't be blank",
            "errors":[{"field":"title","detail":"can't be blank"}]
        }"#;
        let err = ApiError::from_response(StatusCode::UNPROCESSABLE_ENTITY, body);
        match err {
            ApiError::Problem { status, errors, .. } => {
                assert_eq!(status, 422);
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].field, "title");
            }
            _ => panic!("expected Problem"),
        }
    }

    #[test]
    fn maps_401_to_unauthorized() {
        let err = ApiError::from_response(StatusCode::UNAUTHORIZED, b"{}");
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn handles_non_json_error_body() {
        let err = ApiError::from_response(StatusCode::BAD_GATEWAY, b"<html>nginx</html>");
        match err {
            ApiError::Problem { status, code, .. } => {
                assert_eq!(status, 502);
                assert!(code.is_none());
            }
            _ => panic!("expected Problem"),
        }
    }

    // --- the coded-conflict vocabulary (engineer#806, ADR 0036) -------------
    // Fixtures mirror the shipped openapi.yaml examples byte for byte where it
    // shows one.

    #[test]
    fn parses_timer_already_running_with_current_and_resolutions() {
        let body = br#"{
            "type": "https://engineer.example/problems/timer-already-running",
            "title": "Timer already running",
            "status": 409,
            "detail": "Stop the running timer first, or pass switch=true to stop-and-switch.",
            "code": "timer-already-running",
            "current": {
                "id": 114,
                "activity_id": 258777238,
                "label": "Ruby OOP Study",
                "started_at": "2026-07-16T08:59:03.246Z",
                "paused": false
            },
            "resolutions": ["switch", "keep-remote"]
        }"#;
        let err = ApiError::from_response(StatusCode::CONFLICT, body);
        let ApiError::Problem { code, conflict, .. } = err else {
            panic!("expected Problem");
        };
        assert_eq!(code.as_deref(), Some(codes::TIMER_ALREADY_RUNNING));
        let current = conflict.current.expect("the running session rides along");
        assert_eq!(current.id, Some(114));
        assert_eq!(current.activity_id, Some(258777238));
        assert_eq!(current.label.as_deref(), Some("Ruby OOP Study"));
        assert_eq!(
            current.started_at,
            Some("2026-07-16T08:59:03.246Z".parse().unwrap())
        );
        assert!(!current.paused);
        assert_eq!(conflict.resolutions, vec!["switch", "keep-remote"]);
    }

    #[test]
    fn parses_no_live_timer_code_only() {
        let body = br#"{
            "type": "https://engineer.example/problems/no-live-timer",
            "title": "No running timer",
            "status": 404,
            "detail": "There is no live timer to act on.",
            "code": "no-live-timer"
        }"#;
        let err = ApiError::from_response(StatusCode::NOT_FOUND, body);
        let ApiError::Problem { code, conflict, .. } = err else {
            panic!("expected Problem");
        };
        assert_eq!(code.as_deref(), Some(codes::NO_LIVE_TIMER));
        assert!(conflict.is_empty(), "no extensions on this code");
    }

    #[test]
    fn parses_target_version_closed_with_live_target_id() {
        let body = br#"{
            "type": "https://engineer.example/problems/target-version-closed",
            "title": "Target version is closed",
            "status": 422,
            "detail": "cannot adjust a closed target version. Fetch the live target for this axis and scope, then retry.",
            "code": "target-version-closed",
            "live_target_id": 47
        }"#;
        let err = ApiError::from_response(StatusCode::UNPROCESSABLE_ENTITY, body);
        let ApiError::Problem { code, conflict, .. } = err else {
            panic!("expected Problem");
        };
        assert_eq!(code.as_deref(), Some(codes::TARGET_VERSION_CLOSED));
        assert_eq!(conflict.live_target_id, Some(47));
        assert!(conflict.current.is_none());
    }

    #[test]
    fn parses_request_in_flight_with_locked_at() {
        let body = br#"{
            "type": "https://engineer.example/problems/request-in-flight",
            "title": "Request in flight",
            "status": 409,
            "detail": "The first attempt with this Idempotency-Key is still running.",
            "code": "request-in-flight",
            "locked_at": "2026-07-16T09:00:00Z"
        }"#;
        let err = ApiError::from_response(StatusCode::CONFLICT, body);
        assert_eq!(err.code(), Some(codes::REQUEST_IN_FLIGHT));
        let ApiError::Problem { conflict, .. } = err else {
            panic!("expected Problem");
        };
        assert_eq!(
            conflict.locked_at,
            Some("2026-07-16T09:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn parses_idempotency_key_reuse_code_only() {
        let body = br#"{
            "type": "https://engineer.example/problems/idempotency-key-reuse",
            "title": "Idempotency key reuse",
            "status": 422,
            "detail": "This Idempotency-Key was already used for a different request.",
            "code": "idempotency-key-reuse"
        }"#;
        let err = ApiError::from_response(StatusCode::UNPROCESSABLE_ENTITY, body);
        assert_eq!(err.code(), Some(codes::IDEMPOTENCY_KEY_REUSE));
    }

    #[test]
    fn unknown_codes_and_extensions_parse_harmlessly() {
        let body = br#"{
            "title": "Conflict",
            "status": 409,
            "detail": "something new",
            "code": "some-future-code",
            "some_future_member": {"nested": true}
        }"#;
        let err = ApiError::from_response(StatusCode::CONFLICT, body);
        let ApiError::Problem { code, conflict, .. } = err else {
            panic!("expected Problem");
        };
        assert_eq!(code.as_deref(), Some("some-future-code"));
        assert!(conflict.is_empty(), "unknown extensions are ignored");
    }

    #[test]
    fn a_codeless_problem_parses_exactly_as_before() {
        let body = br#"{
            "title": "Conflict",
            "status": 409,
            "detail": "a timer is already running"
        }"#;
        let err = ApiError::from_response(StatusCode::CONFLICT, body);
        let ApiError::Problem {
            title,
            detail,
            code,
            conflict,
            ..
        } = err
        else {
            panic!("expected Problem");
        };
        assert_eq!(title, "Conflict");
        assert_eq!(detail, "a timer is already running");
        assert!(code.is_none());
        assert!(conflict.is_empty());
    }

    #[test]
    fn conflict_info_roundtrips_and_serializes_lean() {
        let info = ConflictInfo {
            current: Some(ConflictTimer {
                id: Some(114),
                activity_id: Some(9),
                label: Some("systems".into()),
                started_at: Some("2026-07-16T08:59:03Z".parse().unwrap()),
                paused: true,
            }),
            resolutions: vec!["switch".into(), "keep-remote".into()],
            live_target_id: None,
            locked_at: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(
            !json.contains("live_target_id"),
            "absent members skipped: {json}"
        );
        let back: ConflictInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);

        let empty = serde_json::to_string(&ConflictInfo::default()).unwrap();
        assert_eq!(empty, "{}", "an empty capture persists as nothing");
    }
}
