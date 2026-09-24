//! Code never references a design (ADR 0006).
//!
//! A design canvas regenerates, and its section labels renumber on the next
//! export, so a comment citing one points at something a reader cannot resolve.
//! Behaviour is pinned by a test; the why is an ADR, which is hand-owned and may
//! be cited. The rule was written down in the web client's repository with no
//! guard, and 47 violations accumulated there; this test is the guard.

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

// Only comment text is read: a `§` inside a string is user-facing copy, such as
// the anchor picker's "chapter · §section". In a comment, a `§` followed by a
// letter is a design section label unless it follows an ADR number, as in
// `ADR 0004 §Rules`; a `§` followed by a digit is a book or RFC section.
fn cites_a_design(line: &str) -> bool {
    let Some(start) = line.find("//") else {
        return false;
    };
    let comment = &line[start..];
    let section_label = comment.match_indices('§').any(|(i, _)| {
        let names_a_section = comment[i + '§'.len_utf8()..]
            .chars()
            .next()
            .is_some_and(char::is_alphabetic);
        let before = comment[..i].trim_end();
        let digits = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let after_an_adr = digits == 4 && before[..before.len() - 4].trim_end().ends_with("ADR");
        names_a_section && !after_an_adr
    });
    comment.contains(".dc.html")
        || comment.contains("docs/designs")
        || comment.contains("tokens.css")
        || comment.contains(".html")
        || section_label
}

#[test]
fn no_source_file_references_a_design() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    source_files(&root.join("src"), &mut files);
    let mut offences = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        for (n, line) in source.lines().enumerate() {
            if cites_a_design(line) {
                let rel = file.strip_prefix(root).unwrap().display();
                offences.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offences.is_empty(),
        "cite the test for what the code does and the ADR for why, never a design:\n{}",
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
        "// ADR 0004 §Rules: a divergence gates only its own stream.",
        "/// A coded conflict's RFC 7807 §3.2 extension members.",
        "Picker::new(\"chapter · §section\", items)",
        "// See ADR 0003.",
        "let label = format!(\"{title} · p.{page}\");",
    ] {
        assert!(!cites_a_design(line), "the guard flags `{line}`");
    }
}

// "ADR 0036" and "engineer ADR 0036" are different records: the bare form is
// this repository's, and a server record cited bare sends the reader to a file
// that does not exist here.
fn bare_adr_numbers(line: &str) -> Vec<String> {
    line.match_indices("ADR ")
        .filter(|(i, _)| !line[..*i].ends_with("engineer "))
        .filter_map(|(i, m)| {
            let digits: String = line[i + m.len()..].chars().take(4).collect();
            (digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
        })
        .collect()
}

#[test]
fn every_bare_adr_citation_names_a_record_in_this_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let records: Vec<String> = fs::read_dir(root.join("docs/architecture/decisions"))
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().to_str().map(|n| n[..4].to_string()))
        .collect();
    let mut files = Vec::new();
    source_files(&root.join("src"), &mut files);
    let mut offences = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        for (n, line) in source.lines().enumerate() {
            for number in bare_adr_numbers(line) {
                if !records.contains(&number) {
                    let rel = file.strip_prefix(root).unwrap().display();
                    offences.push(format!("{rel}:{}: ADR {number}", n + 1));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "a bare ADR number is this repository's; cite a server record as `engineer ADR NNNN`:\n{}",
        offences.join("\n")
    );
}

#[test]
fn a_server_adr_is_told_apart_from_a_local_one() {
    assert_eq!(bare_adr_numbers("// ADR 0004 rule 2"), vec!["0004"]);
    assert!(bare_adr_numbers("// engineer ADR 0036").is_empty());
    assert_eq!(
        bare_adr_numbers("// ADR 0036 and engineer ADR 0035"),
        vec!["0036"]
    );
}
