# Design brief — engineer-cli daily loop: the terminal as the daily driver

**For:** Claude Design (the **`engineer-cli` terminal project** — a separate project from the web `engineer` one, seeded per this folder's `README.md`; the two media must not cross-contaminate).
**Produces:** five self-contained screen mockups in the kit convention (**one screen per file**, each rendered against `terminal-tokens.css`, anchored on the style template `books.html`): `timer.html`, `notes.html`, `activities.html`, `review.html`, `command-palette.html` — plus the **persistent header timer cell** designed once as shared chrome that every mockup carries.
**Do not edit** `books.html` (the anchor mockup), `terminal-tokens.css`, or `README.md` — extend them.
**Status:** handoff draft. Tracked by [engineer-cli#8](https://github.com/dsaenztagarro/engineer-cli/issues/8), part of EPIC [engineer-cli#7](https://github.com/dsaenztagarro/engineer-cli/issues/7); companion of the server epic [engineer#652](https://github.com/dsaenztagarro/engineer/issues/652).

> **Historical / superseded (kept as the record of epic #7).** This is the pre-lifecycle brief that drove the first wave of daily-loop screens; its screens have shipped. It predates the `briefs/proposed|shipped` lifecycle and has been **decomposed into per-module briefs** — see the [briefs index](briefs/README.md) (timer, activities, review, notes, command-palette, home). Two anchors it names below are **retired**: `books.html` and `terminal-tokens.css` no longer exist; the live style anchor is [`design-system.dc.html`](design-system.dc.html). Read the module briefs, not this, for current-state guidance.

### How to read this brief (please read first)
This brief is **deliberately problem-first and light on solution.**
It tells you *who this is for*, *what jobs the screens must do*, and *what genuinely constrains you* — then gets out of your way.
The Rust architecture, API client shapes, and implementation decisions live in the epic's issues, not here, so your proposal isn't coerced into one structure.
Where it names an existing widget or shipped screen, that is **reuse context**, not a prescribed answer.
If a better shape occurs to you, propose it.

---

## 1. Who this is for (the workflow — the heart of the brief)

> "My day already runs in a terminal.
> The web app is the cockpit — great for editing a book's editions or reviewing an automation — but the *daily loop* is three gestures I do dozens of times: **start or pause the timer**, **jot a note the moment I read something worth keeping**, and **check what I meant to do today**.
> Leaving the terminal for any of those breaks flow.
> I want the timer visible wherever I am in the app — a glance tells me it's running and for how long — and pausing it should be muscle memory, not navigation.
> A note should cost me five seconds: type the thought, optionally pin it to the book and page I'm holding, done.
> When I have a spare ten minutes I want to knock out a few due review topics right there — show me the topic, I rate how well I remembered, next.
> And I want to *drive* all of it the way I drive vim: keys for the common path, `:` commands for everything."

The throughline: **the terminal is the daily driver, the web app is the garage.**
Capture and timekeeping must be near-zero cost; browsing and rating must be comfortable; nothing here needs to replicate the web's editing depth.

---

## 2. The jobs the screens must do (outcomes, not mechanisms)

Design screens that let the user, with as few keystrokes as possible:

1. **Always know the timer's state.** On *every* screen: is a timer running, paused, or absent — and the elapsed time — readable in one glance at the chrome. Start/pause/resume/stop from anywhere in a couple of keystrokes.
2. **Bind time to the right work.** A timer can start "blank" (just starting the clock) and be bound to an activity afterwards — by searching candidates or minting a new activity from a title. Stopping shows what was written (which activity, how many minutes) so the user trusts the ledger.
3. **Capture a note from anywhere, fast.** Minimal fields (the thought; optionally a book + place in it), explicit save, gone. Browsing notes later: list, search, open, archive — and a note's anchor must read back in one line of grid text.
4. **Browse and act on activities** — the core domain surface: today's and recent activities, filterable (status, kind, date), with row actions (complete, archive, duplicate) and a detail read. This is a *table* in a character grid — density matters.
5. **Run a review sitting.** See what's due (counts, urgency), open the queue, rate each topic (the four ratings: forgot / fuzzy / solid / instant), advance to the next automatically, and exit cleanly mid-sitting. Browsing all topics (not just due) with the API's sorts is secondary but should exist.
6. **Drive everything from `:`.** A command grammar covering navigation (`:books`, `:activities`, `:review`, `:progress`, …) and actions (`:timer …`, `:note …`), with completion for known verbs and a helpful response to unknown ones. The grammar you pin here becomes the product's spine — design it deliberately.

---

## 3. Principles that genuinely bind (constraints, not solutions)

- **A character grid, not a canvas.** One monospace font, one size; hierarchy comes from weight, colour, and space. No shadows, radii, hover, or mouse. The kit's translate/don't-translate table (`README.md`) binds in full.
- **Keyboard-only, neovim-flavoured.** `j`/`k`, `gg`/`G`, `/` search with `n`/`N`, `<Space>` leader, `i`/`Esc` insert/normal in forms, `:` command line. The footer always shows the active screen's keys. New screens extend this grammar; they don't invent a rival one.
- **The timer cell is the canonical atom, translated.** The web's rule (navigation-bar.html §M): a fixed-width compact pill — pulsing dot + `mm:ss` (→ `h:mm:ss`), **no title in the bar**, title revealed on demand, accent colour, never shape-shifting by kind or title length. Translate that *contract* to the header row's idiom; don't imitate its pixels.
- **Capture is sacred.** The quick-capture path must work from any screen and never lose input — an accidental `Esc` should warn or stash, not discard. (New-entity forms use explicit save, per the product's save model.)
- **Design for ~100×30, degrade to 80×24.** Screens must stay usable at the classic minimum; wider terminals gain density, not new information kinds.
- **The environment is always legible.** The header already carries user @ identity-host; production vs development must never be ambiguous while acting on data. (A richer shard/env indicator is on the kit's roadmap but is **not** part of this epic — treat the current header text as the given.)

---

## 4. What's already shipped (orientation, so you extend rather than reinvent)

- **Chrome:** three stacked rows — header (`engineer › <Screen>` + user@host), bordered body panels, footer (key hints or an auto-expiring notification tile). See `README.md` "Chrome conventions" — match these exactly.
- **Widgets:** status pills (black ink on semantic fill: ` reading `, ` done `, …), the block progress bar (`███▍·····  42%`), full-row inverse selection with a `▌` marker, notify tiles (info/success/warn/error).
- **Screens:** Login, Home (today's activities + reading list), Books list (**the anchor mockup**, `books.html`), Book detail (chapters), New-activity form (multi-field, insert/normal modes), and Progress (weekly pace meters with `[`/`]` week navigation — shipped mid-epic as [engineer-cli#15](https://github.com/dsaenztagarro/engineer-cli/issues/15)). The five new screens should look like they were always part of this family.
- **A minimal `:` line already exists** — `:q`/`:quit`, `:home`, `:books`, `:activity`, `:logout`, `:logs`, `:w` — a dispatcher, not a designed surface. The grammar you pin extends it; the shipped Progress screen deliberately has *no* palette entry yet (`:progress` waits on this grammar).
- **Screens are being built in parallel.** Per the epic's process decision, the screen tickets (#9–#14) proceed on the kit conventions (README + `books.html` + `terminal-tokens.css`) without waiting for these mockups — the #15 Progress screen shipped faithfully that way. Your mockups are the **refinement pass**: treat any screen already shipped by the time you design as current-state input to improve on, not a blank canvas.
- **The API client is complete** for everything this brief covers: timer (start/pause/resume/stop/bind/candidates/discard), notes CRUD + book anchors, activities CRUD + member actions, review (dashboard/browse/rate/sessions). Pagination metadata exists but no screen exposes it yet — the activities table is where it first matters.
- **The server contracts are real, not aspirational.** `GET /api/v1/progress` is shipped (the Progress screen consumes it today); `GET /api/v1/weeks/:iso_week` and the today's-plan data ship as [engineer#662](https://github.com/dsaenztagarro/engineer/issues/662); the `/api/v1/today` aggregate (one call for the Home/today data), ETag-cheap timer polling, and quick-capture note ergonomics are upcoming under epic engineer#652. Design as if data arrives promptly; don't design loading theatre.
- **IA seeds from the web** (information architecture only, not layout): the web `activities.html` (table + filters + row affordances), `review.html` (dashboard triage, session flow), `command-palette.html` (verb categories). The web command palette's *categories* may inform the `:` grammar's shape.

---

## 5. The screens (framed as problems, with room to solve them your way)

One mockup per file, kit convention. Solve, in whatever layout reads best on the grid:

- **`timer.html` — time, at hand.** Two problems in one file: (a) the **header cell** on every screen — how running / paused / absent read at a glance in a few characters; (b) the **timer screen** — the bind moment (search candidates, or mint a new activity from a title), pause/resume, and the stop moment (what got written: activity, minutes). Show the blank-timer flow ("clock first, name it later") — it's the honest way sessions actually start.
- **`notes.html` — five-second capture, findable later.** The quick-capture overlay/flow reachable from anywhere; the browser (list, `/` search, detail, archive); and the one-line anchor read-back (a note pinned to *SICP · ch 3 · p.142* must say so in one row). Show what an unanchored "loose thought" looks like next to an anchored one.
- **`activities.html` — the core table.** Today + recent activities in a dense, scannable grid: status/kind at a glance (pills), duration, domain colour semantics, row actions (complete / archive / duplicate), a detail read, and — first in the app — **pagination** that feels native to a TUI (this is yours to shape: pages, continuation, counts).
- **`review.html` — the sitting.** The dashboard read (due counts, urgency, streak — the web's heatmap language only if it earns its place in character cells); the queue → rate → next loop with the four ratings as keystrokes; the clean exit. Browse-all-topics is a secondary state of this screen, not a separate ceremony.
- **`command-palette.html` — the `:` grammar.** The command line's states: empty (what's possible), partial (completion), unknown (helpful, not hostile), and executing. The **verb inventory is pinned** — navigation `:home` · `:books` · `:activities` · `:notes` · `:review` · `:progress` · `:timer` (bare verb goes to the screen); actions `:timer start|pause|resume|stop` and `:note <text>`; housekeeping already shipped (`:q`/`:quit`, `:logs`, `:w`, `:logout`). What's yours to design: completion, the argument shapes (e.g. how `:note` takes an anchor), abbreviations, the unknown-verb response, and how the line's states read — since every other screen's footer hints will reference this grammar.

Each mockup should carry the shared chrome (header with the timer cell, footer hints) so the family reads as one product.

---

## 6. Visual language (this is a hard constraint — do not drift)

Anchor on **`books.html`** — it is the style template; everything must look like it belongs in the same terminal.
Render against **`terminal-tokens.css`** exactly: dark-first (`#05080F` / `#E6EBF2`), the **indigo-light accent** (`256 #105 / #8787FF` — the kit's ratified brand hue, not the shipped sky-blue drift), semantic success/warn/danger as mapped, borders in `240`.
Box-drawing panels with accent-bold titles; status pills, `▌` selection, block progress bars from the shipped widget vocabulary.
One font, one size; ASCII/sparse-unicode glyphs only (`●`/`○` status dots); no imported web chrome.
The `README.md` palette table and translate/don't-translate rules bind in full.

---

## 7. Out of scope

The shard/environment indicator chrome and the roadmaps + book-progress screen (both on the kit's inventory as separate roadmap items); editing books/roadmaps in the terminal (the web owns editing depth); mouse support; offline mode and local caching; desktop notifications; theming/light mode; any change to `books.html`, `terminal-tokens.css`, or the shipped Rust palette (the accent decision is ratified in the kit, adopted in code separately).

---

## 8. Where the detailed model lives (so you don't have to invent it — but also aren't bound by it)

EPIC [engineer-cli#7](https://github.com/dsaenztagarro/engineer-cli/issues/7) holds the engineering shape — the screen tickets (#9–#14, with #13 implementing the `:` grammar pinned in §5) and the server companions ([engineer#662](https://github.com/dsaenztagarro/engineer/issues/662), [#663](https://github.com/dsaenztagarro/engineer/issues/663), [#664](https://github.com/dsaenztagarro/engineer/issues/664), [#665](https://github.com/dsaenztagarro/engineer/issues/665)).
Intentionally not part of this handoff.
If your design implies a cleaner shape (a different screen split, a better grammar), say so — that feedback updates the plan.

## 9. How to use this brief

Produce the five mockups in the kit's visual language (§6), solving the jobs in §2 under the principles in §3, in the `engineer-cli` Claude Design project seeded per `README.md`.
Iterate on the mockups until they're right; the screen tickets turn them into Rust afterward.
