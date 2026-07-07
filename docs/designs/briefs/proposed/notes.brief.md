# Design brief — engineer-cli · Notes (five-second capture, browse later, `$EDITOR` for prose)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal Notes surfaces — the quick-capture overlay reachable from anywhere, the notes browser (list / `/`-search / read / archive / edit), and the one-line anchor read-back — *plus* the `$EDITOR`-for-prose path that replaces the in-TUI textarea for long-form bodies. Extend `../../design-system.dc.html` (the style anchor); a `notes.dc.html` board is the natural home for the mock, mirroring `timer.dc.html`.
**Status:** proposed. Capture, browse, search, read, archive, and in-TUI edit are **shipped** (`src/app/screens/notes.rs`, `src/app/capture.rs`); **`$EDITOR`-for-prose is the open work** — the omnibus principle the shipped capture quietly skipped — and it is the first slice being implemented.

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the notes daily-loop job (`../../daily-loop.brief.md` job 3 — "capture a note from anywhere, fast … browse later: list, search, open, archive … a note's anchor must read back in one line") *plus* the `$EDITOR`-for-prose principle from the retired omnibus (`terminal-client.brief.md` §3), grounded on the shipped `engineer` API. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "When I read something worth keeping I want it captured in five seconds — from any screen, without losing my place: type the thought, maybe pin the book and the page I'm holding, save, gone.
> I never want to lose a half-typed thought to a stray `Esc`.
> Later I want to *find* it — list, `/`-search, open, archive the stale ones — and a note pinned to *SICP · ch 3 · p.142* had better say exactly that in one row, so I don't have to open it to remember where it lives.
> But when the thought is longer than a line — a paragraph working through an idea, a retro reflection — don't make me type prose into a ratatui box.
> Drop me into my `$EDITOR`, the way `git commit` does, and take back what I write."

The throughline: **capture and browse are shipped and honest** — the `<Space>c` overlay, the `/`-searchable browser, and the one-line anchor read-back all work today. The one gap is that a long-form body is still typed into an in-TUI `tui-textarea`, not the user's editor — the **`$EDITOR`-for-prose** principle the omnibus set out and the shipped capture skipped. Closing it (a one-line thought stays in the overlay; a paragraph opens `$EDITOR`) completes the notes loop the way the workflow always described it.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

The notes module implements **daily-loop job 3** — "capture a note from anywhere, fast … browse later" — decomposed into the outcomes below. Two are shipped and kept here as the module record; the third is the open slice.

**Capture from anywhere, in five seconds.** *Shipped.* Minimal fields — the thought (the star), optionally a book + a place in it — explicit save, gone; reachable from any screen without costing a navigation, and never losing input. Lives as the `<Space>c` quick-capture overlay (`src/app/capture.rs`), also fed by the `:note <text>` palette handoff.

**Find it again.** *Shipped.* The browser (`src/app/screens/notes.rs`): list, `/`-search, open the full read (content + citations), and archive/unarchive — with a note's place readable in one row of grid text so browsing never means opening. Archived notes fold back in *dimmed* (`t`), not hidden, so the ledger stays legible.

**Compose long-form in `$EDITOR`, not a ratatui box.** **The open, first-to-build slice.** A body longer than a line — and the retro reflections `week-planning.brief.md` owns — opens the user's `$EDITOR` (the `git commit` pattern) and takes the written buffer back into the note's `content`. The quick-capture overlay stays for the one-line thought; the in-TUI textarea is *replaced* for prose, not augmented. This is the `$EDITOR`-for-prose principle (`terminal-client.brief.md` §3) the shipped capture quietly skipped.

---

## 3. Principles that genuinely bind

- **Capture is sacred.** The quick-capture path works from any screen and never loses input — an accidental `Esc` on a non-empty draft only *arms* a discard (warns), a second `Esc` confirms, any other key resumes editing (shipped in `capture.rs`). This must survive the new `$EDITOR` boundary: a user who quits the spawned editor without writing must not silently lose the draft.
- **`$EDITOR` for prose; the overlay for a line.** Long-form bodies (and retro reflections) open in the user's editor — never rebuild a rich long-form editor in ratatui. A one-line thought stays in the overlay where five-second capture lives. The two-tier rule is the whole design.
- **Explicit save for new entities.** Save is `Ctrl-S`, never autosave; `Enter` is a newline in the content editor. This is the product's save model, and it holds for the `$EDITOR` round-trip too (writing the buffer is the save).
- **The one-line anchor read-back.** An anchored note reads its place back in one row (`SICP · ch 3 · p.142`, from the server-rendered `address_label`); a loose thought is a single row with no anchor line. Never make the user open a note to recall where it lives.
- **Content-first, title derived.** Quick-capture is content-first: the title is lifted from the first non-empty line and the full text is kept verbatim in `content`, so truncating the title never loses input (`derive_title_content`). A note captured body-first still lists by its first real line.
- **Archive shelves, it doesn't shred.** Archiving is reversible and dims in place; the browse default never offers a destroy. See §5's decision — the same reasoning as progress's "a target is a promise, not a record you delete."

