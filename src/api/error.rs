use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;

/// RFC 7807 problem+json with the Engineer-specific `errors[]` extension for 422s.
#[derive(Debug, Deserialize, Clone)]
struct Problem {
    #[serde(rename = "type")]
    type_uri: Option<String>,
    title: Option<String>,
    status: Option<u16>,
    detail: Option<String>,
    #[serde(default)]
    errors: Vec<FieldError>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FieldError {
    pub field: String,
    pub detail: String,
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
                title: p.title.unwrap_or_else(|| status.canonical_reason().unwrap_or("error").into()),
                detail: p.detail.unwrap_or_default(),
                type_uri: p.type_uri,
                errors: p.errors,
            },
            Err(_) => Self::Problem {
                status: status.as_u16(),
                title: status.canonical_reason().unwrap_or("error").into(),
                detail: String::from_utf8_lossy(body).chars().take(200).collect(),
                type_uri: None,
                errors: vec![],
            },
        }
    }

    pub fn field_errors(&self) -> &[FieldError] {
        match self {
            Self::Problem { errors, .. } => errors,
            _ => &[],
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
            ApiError::Problem { status, .. } => assert_eq!(status, 502),
            _ => panic!("expected Problem"),
        }
    }
}
