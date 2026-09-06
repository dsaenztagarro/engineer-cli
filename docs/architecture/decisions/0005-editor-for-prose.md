# 0005 — `$EDITOR` for prose; the in-app grammar for a line

**Status:** Accepted (#88 → v0.7.0; adopted by the week retro #117; recorded here at #180)

## Context

Two surfaces in this client need the user to write more than a label: a note's long-form body, and the week's retro reflection.
Both started as, or would naturally have become, a multi-line text panel inside ratatui.

A TUI is a character grid with a one-line input idiom.
A multi-line prose editor with the user's own keymaps, undo, syntax, and muscle memory already exists — it is their `$EDITOR` — and building a worse one inside the app is the wrong medium, in the same way a TUI pivot grid is ([ADR 0002](0002-sterling-not-a-replica.md)).

`git commit` established the pattern this client copies.

## Decision

**Long-form prose opens in the user's editor via the `git commit` pattern; short single-line input stays in the app's `i`/`Esc` insert grammar.**

- Spawn on a temp file, honour `$VISUAL` then `$EDITOR` (git's precedence), suspend the alt-screen around the child process and restore it on exit, and read the saved buffer back (`src/editor.rs`).
- The two-tier rule is the whole design: a one-line thought stays in the quick-capture overlay where five-second capture lives; a paragraph opens the editor. The in-TUI textarea is *replaced* for prose, not augmented.
- **Capture is sacred across the boundary.** A user who quits the spawned editor without writing must not silently lose their draft. Writing the buffer *is* the save — there is no autosave — and an aborted editor changes nothing.
- An empty buffer means what the target's contract says it means, and that must be distinguished from an abort: for the week note, an empty body is a deliberate *clear*; a quit-without-write is not.
- Titles, queries, and activity names stay in the app. The hand-off is for the *body*, not the *label*.

## Alternatives considered

- **A rich in-TUI textarea (`tui-textarea` or similar).** Rejected: it competes with the user's editor and loses — no personal keymaps, no undo tree, no syntax, no muscle memory — while adding a surface to maintain. The shipped capture overlay originally did this for note bodies; #88 replaced it.
- **A bundled minimal modal editor of our own.** Rejected for the same reason, with more code.
- **Prose only on the web.** Rejected: it breaks the daily loop at exactly the moment the user has something worth writing, and the whole point of the terminal client is that they never have to leave.
- **Honouring `$EDITOR` only, not `$VISUAL`.** Rejected: git's precedence is the convention users already have configured, and matching it means zero setup.

## Consequences

- `src/editor.rs` is shared machinery, not per-module plumbing: notes long-form bodies and the week retro reflection both use it, and any future prose surface must too rather than inventing a panel.
- The suspend/restore dance around the child process is the fragile part and lives in exactly one place.
- The same hand-off carries structured edits where a form would be heavier than the task — the queue's `e` edit-times on a rejected write opens the intent payload in `$EDITOR` and parses the saved buffer back, re-pending with a fresh idempotency key ([ADR 0004](0004-derived-never-stored-and-the-write-queue.md)). A parse-refused buffer changes nothing, holding the same abort-is-safe rule.
- There is no in-app long-form editing surface to test, style, or keep consistent with the rest of the chrome — the cost of prose editing is borrowed from a program the user already trusts.
