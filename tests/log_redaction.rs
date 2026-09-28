// SPDX-License-Identifier: Apache-2.0
//! No log line carries a credential (the client-state record).
//!
//! The rolling log under the XDG state dir outlives the session and is the
//! file a user attaches to a bug report, so a bearer token, a refresh token or
//! a request's headers written there can act as the user. A logging call is
//! checked by what it names, which holds for calls not written yet.

use std::{fs, path::Path};

const LOG_MACROS: [&str; 5] = ["info!", "warn!", "debug!", "error!", "trace!"];
const CREDENTIALS: [&str; 6] = ["token", "header", "authoriz", "bearer", "secret", "refresh"];

fn source_files(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// Each logging call's argument text, from `info!(` to its closing `);`.
fn log_calls(source: &str) -> Vec<&str> {
    let mut calls = Vec::new();
    for mac in LOG_MACROS {
        let mut rest = source;
        while let Some(start) = rest.find(&format!("{mac}(")) {
            let args = &rest[start + mac.len()..];
            let end = args.find(");").map_or(args.len(), |e| e + 2);
            calls.push(&args[..end]);
            rest = &args[end..];
        }
    }
    calls
}

fn names_a_credential(call: &str) -> bool {
    let call = call.to_lowercase();
    CREDENTIALS.iter().any(|word| call.contains(word))
}

#[test]
fn no_log_call_names_a_credential() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    source_files(&root.join("src"), &mut files);
    let mut offences = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        for call in log_calls(&source) {
            if names_a_credential(call) {
                let rel = file.strip_prefix(root).unwrap().display();
                offences.push(format!(
                    "{rel}: {}",
                    call.split_whitespace().collect::<Vec<_>>().join(" ")
                ));
            }
        }
    }
    assert!(
        offences.is_empty(),
        "a log call names a credential:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_redaction_guard_notices_each_way_of_logging_one() {
    let logging = [
        r#"tracing::info!(target: "engineer_cli::api", headers = ?request.headers(), "request");"#,
        r#"tracing::warn!(%token, "refresh failed");"#,
        "debug!(\"auth {}\",\n    bearer);",
        r#"tracing::error!(refresh_token = %rt, "store failed");"#,
    ];
    for source in logging {
        assert!(
            log_calls(source).iter().any(|c| names_a_credential(c)),
            "missed: {source}"
        );
    }
    let clean =
        r#"tracing::info!(target: "engineer_cli::api", %method, %url, status = 200, "api call");"#;
    assert!(!log_calls(clean).iter().any(|c| names_a_credential(c)));
}
