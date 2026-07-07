# Design brief — engineer-cli · Activities & Segments (the core domain ledger)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal Activities table and its row-action grammar, the new-activity form, the segment audit, *and* their headless twin — the one open piece being the `engineer log …` quick-capture verb. Extend `../../design-system.dc.html` (the style anchor); an `activities.dc.html` board is the natural home for the mock, mirroring `timer.dc.html`.
**Status:** **shipped.** The table (`src/app/screens/activities.rs`), the new-activity form (`src/app/screens/activity_new.rs`), and the segment audit (`src/app/screens/audit.rs`) are live, with the activities and segments API clients wired (epic #7 daily-loop). This brief is kept as the module record; §8 is the one residual gap — the headless `engineer log …` capture verb.

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries two jobs of the retired omnibus (`proposed/terminal-client.brief.md` jobs 3 and 4) — *work the activities table* and *log after the fact, fast* — reconciled against the shipped CLI and the `engineer` API. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "The activities table is my ledger — every session I've run, planned, or logged. I want to work it like a buffer: `j`/`k` down the rows, `f` to ring through the lifecycle, `/` to narrow the page, `⏎` to read the full record, and a single key to complete, archive, duplicate, or bind the live timer to the row under the cursor — no mouse, no menu, no leaving the grid.
> When a logged segment looks wrong — a five-hour row I forgot to stop, a near-zero blip, a session with no metadata — I want to fix it in the audit: acknowledge it as fine, trim it to a sane length, or delete it, two-press so I never nuke a row by fat-fingering.
> And when I *did* the work but forgot to time it, I want to log it after the fact in one shot — `engineer log 'Crafting Interpreters ch.4' --minutes 45 --kind reading` — the same record a stopped timer would have written, without opening the timer or arrowing through a form."

The throughline: the table and its audit are the terminal's **domain ledger**, and they're shipped — the dense, vim-worked list plus the flagged-segment cleanup. The one thing the workflow still asks for that the command line can't do is the last line above: after-the-fact capture *as a one-shot*. The TUI already logs after the fact (the `a` new-activity form writes a completed activity with a duration); the headless twin the duality contract promises — `engineer log …` and its `:log` palette entry — is the residual gap that closes this module.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**4. Work the activities table.** List, filter, sort, open, edit, complete, archive, unarchive, duplicate — with the modal vim table grammar — and view/repair the segments of an activity in the audit. *Shipped* as three screens: the table (`activities.rs`), the new-activity form (`activity_new.rs`), and the segment audit (`audit.rs`). The row actions of the daily loop all live on a single keystroke, mutations refetch the visible page so the ledger mirrors the server, and destructive-vs-reversible states are treated differently (archive toggles quietly; delete asks twice).

**3. Log after the fact, fast.** Record a completed activity — or a manual segment on an existing one — without having run the timer, from a quick one-shot `engineer log …`. **This is the open, residual slice.** The interactive path exists (the `a` form → `create_activity` with `duration_minutes`); the *headless* one-shot and its `:log` palette verb do not. The API it needs is already shipped server-side (see §5), so this is CLI-only work, not blocked on `engineer`.

---

## 3. Principles that genuinely bind

- **A ledger you scan and act on by the row — not a spreadsheet (the one deliberate concession).** Of every surface in the client, the activities table sits closest to the `engineer` web UI, and it earns its place as *the* dense reference view: scan today/recent, act on the row under the cursor. But it is **capped against web parity** by the governing principle (`cross-cutting.brief.md` — sterling, not a replica): no rich saved-filter matrix, no bulk edit, no server-side `POST /search` until a real workflow outgrows the client-side `/` (§7 declines it deliberately). The module's *watch-native* core — the parts that should grow — are **capture** (`engineer log`, `t` to bind the timer) and the **audit** (a short flagged-segment list you clear to empty, two-press). Deepen those; hold the table where it is.
- **Modal vim grammar, everywhere.** The table is a buffer: `j`/`k`/`gg`/`G` move, `f` rings the filter, `/` narrows, `⏎` opens the detail, `Esc` backs out, single letters act on the row (`c`omplete, `a`rchive, `d`uplicate, `t` bind timer). The form is insert/normal (`i`/`Esc`, `:w`/`⎵s` to submit). The `engineer log` verb inherits this discipline the moment it grows a TUI affordance, but its first form is headless. No mouse; the footer advertises the live keys.
- **Density matters — a table in a character grid.** Six columns in fixed character widths (status pill · kind · title · domain-by-name · duration · relative when), a bottom-border `page N of M · X total` status line, full-row inverse selection with a `▌` marker. The terminal palette has **no per-domain colours**, so domain reads as a name, not a swatch. Every column earns its width; nothing wraps.
- **TUI ↔ headless duality is first-class.** Every capability the screens expose should also exist as a non-interactive one-shot with `--json` (machine) and plain-text (pipe) forms — TTY-detected, `NO_COLOR`-respecting, meaningful exit codes, no ANSI when piped. The table's *reads and mutations* are still TUI-first by design; the one verb this contract now obliges is `engineer log …`, whose template is `src/timer_cli.rs` (see `cross-cutting.brief.md` for the shared contract).
- **Derived actuals — segments are the ledger, pace is derived elsewhere.** This module owns the *writes* to the ledger (create an activity, append/trim/delete a segment). It never keeps a second copy of the rollups: pace, totals, and the flagged-segment derivation are read-through from the Progress/timer surfaces. The audit *acts on* flags it does not compute.
- **Reversible states toggle quietly; destructive ones confirm.** Archive/unarchive is one reversible key with no prompt (it mirrors the notes resource); complete and duplicate are single-press with a `notify` tile; segment delete is a strict two-press confirm on the same row. Match this asymmetry in any new affordance.

---

## 4. What's already in the app (the shipped reality)

- **The Activities table** (`src/app/screens/activities.rs`, ~938 lines) — the first screen to drive `meta.page` **server pagination** (`[`/`]` step pages), a single **filter ring** on `f` folding the two server axes worth cycling (lifecycle `status=` and `archived`) into one key (`all → planned → started → completed → archived`), a **`/` client-side narrow** over the *loaded page* across title/kind/domain (the list API takes no `q`, so kind — free-form on the wire — is reached this way), **semantic status pills**, the daily-loop **row actions** complete / archive / unarchive / duplicate, an `⏎` **detail read** that opens instantly from the row then refines with the full record, and **`t`** to start-and-bind the live timer to the selected row. Mutations refetch the page rather than patch in place.
- **The new-activity form** (`src/app/screens/activity_new.rs`) — the `a` capture screen: title / kind / duration / notes fields, insert-vs-normal modes, per-field validation errors surfaced from the API, submit on `:w` / `⎵s`. Because it posts a `duration_minutes`, **this is already after-the-fact logging inside the TUI** — the headless verb in §8 is its one-shot twin, not a new capability.
- **The segment audit** (`src/app/screens/audit.rs`, ~491 lines) — the flagged-segment cleanup, grouped by severity (`IMPLAUSIBLY LONG` / `ZERO / NEAR-ZERO` / `MISSING METADATA`, from the `too_long` / `near_zero` / missing-metadata flags): **`a`** looks-right/acknowledge, **`t`** trim (a segment-edit **PATCH preset** that shortens `minutes` to the settings long-fence), **`d`** delete (two-press confirm). A clean log is an empty screen and no badge.
- **The API clients, wired** — `src/api/activities.rs` (list/show/create/complete/archive/unarchive/duplicate) and `src/api/segments.rs` (segment `update` = the trim preset, `delete` = the audit delete / post-save undo). The audit read/acknowledge go through `src/api/audit.rs`.
- **What genuinely does not exist yet:** the `engineer log …` headless verb and its `:log` palette entry (the command-palette brief flags `:log` as still-to-add); the nested-segments **create** client that an `engineer log --activity …` (append a manual segment to an existing activity) would call; and the `POST …/search` collection client — the table's `/` is a *deliberate* client-side narrow, and timer-bind fuzzy search runs through `GET /api/v1/timer/candidates`, so nothing has needed the richer server search yet.

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

All the routes this module needs are **shipped server-side** — including the two the `engineer log` verb will call — so the residual work is CLI-only. Verified in `routes.rb:338-352` (`namespace :api`, `namespace :v1`, `resources :activities`):

- **Activities** — `resources :activities` gives `index` / `show` / `create` (`routes.rb:338`). `GET /api/v1/activities?status=&archived=&page=&per_page=` (the table's list), `GET /api/v1/activities/:id` (the detail refine), `POST /api/v1/activities` with `{ "activity": { title, kind, duration_minutes, … } }` (the form's write, and `engineer log`'s primary write). Consumed today in `src/api/activities.rs`.
- **Member actions** (`routes.rb:343-348`) — `patch :complete`, `patch :archive`, `patch :unarchive`, `post :duplicate`. Wired to the table's row keys; `archive`/`unarchive` are the reversible pair the table toggles without a confirm.
- **Collection search** (`routes.rb:349-351`) — `post :search`. Exists server-side; **the CLI has not wired it** — the table narrows client-side with `/` and timer-bind uses `GET /api/v1/timer/candidates`. Left unclaimed until a workflow needs server-side search beyond the loaded page.
- **Nested segments** (`routes.rb:342`) — `resources :segments, only: %i[index create update destroy]` under an activity. The CLI wires **`update`** (`PATCH /api/v1/activities/:id/segments/:id` — the audit trim preset, `{ "minutes": <n> }`) and **`destroy`** (`DELETE …` — the audit delete / post-save undo) in `src/api/segments.rs`. **`create`** (`POST /api/v1/activities/:id/segments`) is the one route `engineer log --activity …` (append a manual segment) would newly consume; `index` is unused.
- **The segment-audit read is not this module's to own.** The flagged-segment list and the acknowledge write live at `GET /api/v1/progress/audit` and `PATCH /api/v1/progress/audit/segments/:id/acknowledge` (`routes.rb`, the `progress` collection), consumed in `src/api/audit.rs`. They belong to the Progress/timer surfaces (server-derived flags, the audit badge); this module *acts on* their results (trim/delete via the ordinary segments API). Mentioned here for completeness — see `shipped/timer.brief.md` §C and `proposed/progress.brief.md`.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus cited no longer exist). Assemble from shipped atoms only: `bordered()` panels with a bottom-border status line, `widgets::status_pill` (black ink on a semantic fill — ` done ` success, ` reading ` accent, ` hold ` warn, ` stop ` danger), full-row inverse selection with a `▌` marker, `footer_hints` (each key a black-on-accent cap), `notify` tiles for the mutation results. Keyboard-only, neovim-flavoured; ASCII-only diagrams; one font size, weight-and-colour for hierarchy. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see `cross-cutting.brief.md`.

---

## 7. Out of scope

Pace, targets, and rollups (that's `proposed/progress.brief.md` — actuals stay derived, this module never recomputes them); the timer face, idle reclaim, focus rhythm, and overrun (all `shipped/timer.brief.md`); the *server-side derivation* of segment flags and the audit read/badge (owned by Progress/timer — this module only consumes the trim/delete/acknowledge write side); billing, rates, and invoice export beyond a generic `--json`; the richer `POST …/search` collection endpoint until a workflow outgrows the client-side `/`; a stored client-side ledger; light mode.

---

## 8. Phasing

One residual slice — the rest of the module is shipped.

1. **`engineer log …` — the headless capture verb (job 3), and its `:log` palette entry.** A one-shot that records a completed session without the timer, obeying the TUI↔headless duality contract (`--json` + piped-plain, TTY-detect, `NO_COLOR`, meaningful exit codes). The implementation template is `src/timer_cli.rs` (the `engineer timer` suite); the cross-cutting contract is `cross-cutting.brief.md`. Two shapes to design as one verb: (a) **log a new completed activity** — `engineer log '<title>' --minutes <n> --kind <k>` → `POST /api/v1/activities` with `duration_minutes` (client already wired), the exact write the `a` form makes; (b) **append a manual segment to an existing activity** — `engineer log --activity '<fuzzy match>' --minutes <n>` → fuzzy-resolve the target (reuse the bind-picker candidates idiom) then `POST /api/v1/activities/:id/segments` (the one segments route still to wire). Route it through the palette as `:log` so it exists inside the TUI too. Track as its own small epic (the repo's pattern: gap brief → epic, as timer did), since it's the only build left in this module.
