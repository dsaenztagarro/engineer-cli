# Design brief — engineer-cli · Home, enriched (today at a glance — timer and pace leading, not a list)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal Home screen, rebuilt to *open* with the live timer and this week's pace and to fold its two-call load into one aggregate read — plus the net-new `src/api/today.rs` client on `GET /api/v1/today` it needs. Extend `../../design-system.dc.html` (the style anchor); a `home.dc.html` board is the natural home for the mock, mirroring `timer.dc.html`.
**Status:** proposed. A single unblocked slice — the server aggregate is shipped and unused, so the work is pure-CLI.

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries job 11 of the retired omnibus (`terminal-client.brief.md`, "Home, enriched") plus the daily loop's opening question — "check what I meant to do today" — verified against the shipped `engineer` API. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "When I sit down to study I open the terminal, not a browser — and the first thing I want isn't a list, it's the two things that decide what I do next: is a timer already running (did I leave one going, and on what?), and am I keeping this week's promises.
> `● systems 24:13` up top, then `systems ███▍···· behind 1.8h` — before I've read a single row.
> Under that, quietly: what did I actually *mean* to do today — the plan slice, what's left, how many minutes I've logged — and the books I'm mid-chapter in.
> I don't want four screens' worth of API calls for that; I want one read that opens the day."

The throughline: **Home is the daily loop's opening question.** Today it answers with a flat list and leads with neither ambient read. The enrichment is to lead with the timer and the pace — the two reads the rest of the app already owns — and to serve the whole screen from the one aggregate the server built for exactly this. Nothing here is blocked; it's a rebuild of a shipped screen against an endpoint it doesn't yet call.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**11. Open the day with the timer and the pace, not a list.** Home's top band leads with the running timer — the same status grammar the header cell already speaks — and this week's pace fold (the worst-behind target named by scope, how many trail), the two ambient reads that decide the next move. Today's plan and activities sit *beneath* that, not above it. Shipped Home renders a today's-activities table over a reading list and leads with neither. **This is the open slice.**

**(the daily-loop read) Check what I meant to do today.** Under the lead: today's plan slice (planned | live | done | left, what's left, logged-vs-planned), today's logged minutes, the review due-counts, and the books I'm mid-chapter in with where-I-am — the composed daily-loop payload, rendered in one pass instead of N per-resource calls.

---

## 3. Principles that genuinely bind

