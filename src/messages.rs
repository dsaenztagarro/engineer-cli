//! The one-spelling catalogue (design-system.dc.html §ERROR & NOTIFICATION
//! MODEL, §C). Every outcome the client can report is spelled here **once**, so
//! the Tier-1 notify tile, the Tier-2 inline panel line, and the headless
//! `stderr` a script greps all read the same words. The screens
//! (`crate::app`/`crate::ui`) and the headless verbs (`crate::*_cli`) both call
//! these — a single binary crate, so the module is reachable from either side.
//!
//! Keep the wording here, not at the call site: a reason token spelled twice
//! drifts, and drift is exactly the divergence §C forbids.

use crate::api::ApiError;

/// The refusal a read/write prints when there is no session. One spelling
/// wherever auth is missing — the notify tile, the CLI stderr, the login hint.
/// Adopted by the Tier-3 / write-refusal tickets; recorded here so the whole
/// catalogue lives in one place (cf. `api::error::codes`).
#[allow(dead_code)]
pub fn not_authenticated() -> &'static str {
    "not authenticated — run `engineer login`"
}

/// A live-only write refused because the network is down. `verb` names the
/// gesture ("connecting", "a rating", "triage") so the line reads naturally.
/// Matches the `OFFLINE_REFUSAL` copy the connect/inbox/notes/review screens
/// spelled by hand before this catalogue existed. Adopted by the flat-list /
/// write-refusal tickets.
#[allow(dead_code)]
pub fn offline(verb: &str) -> String {
    format!("offline — {verb} needs the server; retry online")
}

/// The Tier-2 panel headline for a read that failed — the loud red line inside
/// the bordered block ("couldn't load books"). No glyph: the panel renderer
/// owns the `✖`.
pub fn load_failed(noun: &str) -> String {
    format!("couldn't load {noun}")
}

/// The Tier-1 tile / headless-stderr form of the same failure, carrying the
/// cause after the colon ("books load failed: {reason}"). Pair `reason` with
/// [`fail_reason`] so the tile and the panel's second line agree word for word.
/// Kept in the catalogue for the headless verbs and any read that still tiles a
/// failure; the Tier-2 read panels surface theirs inline instead (§C), so no
/// screen currently calls it.
#[allow(dead_code)]
pub fn tile_load_failed(noun: &str, reason: &str) -> String {
    format!("{noun} load failed: {reason}")
}

/// The canonical cause line for an [`ApiError`] — the Tier-2 panel's muted
/// second line, and the `{reason}` half of [`tile_load_failed`]. `host` is the
/// identity host (`ApiClient::host`) so a server error names where it came from,
/// the way the mock shows `identity.dsaenz.dev → HTTP 500`.
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
