//! The palette is generated from the design system's token set, never written by hand.
//!
//! `design/tokens.toml` is mirrored from engineer-cli-ds, where every colour is
//! checked for quantisation collisions and contrast. These tests render
//! `src/ui/tokens.rs` from it and fail when the committed file disagrees, so a
//! hand-edited colour cannot reach a release. `theme.rs` was once adapted by hand
//! from a token file that had since moved, and two of its colours failed floors
//! nobody measured.
//!
//! `UPDATE_TOKENS=1 cargo test --test tokens` rewrites the generated module.

use std::{env, fs, path::Path};

const TOKENS: &str = "design/tokens.toml";
const GENERATED: &str = "src/ui/tokens.rs";

struct Decision {
    name: String,
    rgb: String,
    indexed: i64,
    references: String,
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn decisions() -> Vec<Decision> {
    let source = fs::read_to_string(root().join(TOKENS)).expect("design/tokens.toml is readable");
    let set: toml::Table = source.parse().expect("design/tokens.toml parses");
    set["token"]
        .as_array()
        .expect("the token set has tokens")
        .iter()
        .filter(|t| t["tier"].as_str() == Some("decision"))
        .map(|t| Decision {
            name: t["name"].as_str().unwrap().to_string(),
            rgb: t["rgb"].as_str().unwrap().to_string(),
            indexed: t["indexed"].as_integer().unwrap(),
            references: t["references"].as_str().unwrap().to_string(),
        })
        .collect()
}

fn render(decisions: &[Decision]) -> String {
    let mut out = String::from(
        "// Generated from design/tokens.toml by tests/tokens.rs. Do not edit:\n\
         // `UPDATE_TOKENS=1 cargo test --test tokens` regenerates it, and CI fails when it drifts.\n\
         //\n\
         // One xterm-256 index per decision token. Options are not emitted, because a\n\
         // surface names where a colour applies, never the hue itself.\n\
         \n\
         // Every decision is emitted, including the terminal's own foreground and ground,\n\
         // which nothing paints.\n\
         #![allow(dead_code)]\n\
         \n",
    );
    for d in decisions {
        out.push_str(&format!(
            "pub const {}: u8 = {}; // {}, {}\n",
            d.name.to_uppercase().replace('-', "_"),
            d.indexed,
            d.rgb,
            d.references
        ));
    }
    out
}

#[test]
fn the_committed_palette_is_what_the_token_set_generates() {
    let expected = render(&decisions());
    let path = root().join(GENERATED);
    if env::var_os("UPDATE_TOKENS").is_some() {
        fs::write(&path, &expected).expect("src/ui/tokens.rs is writable");
        return;
    }
    let actual = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "{GENERATED} disagrees with {TOKENS}: regenerate it with \
         `UPDATE_TOKENS=1 cargo test --test tokens` rather than editing it"
    );
}

#[test]
fn every_decision_lands_on_an_index_no_terminal_theme_redefines() {
    let decisions = decisions();
    assert!(
        !decisions.is_empty(),
        "no decisions, so nothing below is checked"
    );
    for d in decisions {
        assert!(
            (16..=255).contains(&d.indexed),
            "{} resolves to index {}, one of the ANSI slots a terminal theme redefines",
            d.name,
            d.indexed
        );
    }
}