---

## 4. What's already in the app (orientation)

- **Shipped:** the quick-capture overlay (`src/app/capture.rs`) via `<Space>c` from anywhere and the `:note <text>` palette handoff — a multiline content field, an optional book anchor (a live search over the books list, the timer bind-panel idiom), an optional page, `Ctrl-S` save, and the sacred-capture discard guard. The browser (`src/app/screens/notes.rs`): list, `/`-search, a full detail read, archive/unarchive (`a`), `t` to reveal archived (dimmed), and `e` to edit — which hands the note back to the *same* overlay pre-filled for a `PATCH` (one editor, two verbs: POST new, PATCH existing).
- **Widgets/idioms to reuse:** `bordered()` panels, full-row inverse selection with `▌`, `notify` tiles, `footer_hints`, the centered modal the overlay already draws, and the live book-picker (shared with the timer bind panel). The anchor read-back is `notes.rs::anchor_line`; the content-first title is `capture.rs::derive_title_content`.
- **Notes API client — wired and consumed:** `list_notes`, `get_note`, `create_note`, `update_note`, `archive_note`, `unarchive_note` (`src/api/notes.rs`).
- **Client present, no face yet (dead code):** `delete_note` (`DELETE /notes/:id`), `unlink_note` (`PATCH /notes/:id/unlink`), and `book_anchor_data` (`GET /books/:id/anchor_data`, returning `AnchorData` → editions/chapters/sections) are implemented in `src/api/notes.rs` behind the module's `#![allow(dead_code)]` but have **no screen or caller** — a hard-delete verb, a book-detach verb, and the richer chapter/section anchor picker the overlay's page-only anchor could grow into. Wiring any of them is an explicit design decision (§5, §8), not a gap to paper over.
- **What genuinely does not exist yet:** the `$EDITOR` spawn/round-trip — there is no `$EDITOR`/`$VISUAL` usage anywhere in `src`; a headless `engineer note` verb suite (capture/list/search/show with `--json`); a delete-or-unlink face; and the chapter/section anchor picker (today the overlay anchors a book + a bare page only).

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

Both the notes CRUD and the anchor-data read are **shipped server-side** — this module is CLI-only work, not blocked on the server.

- **Notes CRUD + member actions** — `resources :notes, only: %i[index show create update destroy]` with `member { patch :unlink; patch :archive; patch :unarchive }` (`routes.rb:365-371`).
  - `GET /api/v1/notes?q=&book_id=&domain_id=&has_physical_paper=&archived=all|true` — the browser list; `archived=all` folds archived notes back in (rendered dimmed), default is active-only. Consumed today.
  - `GET /api/v1/notes/:id` — the full record (content + citations a list row may omit). Consumed on detail open.
  - `POST /api/v1/notes` — create. Body `{ "note": { title, content?, book_id?, anchors?: [{ chapter_id? | section_id? | page?, edition_ids? }], … } }` — the `NoteInput` shape. Consumed on save-new.
  - `PATCH /api/v1/notes/:id` — update. **Omitting `anchors` leaves existing anchors untouched; sending `anchors` replaces them** (the `NoteInput` contract — hold this when the `$EDITOR` edit reuses the same body). Consumed on edit-save.
  - `DELETE /api/v1/notes/:id` — hard delete. **Client built (`delete_note`), no face.** See the decision below.
  - `PATCH /api/v1/notes/:id/unlink` — detach a note from its book, keeping the note. **Client built (`unlink_note`), no face.**
  - `PATCH /api/v1/notes/:id/{archive,unarchive}` — the reversible shelve. Consumed today (`a` in the browser).
- **Anchor data** — `GET /api/v1/books/:book_id/anchor_data` (`resources :books do member { get :anchor_data } end`, `routes.rb:354-355`). Returns a book's editions + chapters + sections for building a citation richer than a bare page. **Client built (`book_anchor_data` → `AnchorData`/`AnchorChapter`/`AnchorSection`), no face** — the overlay anchors book + page only today.

