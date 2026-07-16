//! The `$EDITOR` hand-off — the `git commit` pattern shared by the quick-capture
//! overlay (#88) and the week retro reflection (#117): seed a temp file, spawn
//! `$VISUAL`/`$EDITOR`, and read the saved buffer back.
//!
//! The TUI suspends the alt-screen around this (`app::run_editor`); the headless
//! `engineer week reflect` calls it straight from the shell. Both share the seed
//! → spawn → read-back mechanics; only the surrounding terminal handling differs.

use std::io::Result;

/// How the editor session ended. The two are kept distinct because the retro
/// reflection treats them differently: an **abort** (a non-zero exit — `:cq`,
/// `false`) keeps the note untouched — capture-is-sacred across the boundary —
/// while a **saved** buffer persists, and a saved-but-empty buffer clears the
/// note deliberately (the server's `week_notes` contract treats empty as clear).
/// The quick-capture overlay collapses both non-writes to "keep the draft".
#[derive(Debug, PartialEq, Eq)]
pub enum EditorOutcome {
    /// The editor exited non-zero — an abort; nothing to persist.
    Aborted,
    /// The editor exited zero — the saved buffer, trailing newline trimmed. May
    /// be empty (a deliberate clear).
    Saved(String),
}

/// `$VISUAL` then `$EDITOR`, else `vi`.
pub fn resolve_editor() -> String {
    std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string())
}

/// Open the seed in `$VISUAL`/`$EDITOR` (falling back to `vi`) and read it back.
pub fn edit(seed: &str) -> Result<EditorOutcome> {
    edit_with(&resolve_editor(), seed)
}

/// Write the seed to a temp file, run `editor` on it, and read it back.
/// `editor` may carry flags (`code -w`), so split on whitespace. Returns
/// [`EditorOutcome::Aborted`] on a non-zero exit and [`EditorOutcome::Saved`]
/// (trailing newline trimmed, possibly empty) on a clean save.
pub fn edit_with(editor: &str, seed: &str) -> Result<EditorOutcome> {
    let path = std::env::temp_dir().join(format!("engineer-note-{}.md", std::process::id()));
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
    fn roundtrips_the_edited_buffer() {
        // A fake editor that overwrites the file it's given ($1).
        let editor = write_fake_editor("fakeed", "printf 'edited body' > \"$1\"");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved("edited body".into()));
    }

    #[test]
    fn an_empty_save_is_distinct_from_an_abort() {
        // A clean exit with an emptied buffer is a deliberate clear, NOT an abort.
        let editor = write_fake_editor("emptyed", ": > \"$1\"");
        let out = edit_with(editor.to_str().unwrap(), "seed").unwrap();
        let _ = std::fs::remove_file(&editor);
        assert_eq!(out, EditorOutcome::Saved(String::new()));
    }

    #[test]
    fn a_nonzero_exit_is_an_abort() {
        // `false` exits 1 without touching the file — the abort keeps the seed.
        assert_eq!(edit_with("false", "seed").unwrap(), EditorOutcome::Aborted);
    }
}