- **Derived, never stored.** `/today` *composes* the derivation that already owns each number (the week story, the timer serializer, the review dashboard counts, `next_unread_chapter`, the pace fold, today's segment sum) — it re-derives nothing, so it can never disagree with the per-resource endpoints. The client renders it and keeps no second ledger. **This module owns no write.**
- **One clock.** `date.day` and `date.week` come under `engineer`'s 4 AM study-day boundary and Monday-first ISO week, computed server-side — so Home agrees with the header cell and the Progress screen to the minute, and the client stops doing its own day-window math. Week ids are `YYYY-Www`.
- **Quiet by default; `behind` is as loud as it gets.** The `pace` block is `null` when nothing is behind — *silence is the on-pace state*, baked into the API, not a client choice. Home must render that silence as calm, not as an empty panel. There is no red pace state (`met` / `on_pace` / `behind`); behind wears a small warn signal, matching Progress.
- **The ambient timer cell is already the header atom — lead with it in the body too.** `widgets::timer_cell` speaks the full status-line grammar and is drawn on every screen's header by `layout.rs::render_chrome` (returning `None` for a clean header when nothing runs). Home's lead band is the *same atom, larger* — one timer grammar everywhere, not a second rendering.
- **Additive-only contract (ADR 0027).** The payload evolves by adding keys; existing keys never change meaning, and consumers must ignore unknown ones. So the client reads `date.day`, not the deprecated `study_day` alias (renamed by ADR 0032), and `serde`-defaults the rest so it survives the payload growing under it.

---

## 4. What's already in the app (shipped reality)

- **Shipped Home (`src/app/screens/home.rs`):** a today's-activities `Table` over the top ~55% (`TIME` / `KIND` / `DUR` / `TITLE`, titled "Today · N min logged") above a "Currently reading" `List` of `progress_bar` rows over ~45%. Its `spawn_load` makes **two** calls — `list_activities` over a *client-computed* today window (via `jiff`) plus `list_books(Reading)` — leads with neither the timer nor the pace, and never touches `/today`.
- **The ambient timer cell already exists.** `ui::widgets::timer_cell` renders the full status-line grammar (running `●` / paused `‖` / idle `◐` / focus `◆` / break `○` / over), and `ui::layout::render_chrome` draws it right-aligned in every screen's header (`None` ⇒ clean header). Home's lead band reuses this atom rather than inventing a timer face.
- **The pace read already exists.** `src/api/progress.rs::get_progress` → `GET /api/v1/progress` returns the per-target readings, `PaceState` (`met` / `on_pace` / `behind`, no red), and `by_day`, wired and tested for the Progress screen (see `progress.brief.md`). Home needs only the *pre-folded* worst-behind, which `/today` carries — so it can lead with pace without duplicating that screen's meter set.
- **Widgets to reuse:** `bordered()` panels, `progress_bar` (`███▍····· 42%`), `pace_bar` (the now-tick meter), `status_pill` (` reading `), the `Table` grammar the activities list already uses, and `notify` tiles.
- **What does not exist yet:** any `/today` client — there is **no** `src/api/today.rs`, and `src/api/mod.rs` wires none — and the enriched Home layout (a timer + pace lead over the daily-loop blocks) and its states.

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

The aggregate is **shipped server-side and simply unused by the CLI** — so this module is CLI-only work, not blocked on the server. The controller says so in as many words: `GET /api/v1/today` exists as "ONE composed payload … the TUI Home renders it in a single pass instead of N paginated calls (engineer-cli#11 deferred exactly this)."

- **The aggregate** — `GET /api/v1/today` (`routes.rb:434`; EPIC #652, ADR 0027). One read-only, unpaginated, un-feature-flagged payload. Server: `app/controllers/api/v1/today_controller.rb`, view `app/views/api/v1/today/show.json.jbuilder`. Every block *composes* the derivation that owns it, so `/today` can never disagree with the per-resource endpoints. The blocks:
  - `date` — `{ day (ISO date under the 4 AM boundary), weekday, week (YYYY-Www) }`. (`study_day` is a deprecated alias of `day` — ADR 0032 — read `day`.)
  - `plan` — `{ items: [{ id, title, status, state: planned|live|done|left, size_minutes, logged_minutes, moved_from }], left_count }` — the `WeekStory` today slice, the same items the week canvas reads; empty `items` ⇒ nothing planned today.
  - `timer` — the `api/v1/timers/_timer` partial, **byte-identical to `GET /api/v1/timer`** (`{ running: false }` when idle) — so `src/api/timer.rs::Timer` decodes it verbatim and the new client reuses that struct, never a second timer shape.
  - `review` — `{ due_count (includes stale), stale_count, est_minutes }` — triage counts only; the queue itself stays on `GET /api/v1/review/dashboard`.
  - `reading` — `[{ id, title, progress_percent, chapters_total, next_chapter: { number, title } | null }]`, most-recently-touched first — a superset of what Home's reading list shows today (it adds the where-you-are chapter).
  - `pace` — `{ behind_count, worst: { target_id, axis, scope_value, scope_name, delta_minutes } }` **or `null`** — the pace-chip fold: the single worst-behind target named by scope plus how many trail; `null` when nothing is behind (silence = on-pace). The full per-target readings stay on `GET /api/v1/progress`.
    > **Gap surfaced by epic #61 (Home pace fold, #65).** `worst` carries only `delta_minutes`, so Home renders the fold as a **chip summary** — `scope_name` + ` behind Xh ` + `N targets trailing` + `g p → Progress` — and does **not** draw the small now-tick meter the `home.dc.html` mock shows (that meter needs the fill / expected-now / target for the worst target, which this block doesn't carry). This matches the design's own §Home·behind caption ("`g p` for the full meters"). If a single inline meter on Home is wanted, add `fraction` + `now_fraction` (or `actual_minutes` / `expected_minutes` / `target_minutes`) to `worst` — additive per ADR 0027, and the client already tolerates their absence.
  - `totals` — `{ logged_minutes }` — completed segment minutes on today's day, plan-agnostic (unplanned work counts).
- **What it replaces** — the two calls shipped Home makes today: `resources :activities` (`routes.rb:338`, `list_activities` over a client-computed window) and `resources :books` (`routes.rb:354`, `list_books(Reading)`). Both fold into `/today`'s `plan`/`totals` and `reading` blocks, and the client-side day-window math is deleted — the server owns the boundary.
- **Still available for depth** — `GET /api/v1/progress` (`routes.rb:415`) for the full per-target meters when Home's pace lead invites a drill-down to the Progress screen. The `pace` fold on `/today` is the summary line, not a replacement for that screen.

**New client to build:** `src/api/today.rs` → `GET /api/v1/today`, modeled on the jbuilder above — reusing `api::Timer` for the `timer` block, and `Book` where the `reading` shape fits — and wired into `src/api/mod.rs` beside `progress.rs`. This is the only net-new code the module needs.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus cited no longer exist). Assemble from shipped atoms only: `bordered()` panels, the `timer_cell` status grammar as the lead band, `pace_bar`/`progress_bar` meters, `status_pill`, the `Table` grammar for the plan and activities rows, `notify` tiles. Keyboard-only, neovim-flavoured; the footer advertises the active keys (`r` refresh, `a` add, `↵` open, and the `g`-nav to the Timer / Progress / Review surfaces the blocks point at). ASCII-only diagrams. The pace lead must render on-pace silence as *calm* — no red state — while behind wears the small warn signal, matching Progress. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see the cross-cutting brief.

---

## 7. Out of scope

- **No writes.** Home reads; starting/stopping the timer lives in `timer.brief.md`, declaring/adjusting a target in `progress.brief.md`, logging in the activities module. Home's blocks *link out* to those surfaces — they don't own the verbs.
- **The deep versions of what Home folds:** the full per-target pace meters (the Progress screen), the full review queue (the Review screen), and the week retro band (`GET /api/v1/weeks/:iso_week`, week-planning). Home shows the fold and points at them.
- **A headless `engineer today` one-shot.** The `src/api/today.rs` client makes it cheap and the house's TUI ↔ headless duality invites it, but this slice is the TUI Home; the one-shot is an obvious follow-up, not the first cut.
- Light mode — never.

---

## 8. Phasing

1. **The single unblocked slice — `/today` client + enriched Home.** Add `src/api/today.rs` on `GET /api/v1/today` (reusing `api::Timer`), then rebuild `src/app/screens/home.rs` to lead with the timer band and the pace fold and to render the plan / activities / reading blocks beneath from the one payload — deleting the two-call `spawn_load` and its client-side day-window math. Fully API-supported, no server work. Track as its own epic (the repo's pattern: a `home.dc.html` design pass → this brief → epic, as `timer.dc.html` + timer did).
2. **(growth, not this slice)** the headless `engineer today` twin the same client unlocks, and the block drill-downs (Home → Progress / Review / Timer) where they don't already fall out of the existing `g`-navigation.
