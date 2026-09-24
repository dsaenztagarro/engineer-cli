//! The one-spelling catalogue: every outcome the client reports is worded here once (ADR 0001).

use crate::api::ApiError;

pub fn not_authenticated() -> &'static str {
    "not authenticated — run `engineer login`"
}

pub fn offline(verb: &str) -> String {
    format!("offline — {verb} needs the server; retry online")
}

/// No glyph: the panel renderer owns the `✖`.
pub fn load_failed(noun: &str) -> String {
    format!("couldn't load {noun}")
}

/// Pair `reason` with [`fail_reason`] so the tile and the panel's second line
/// agree word for word.
pub fn tile_load_failed(noun: &str, reason: &str) -> String {
    format!("{noun} load failed: {reason}")
}

/// `host` is the identity host (`ApiClient::host`).
pub fn fail_reason(host: &str, e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized => not_authenticated().to_string(),
        ApiError::Transport(_) => format!("offline — can't reach {host}"),
        ApiError::Problem { status, .. } => format!("{host} → HTTP {status}"),
        ApiError::Decode(_) => format!("{host} → unreadable response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_outcome_has_one_spelling_across_the_error_type_and_the_catalogue() {
        assert_eq!(ApiError::Unauthorized.to_string(), not_authenticated());
    }

    #[test]
    fn auth_and_offline_copy_is_stable() {
        assert_eq!(
            not_authenticated(),
            "not authenticated — run `engineer login`"
        );
        assert_eq!(
            offline("connecting"),
            "offline — connecting needs the server; retry online"
        );
    }

    #[test]
    fn load_failure_headline_and_tile_agree_on_the_noun() {
        assert_eq!(load_failed("books"), "couldn't load books");
        assert_eq!(
            tile_load_failed("books", "identity.dev → HTTP 500"),
            "books load failed: identity.dev → HTTP 500"
        );
    }

    #[test]
    fn fail_reason_names_the_host_and_status() {
        let host = "identity.dsaenz.dev";
        assert_eq!(
            fail_reason(host, &ApiError::Transport("dns".into())),
            "offline — can't reach identity.dsaenz.dev"
        );
        assert_eq!(
            fail_reason(
                host,
                &ApiError::Problem {
                    status: 500,
                    title: "Server Error".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Box::default(),
                }
            ),
            "identity.dsaenz.dev → HTTP 500"
        );
        assert_eq!(
            fail_reason(host, &ApiError::Unauthorized),
            "not authenticated — run `engineer login`"
        );
    }
}
