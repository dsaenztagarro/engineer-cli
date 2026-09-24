//! The `$EDITOR` hand-off: seed a temp file, spawn the editor, read the saved buffer back.

use std::io::Result;

#[derive(Debug, PartialEq, Eq)]
pub enum EditorOutcome {
    Aborted,
    Saved(String),
}

pub fn resolve_editor() -> String {
    resolve_editor_from(|name| std::env::var(name).ok())
}

fn resolve_editor_from(var: impl Fn(&str) -> Option<String>) -> String {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| var(name).filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_string())
}

pub fn edit(seed: &str) -> Result<EditorOutcome> {
    edit_with(&resolve_editor(), seed)
}

pub fn edit_with(editor: &str, seed: &str) -> Result<EditorOutcome> {
    // Unique per call, not just per process: concurrent sessions in one
    // process (parallel tests; a TUI overlay racing a spawned task) must
    // never share a buffer file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("engineer-note-{}-{seq}.md", std::process::id()));
    std::fs::write(&path, seed)?;

    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()?;

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);

    if !status.success() {
        return Ok(EditorOutcome::Aborted);
    }
    Ok(EditorOutcome::Saved(
        content.trim_end_matches('\n').to_string(),
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_fake_editor(name: &str, script: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("engineer-{name}-{}.sh", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_clean_exit_saves_the_edited_buffer() {
        let editor = write_fake_editor("fakeed", "printf 'edited body' > \"$1\"");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved("edited body".into()));
    }

    #[test]
    fn an_empty_save_is_distinct_from_an_abort() {
        let editor = write_fake_editor("emptyed", ": > \"$1\"");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved(String::new()));
    }

    #[test]
    fn a_nonzero_exit_is_an_abort() {
        assert_eq!(edit_with("false", "seed").unwrap(), EditorOutcome::Aborted);
    }

    #[test]
    fn quitting_without_writing_is_an_abort_even_after_the_buffer_changed() {
        let editor = write_fake_editor("quitbang", "printf 'half typed' > \"$1\"; exit 1");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Aborted);
    }

    #[test]
    fn a_saved_buffer_loses_its_trailing_newlines() {
        let editor = write_fake_editor("nled", "printf 'body\\n\\n' > \"$1\"");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved("body".into()));
    }

    #[test]
    fn an_editor_command_may_carry_its_own_flags() {
        let editor =
            write_fake_editor("flagged", "[ \"$1\" = --wait ] && printf 'waited' > \"$2\"");
        let out = edit_with(&format!("{} --wait", editor.to_str().unwrap()), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved("waited".into()));
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi_skipping_blank_values() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            resolve_editor_from(env(&[("VISUAL", "code -w"), ("EDITOR", "nano")])),
            "code -w"
        );
        assert_eq!(
            resolve_editor_from(env(&[("VISUAL", "  "), ("EDITOR", "nano")])),
            "nano"
        );
        assert_eq!(resolve_editor_from(env(&[])), "vi");
    }
}
