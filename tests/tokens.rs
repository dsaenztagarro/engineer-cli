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

// Indices 0-15 are the terminal's ANSI slots, which every theme redefines, so a
// named ANSI colour renders as whatever the user's theme says it is.
const ANSI_NAMES: &[&str] = &[
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "Gray",
    "DarkGray",
    "LightRed",
    "LightGreen",
    "LightYellow",
    "LightBlue",
    "LightMagenta",
    "LightCyan",
    "White",
];

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

fn spells_a_colour(line: &str) -> bool {
    let after_digit = |marker: &str| {
        line.match_indices(marker).any(|(i, _)| {
            line[i + marker.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    };
    let names_ansi = ANSI_NAMES.iter().any(|name| {
        let marker = format!("Color::{name}");
        line.match_indices(&marker).any(|(i, _)| {
            !line[i + marker.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        })
    });
    let literal_colour_const = line.contains("COLOR_") && after_digit("u8 = ");
    after_digit("Color::Indexed(")
        || line.contains("Color::Rgb(")
        || after_digit("38;5;")
        || names_ansi
        || literal_colour_const
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

#[test]
fn no_source_file_spells_a_colour_the_token_set_does_not_name() {
    let mut files = Vec::new();
    source_files(&root().join("src"), &mut files);
    let generated = root().join(GENERATED);
    let mut offences = Vec::new();
    for file in files.iter().filter(|f| **f != generated) {
        let source = fs::read_to_string(file).unwrap();
        for (n, line) in source.lines().enumerate() {
            if spells_a_colour(line) {
                let rel = file.strip_prefix(root()).unwrap().display();
                offences.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offences.is_empty(),
        "a colour is named through src/ui/tokens.rs, never spelled as a literal:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_colour_guard_notices_each_way_of_spelling_a_literal() {
    for line in [
        "Style::default().fg(Color::Black)",
        "pub const ACCENT: Color = Color::Indexed(105);",
        "let c = Color::Rgb(135, 135, 255);",
        "const COLOR_RUNNING: u8 = 108;",
        r#"format!("\x1b[38;5;108m{s}")"#,
    ] {
        assert!(spells_a_colour(line), "the guard misses `{line}`");
    }
    for line in [
        "Style::default().fg(theme::INK_ON_FILL)",
        "pub const ACCENT: Color = Color::Indexed(tokens::ACCENT);",
        "const COLOR_RUNNING: u8 = tokens::NOTICE_SUCCESS;",
        r#"format!("\x1b[38;5;{color}m{s}")"#,
        "Color::Reset",
        "Color::BlackboardGreen",
    ] {
        assert!(!spells_a_colour(line), "the guard flags `{line}`");
    }
}