Note object shape (`src/api/notes.rs::Note`): `{ id, title, content?, book_id?, book_linked, book_title?, domain_id?, subdomain_id?, has_physical_paper, paper_location_label?, archived_at?, citations: [{ address_label?, page?, … }], updated_at? }`. The server renders each citation's `address_label`, and the CLI reads that back rather than re-deriving the place string — so a richer anchor (chapter/section, not just page) becomes a one-line read-back for free once it's written.

### Decision record — archive is the default shelf; delete/unlink stay deliberate

Two write verbs exist server-side that the shipped UI intentionally routes around:

- **Archive, not delete, is the browse default.** The browser offers `a` = archive/unarchive (reversible, dims in place) and never a destroy. `DELETE /notes/:id` (`delete_note`) exists but has no face because a study note is a record you shelve, not shred — the same reasoning as progress's "a target is a promise, not a record you delete." If a hard-delete verb lands, it should be a confirmed, deliberate action (a `:note delete` palette verb or a guarded key) with copy reading "delete (permanent)," visibly distinct from `a`.
- **Unlink is a book-detach, not an archive.** `PATCH /notes/:id/unlink` (`unlink_note`) severs the book anchor while keeping the note — a different intent from archiving the whole note. It has no face yet; if wired, it belongs on the detail read or the edit overlay as "detach from book," never conflated with archive.

Wiring either is a small, self-contained follow-up (§8) — flagged here so it is a decision, not a silent omission.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the pre-lifecycle `daily-loop.brief.md` cited no longer exist). Assemble from shipped atoms only: `bordered()` panels, full-row inverse selection with `▌`, `status_pill`, `notify` tiles, the centered modal the overlay already uses, and the live book-picker (the timer bind-panel idiom). Keyboard-only, neovim-flavoured — `j`/`k`, `/` search with the shipped prompt, the `<Space>c` leader, `Ctrl-S` save, `Tab` to cycle fields; the footer advertises the active keys. ASCII-only diagrams. The `$EDITOR` handoff is the one surface with **no in-TUI face**: spec how the TUI *suspends and restores* around the spawned editor (the terminal is handed to `$EDITOR`, the alt-screen restored on exit, the buffer round-tripped into `content`) rather than drawing a text panel. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see `cross-cutting.brief.md`.

---

## 7. Out of scope

Rebuilding a rich long-form editor in ratatui — that is exactly what `$EDITOR` replaces (the in-TUI `tui-textarea` stays only for the one-line quick thought). The web notes app's own editing depth (a tags-management UI, physical-paper location curation beyond the fields the note model already carries, source-url management) — the CLI *consumes* those fields, it doesn't redesign their web management. Retro reflections' own screen — long-form retro *reuses* this module's `$EDITOR` path, but the plan/retro surface itself is `week-planning.brief.md`. The spaced-repetition/review life of a note lives in `review.brief.md`. Durable offline capture / draft persistence across app exit — the draft lives in memory today; making capture survive a crash or offline session is a cross-cutting concern (`cross-cutting.brief.md`). Light mode; a truecolor requirement.

---

## 8. Phasing

1. **`$EDITOR`-for-prose (the headline slice) — first.** Spawn `$EDITOR` (honouring `$VISUAL` then `$EDITOR`, the `git commit` precedence) for a long-form body: from the overlay (an "open in editor" affordance when the thought outgrows a line) and from `e` on the browser. Suspend/restore the TUI cleanly around the child process, round-trip the buffer back into `content`, and hold **capture-is-sacred** across the boundary (a quit-without-write must not silently drop the draft). The suspend/resume plumbing and the spawn helper are shared machinery — cross-ref `cross-cutting.brief.md`. Track as its own epic (the repo's pattern: a `notes.dc.html` design pass → this gap brief → epic, as `timer.dc.html` + timer did).
2. **Headless twin — `engineer note` verbs.** `note capture <text>` (with `-`/stdin and `--book`/`--page`), plus `note list`/`search`/`show` with `--json` and a piped-plain form, mirroring the TUI↔headless duality `engineer timer` already ships. Bound by the same NO_COLOR / TTY-detect / meaningful-exit-code contract (`cross-cutting.brief.md`).
3. **Wire the built-but-faceless verbs (optional, self-contained).** A confirmed hard delete (`delete_note`) and a book-detach (`unlink_note`) per §5's decision record; and the richer chapter/section anchor picker over `book_anchor_data`, so a note can pin `ch 3 · §3.2`, not just `p.142`.
