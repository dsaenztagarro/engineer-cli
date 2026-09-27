//! Code never references a design (the scope record), and cites a decision
//! record by its theme, never by number.
//!
//! A design canvas regenerates, and its section labels renumber on the next
//! export, so a comment citing one points at something a reader cannot resolve.
//! Behaviour is pinned by a test; the why is a decision record, which is
//! hand-owned and may be cited. The rule was written down in the web client's
//! repository with no guard, and 47 violations accumulated there; this test is
//! the guard. A record is named for its theme and amended in place, so a number
//! names nothing a reader can open, in this repository or the server's.

use std::{fs, path::Path};

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

fn comment(line: &str) -> Option<&str> {
    line.find("//").map(|start| &line[start..])
}

fn offences_in_src(offends: fn(&str) -> bool) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    source_files(&root.join("src"), &mut files);
    let mut offences = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        for (n, line) in source.lines().enumerate() {
            if offends(line) {
                let rel = file.strip_prefix(root).unwrap().display();
                offences.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    offences
}

// Only comment text is read: a `§` inside a string is user-facing copy, such as
// the anchor picker's "chapter · §section". In a comment, a `§` followed by a
// letter is a design section label; a `§` followed by a digit is a book or RFC
// section.
fn cites_a_design(line: &str) -> bool {
    let Some(comment) = comment(line) else {
        return false;
    };
    let section_label = comment.match_indices('§').any(|(i, _)| {
        comment[i + '§'.len_utf8()..]
            .chars()
            .next()
            .is_some_and(char::is_alphabetic)
    });
    comment.contains(".dc.html")
        || comment.contains("docs/designs")
        || comment.contains("tokens.css")
        || comment.contains(".html")
        || section_label
}

#[test]
fn no_source_file_references_a_design() {
    let offences = offences_in_src(cites_a_design);
    assert!(
        offences.is_empty(),
        "cite the test for what the code does and the decision record for why, never a design:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_design_guard_notices_each_way_of_citing_one() {
    for line in [
        "//! Timer screen (timer.dc.html §Timer hero).",
        "// Terminal-palette 256 colours (docs/designs/README.md palette mapping).",
        "//! Palette adapted from design tokens (tokens.css).",
        "/// pill contract (navigation-bar.html) keeps a fixed width",
        "/// the §Queue inspector board",
        "//! the notes module's §C twin",
    ] {
        assert!(cites_a_design(line), "the guard misses `{line}`");
    }
    for line in [
        "// A divergence gates only its own stream (the client-state record).",
        "/// A coded conflict's RFC 7807 §3.2 extension members.",
        "Picker::new(\"chapter · §section\", items)",
        "// See the terminal-surface record.",
        "let label = format!(\"{title} · p.{page}\");",
    ] {
        assert!(!cites_a_design(line), "the guard flags `{line}`");
    }
}

// `ADR 0004`, `ADR-0004`, `engineer ADR 0036` and a record's file path alike:
// the number is what goes stale, whichever repository it once named.
fn names_a_record_by_number(line: &str) -> bool {
    let Some(comment) = comment(line) else {
        return false;
    };
    let four_digits_after = |marker: &str, separators: &[char]| {
        comment.match_indices(marker).any(|(i, m)| {
            let digits: String = comment[i + m.len()..]
                .trim_start_matches(separators)
                .chars()
                .take(4)
                .collect();
            digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit())
        })
    };
    four_digits_after("ADR", &[' ', '-', '_', '#']) || four_digits_after("decisions/", &[])
}

#[test]
fn no_source_comment_names_a_decision_record_by_number() {
    let offences = offences_in_src(names_a_record_by_number);
    assert!(
        offences.is_empty(),
        "cite a decision record by its theme (\"the client-state record\", \"engineer's api-wire record\"), never by number:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_record_number_guard_notices_each_way_of_citing_one() {
    for line in [
        "//! The offline write queue (ADR 0004).",
        "// ADR 0004 rule 2: never silently lose a segment.",
        "/// The stable conflict-code vocabulary (engineer ADR 0036).",
        "    // replays plain (ADR0036)",
        "// See ADR-0004.",
        "//! docs/architecture/decisions/0004-derived-never-stored.md",
    ] {
        assert!(names_a_record_by_number(line), "the guard misses `{line}`");
    }
    for line in [
        "//! The offline write queue (the client-state record).",
        "/// The conflict vocabulary (engineer's api-wire record).",
        "/// A coded conflict's RFC 7807 extension members.",
        "let label = \"ADR 0004\";",
    ] {
        assert!(!names_a_record_by_number(line), "the guard flags `{line}`");
    }
}
