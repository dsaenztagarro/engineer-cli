# Design brief — engineer-cli · Week planning & retrospect (the Plan pillar — declare the week, read planned-vs-done)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal week-planning board and its headless twins — the **plan side** (declare what to study this week) and the **retro** (a planned-vs-done readout plus a free-text reflection that opens in `$EDITOR`). Extend `../../design-system.dc.html` (the style anchor); a `week-planning.dc.html` board is the natural home for the mock, mirroring `timer.dc.html` and the `progress.dc.html` the Progress brief calls for.
**Status:** **shipped.** (epic #113 → v0.9.0; headless readout #89 → v0.7.0)
The Plan pillar is live: the headless `engineer week` / `engineer plan` readout and one-liner shipped first (#89), then the board — the planned-vs-done screen (`src/app/screens/week.rs`), declaring/adjusting/dropping plan items from it, the Plan↔timer seam (`s` starts the timer on a planned item), and the `$EDITOR` retro reflection persisted through the v1 week-note route (`src/api/weeks.rs::update_week_note`). This brief is kept as the module record; the server gap in §5 is now closed (see the RESOLVED note there).

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the plan-and-retrospect slice of the retired omnibus (`terminal-client.brief.md` job 9) plus the ground truth verified against the shipped `engineer` API. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "On Monday I want to say what the week is *for* — 'ship the timer epic, two SICP chapters, one systems paper' — as a short list of intents, from the terminal, without opening the web app.
> Through the week I want the timer to bind to those planned items, so the plan and the actuals are one ledger, not two things I have to reconcile.
> And on Friday I want the honest readout: what I said I'd do vs what the segments say I did — `SICP done · systems 2 of 3 · that paper never happened` — and one place to write *why*.
> Not a cramped text box in a TUI: `$EDITOR`, the way I write a commit message. Type the reflection, save, quit, done."

The throughline: **the plan and the retro are two faces of one ISO week.** The plan is *declared* — a planned activity is a plan item — and the retro is *derived* (planned-vs-done read straight from the segments) with exactly one piece of stored prose, the week note. The terminal's job is to make declaring the week a keystroke, the retro a glance, and the reflection an `$EDITOR` hand-off — not to reinvent a text editor in ratatui, and not to keep a second copy of the plan the server already owns.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**9a. Declare the week (the plan side).** Say what to study this week as a short list of planned activities — declare, adjust, and drop plan items for an ISO week, from the command line and from the board, with near-zero ceremony. *Not built.* Planning writes **reuse the activities API** — a planned activity with `planned_on` set *is* a plan item — so this is a new surface over a largely-existing write client, not a new endpoint (see §5).

**9b. Retrospect the week (planned-vs-done + a reflection).** A planned-vs-done readout for the week — each plan item as done / partial / untouched against the actuals — plus a free-text reflection the user writes in `$EDITOR` and that persists to the week's note. *Not built.* The readout is **derived** (read-through from the week aggregate); the reflection is the module's one stored write — and, today, its one *blocked* write (see §5).

*Integration seam — "Start binds the timer to a planned item."* Starting the timer on a plan item is the join between this module and the timer module; `timer.brief.md` §D lists it as an adjacent surface deliberately **not** folded into `timer.dc.html`, to be designed here where the plan list lives.

---

## 3. Principles that genuinely bind

- **A readout and a one-liner, not a planning canvas (the governing principle).** Per `cross-cutting.brief.md` — sterling, not a replica — the terminal week is three watch-native things: a glanceable **planned-vs-done readout** (`SICP done · systems 2 of 3 · that paper never happened`), a **one-gesture "start on a planned item"** (the timer seam, §2), and **declaring a plan item as a one-liner** (an `engineer plan add '<title>'`-shaped verb, reusing the activities create). Arranging and re-shaping the week — the web's drag-the-plan canvas, copy-week-forward, calendar mutations (§7) — stays on the web. The board *shows* the plan, lets you add to it, and lets you act on it; it is **not** a planner you assemble cell-by-cell in a TUI. Design the readout and the add-verb first; anything that starts to feel like a web form is the signal to stop and leave it on the web.
- **Derived, never stored (the actuals half).** Planned-vs-done and pace come read-through from `GET /api/v1/weeks/:iso_week`, recomputed server-side from segments on every read. The board renders them; it keeps no second ledger. The plan items themselves are real activity rows written through the activities API — not a client-side plan list — and the reflection is the only prose this module stores.
- **One clock.** ISO week ids are Monday-first `YYYY-Www`; week attribution uses engineer's 4 AM study-day boundary. Week stepping `[` / `]` and `t` → this-week must agree with the Progress screen and the web to the minute — the Progress screen already speaks this dialect (§4), so reuse it rather than re-deriving the boundary.
- **`$EDITOR` for prose — the git-commit pattern.** Long-form reflection opens in the user's `$EDITOR` (write → save → quit → persist), never a ratatui text box. This is a cross-cutting rule shared with note capture; hold it here and cross-reference `cross-cutting.brief.md` and `notes.brief.md` rather than re-litigating it. The retro *readout* is a screen; the retro *writing* is a hand-off.
- **Planning writes reuse the activities API.** A plan item **is** a planned activity with `planned_on` set — declaring and adjusting the week go through `POST` / `PATCH /api/v1/activities`, not a new week-plan endpoint. This is what keeps the plan and the actuals one ledger (the derived principle above), and it means most of the write client already exists (`src/api/activities.rs`).
- **TUI ↔ headless duality is first-class.** As in every module, each read the board shows must also exist as a non-interactive one-shot (`--json` for machines, plain-text for pipes), and declaring the week gets a headless verb — TTY-detected, `NO_COLOR`-respecting, no ANSI when piped. Bound where the server allows it: the reflection *write* is currently blocked (§5), so only its read has a twin until the route lands.

---

## 4. What's already in the app (orientation)

- **Adjacent, shipped:** the Progress screen (`src/app/screens/progress.rs`) — same ISO week, pace meters, week stepping `[` / `]`, `t` → this-week, the `by_day` sparkline — is the nearest neighbour and the dialect the retro's readout should speak. The Activities table (`src/app/screens/activities.rs`) already lists activity rows, the substrate a plan is made of.
- **Write client to reuse:** `src/api/activities.rs::create_activity` (with `ActivityCreate`) already writes activities and already round-trips `status: "planned"` — so plan-declare is largely this client. **Gap:** `ActivityCreate` has no `planned_on` field yet (it carries title / domain / kind / duration / `started_at` / `book_ids`), so turning a create into a *plan item for a given week* needs that one field added.
- **What genuinely does not exist:** the week-planning board/screen and its empty state; a weeks API client (`src/api/` has no `weeks.rs`); the `GET /api/v1/weeks/:iso_week` read; the planned-vs-done readout widget; the plan-declare verbs; and the `$EDITOR` reflection flow — plus the server route to persist it (§5).

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

The **read** is shipped server-side; the **plan writes** ride the shipped activities API; the **retro reflection write** has no v1 route yet — this module is part CLI-only work, part blocked on the server, and the brief keeps the two apart.

- **Week read** — `GET /api/v1/weeks/:iso_week` (`routes.rb:428`, `weeks#show`). One derived aggregate for a single ISO week: the plan slice, the actuals, the pace fold, the note, and the retro band, rendered for CLI/MCP consumers (route comment; `week-planning.html §G` on the web). **Read-only** — the route comment is explicit: *"planning writes go through the activities API (a planned activity + `planned_on` = a plan item)."* No weeks client exists yet; this is the first read to wire.
- **Plan writes** — the activities API, no new endpoint. `POST /api/v1/activities` declares a plan item (an activity with `planned_on` for the target week); `PATCH /api/v1/activities/:id` adjusts it; `member { patch :complete }` closes a plan item as done (`routes.rb:338–352`). This is the reuse the week-read's route comment mandates.

### Server gap — no v1 route persists the retro reflection (raise it, as timer's §C once was)

> **RESOLVED (epic #113 → #117).** The v1 route shipped — `PATCH /api/v1/weeks/:iso_week/note` with `{ "note": { "body": … } }`, returning the bare persisted note `{ iso_week, body, updated_at }` (dsaenztagarro/engineer#805, engineer PR #807) — exactly the ask below, mirroring how the timer-hygiene endpoints unblocked the timer module.
> The CLI now persists the reflection through it: `src/api/weeks.rs::update_week_note`, routed through `QueuedClient` (`IntentKind::WeekNoteWrite`, stream `week:<iso_week>`, replayed as a plain idempotent PATCH), driven by the board's `i` (`$EDITOR`, the git-commit pattern) and the headless `engineer week reflect`.
> An empty body is a deliberate clear (the `week_notes` contract treats empty as clear), kept distinct from a quit-without-write abort (capture-is-sacred). The original gap analysis is kept below as the record of what drove the design.

The reflection is meant to open in `$EDITOR` and save to the week's note. The server *does* have a week-note write — `patch "weeks/:iso_week/note"` → `week_notes#update` (`routes.rb:129`) — but it lives in the **top-level web namespace, not `api/v1`**: it is the Turbo/HTML autosave behind the web retro band's textarea (`week-planning.html §E`), not a token-authenticated JSON endpoint the CLI can call. Inside `namespace :api { namespace :v1 }` there is **no** week-note write: `weeks/:iso_week` is `GET`-only (`routes.rb:428`), and the v1 `resources :notes` (`routes.rb:365`) are the standalone reading-notes resource — a different thing from the per-week retro line.

So, exactly as the timer's §C once stood: **the CLI can render the retro but cannot persist the reflection via v1 yet.** Raise this as a server ask — a v1 `PATCH /api/v1/weeks/:iso_week/note`, or folding the note write into the aggregate — analogous to the timer-hygiene endpoints (`POST /api/v1/timer/{reclaim,phase,mode,heartbeat}`) the server later shipped to unblock the timer module. Until it lands, the reflection is read-and-display only. Don't design around the gap silently — §8 phases the write behind it.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus cited no longer exist). Assemble from shipped atoms only: `bordered()` panels, the `progress_bar`/`pace_bar` block meters, `status_pill`, full-row inverse selection with `▌`, `notify` tiles. The plan list reuses the **Activities table grammar**; the planned-vs-done readout reuses the **pace/progress meter block** plus status pills (` done ` success, ` hold ` warn, muted for untouched). ASCII-only diagrams; keyboard-only, neovim-flavoured; the footer advertises the active keys (including week stepping `[` / `]` and `t`, shared with Progress). The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see the cross-cutting brief.

---

## 7. Out of scope

Editing actuals/segments here (that's the activities/audit modules); the pace meters and the weekly-target lifecycle themselves (that's `progress.brief.md` — this board *reads* pace, it doesn't own the targets that define it); a ratatui rich-text editor for the reflection (that is exactly what the `$EDITOR` hand-off avoids); the web's drag-the-plan canvas and copy-week-forward affordances (`routes.rb:115–122`, the calendar plan mutations) — the terminal declares plan items as activities, it does not reimplement the web planning canvas; light mode.

---

## 8. Phasing

1. **Design pass first — `week-planning.dc.html`.** Unlike progress and timer, this module has no mock to start from, so it can't open with a gap analysis. Do the design pass (plan side + retro readout + the `$EDITOR` reflection moment + empty states) → a gap brief → an epic, the repo's established `*.dc.html` → brief → epic pattern. Sequence it **after** the higher-leverage progress (targets-write) and home slices.
2. **Week read + retro readout (read-only, unblocked).** Wire the weeks client (`GET /api/v1/weeks/:iso_week`), render the planned-vs-done board and its headless `--json`/plain twin. Grounds fully on the shipped read.
3. **Plan writes — declare the week (unblocked).** The plan-declare/adjust/complete verbs + board affordances, reusing `src/api/activities.rs::create_activity` (add `planned_on` to `ActivityCreate`). Grounds on the shipped activities API. The timer's "Start binds the timer to a planned item" seam (§2) lands against these plan items.
4. **The `$EDITOR` reflection — behind the server gap.** Design the write flow now (open `$EDITOR`, persist to the week note), but it is **blocked on a v1 week-note write route** (§5). Ship the render side in phase 2; land the write when the server exposes the v1 route, mirroring how the timer's §C gaps unblocked once `engineer` shipped the timer-hygiene API.
