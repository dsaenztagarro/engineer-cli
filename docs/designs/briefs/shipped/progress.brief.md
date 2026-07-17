# Design brief — engineer-cli · Progress & Targets (the Measure pillar)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal Progress screens *and* their headless twins — the one-line pace meters, the `engineer target …` verbs that declare/adjust/retire a weekly target, and the time-explorer pivot. Extend `../../design-system.dc.html` (the style anchor); a `progress.dc.html` board is the natural home for the mock, mirroring `timer.dc.html`.
**Status:** **shipped.** (meters v0.2–v0.4; targets-write + declare #85/#86 → v0.7.0; queue-aware writes, the where-it-went fold + :target verb epic #119 → v0.9.0)
The Measure pillar is complete in the terminal. The pace meters shipped first (`src/app/screens/progress.rs`), then targets-write — the `engineer target` headless verbs (`src/target_cli.rs`), the pace headless twin, and interactive **adjust / retire** on the Progress screen (v0.7.0) — and epic #119 closed the rest: interactive **declare** in place (the shared fuzzy picker as the scope chooser, #121), queue-aware target writes, the §Where it went by-domain/by-intent **fold** plus the `engineer progress --json` rollup (#122), and the deferred `:log`/`:target` **palette verbs** (#97, this pass — the epic's final surface). This brief is kept as the module record; the two §8 deferrals are now resolved (see the RESOLVED notes there), and the one residual is the by-domain / by-intent facet, which waits on a server rollup — the ask is filed (dsaenztagarro/engineer#810).

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the Progress slice of the retired omnibus (`terminal-client.brief.md` jobs 5–7) plus the ground truth verified against the shipped `engineer` API. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "When I'm studying I don't want a dashboard — I want to know, in one glance, whether I'm keeping the promises I made myself this week.
> `systems ███▍···· 4.2/6h · behind 1.8h`, one honest line per target, is the whole payoff.
> I want that same line in my zellij status bar and out of `--json` so a script can nag me, not just the TUI.
> And when the week's shape is wrong — I set 6h of systems but I've been coasting — I want to *fix the promise from the terminal*: declare a new target, bump the hours, retire one I've outgrown, without opening the web app.
> Once a week I want to see where the time actually went — by domain, by kind, by intent — so next week's promises are honest."

The throughline: **pace is the terminal's promised superpower** — the web Progress brief explicitly designed it to "survive being a line of text per target." The remaining gap is that the promises themselves (the targets) are still read-only in the terminal: you can *see* your pace but not *adjust the target* that defines it. Closing that — plus the once-a-week pivot — completes the Measure pillar in the terminal.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**5. Read pace as one-line meters.** Per weekly target: done-vs-intended and ahead/behind, as ASCII meters. *Shipped* as a TUI screen (`progress.rs`); the **headless twin is still missing** — there is no `engineer progress`/`engineer pace` one-shot with `--json` and a piped-plain form to feed a status bar or script, the way `engineer timer` already has one. Design that output shape with the same care as the screen.

**6. Adjust targets in place.** Declare, adjust, and retire a weekly target from the command line (`engineer target …`) *and* from the Progress screen, with near-zero ceremony. **This is the open, first-to-build slice.** The empty state must teach it: today the screen says "declare a weekly intent in the web app" — that pointer becomes a keystroke.

**7. See where the time went — a glance and a rollup, not a pivot table.** The web has a full pivot/explorer; the terminal answer is *distilled* (the governing principle, `cross-cutting.brief.md`). It is two things: a **glance** — the shipped kind-mix line (`kind mix  coding 3.0h · reading 2.5h`) extended to a toggleable "this week by domain / by intent" fold — and a **`--json` rollup** you pipe when you want to slice it yourself. It is explicitly **not** a TUI pivot grid with axes, periods, and pagination — that is a lean-back web surface that fails the glance-or-gesture test, and it stays on the web. *Not built; lowest priority — the one-look answer to "where did this week go" may already be enough.*

---

## 3. Principles that genuinely bind

- **Derived, never stored.** Pace and rollups are read-through from `GET /api/v1/progress`, recomputed server-side from segments on every read. The client renders them; it never keeps a second ledger. Writing a *target* is the only write this module owns — and even then actuals stay derived.
- **Quiet by default; `behind` is as loud as it gets.** There is deliberately **no red pace state** (`PaceState` is `met` / `on_pace` / `behind`). On-pace shows a calm ✓; behind shows a small warn-coloured signal; nothing nags. Hold this in every new surface, including the headless string.
- **TUI ↔ headless duality is first-class.** Every read the screen shows must also exist as a non-interactive one-shot with `--json` (machine) and plain-text (pipe) forms — TTY-detected, `NO_COLOR`-respecting, meaningful exit codes, no ANSI when piped. The `engineer target` verbs and the pace one-shot are both bound by this.
- **One clock.** Week attribution uses engineer's 4 AM study-day boundary and Monday-first ISO week; a meter and a status string must agree with the web to the minute. Week ids are `YYYY-Www`.
- **A target is a promise, not a record you delete.** See §5's decision record — retire ≠ delete.

---

## 4. What's already in the app (orientation)

- **Shipped:** the Progress screen (`src/app/screens/progress.rs`) — one `pace_bar` meter row per target (behind-first, largest-gap-first, with a now-tick), the week header (`2026-W27 · fri · day 5 of 7 · now = 57%`), a behind-total footer, the kind-mix line, and a THIS-WEEK sparkline the timer rail reuses from `by_day`. Week stepping `[` / `]`, `t` → this week. The read client is `src/api/progress.rs::get_progress` → `GET /api/v1/progress`, wired and tested.
- **Widgets to reuse:** `ui::widgets::pace_bar` (the now-tick meter) and `progress_bar` (`███▍····· 42%`), `bordered()` panels — the pace meters you need are largely this widget set aimed at time.
- **Read model to reuse:** `src/api/progress.rs` already defines `TargetRef` and `Scope` (id, axis, scope{axis,value,domain}, hours_per_week, active, retired) — the exact shape the targets-write endpoints return, so the new client reuses these structs.
- **Shipped this pass (targets-write):** the `src/api/targets.rs` client (list/create/update/retire); the `engineer target` headless verbs (`src/target_cli.rs` — `list`/`declare`/`adjust`/`retire`, `--json`, plain-pipe, exit 0/1); and on the Progress screen, a target-row cursor (`j`/`k`), **`e` to adjust the selected target's hours in place**, **`x` to retire (confirmed on a second press)**, and a teaching empty state pointing at `engineer target declare`.
- **What still does not exist yet:** **interactive declare** in the TUI (it needs a scope picker — a domain/kind/intent chooser — which waits on the shared fuzzy picker in `cross-cutting.brief.md`; declaring is fully available via the headless verb meanwhile); the headless pace one-shot (job 5's twin); the time-explorer pivot (job 7).

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

The pace read and the full target lifecycle are **both shipped server-side** — this module is CLI-only work, not blocked on the server. (This corrects the omnibus brief, which guessed targets was "the one net-new client this brief implies"; the *server* endpoints exist, only the CLI face is missing.)

- **Pace read** — `GET /api/v1/progress` (`routes.rb:415`). One derived object: the ISO week, one reading per active target, kind-mix, Bloom, totals, `by_day`. Consumed today.
- **Targets** — `resources :targets, only: %i[index show create update]` + `member { patch :retire }` (`routes.rb:412-414`). Server: `app/controllers/api/v1/targets_controller.rb`, view `app/views/api/v1/targets/_target.json.jbuilder`.
  - `GET /api/v1/targets?state=active|retired|all` — one row per lineage (default `active`).
  - `POST /api/v1/targets` — **declare**. Body `{ "target": { "axis": "domain|kind|intent", "hours_per_week": <num>, "<scope>": <val> } }` where `<scope>` is `domain_id` (int), `kind`, or `intent` matching the axis. → `201` with the target object.
  - `PATCH /api/v1/targets/:id` — **adjust hours**. Body `{ "target": { "hours_per_week": <num> } }`. Returns the **live row, whose `id` may differ** (an edit past the same day mints a successor version).
  - `PATCH /api/v1/targets/:id/retire` — **retire**. No body. Closes the lineage.
  - Target object shape (both `show` and `index` rows): `{ id, axis, scope: { axis, value, domain?: {id,name} }, hours_per_week, active, retired, active_from, active_until, retired_at, created_at, updated_at }` — a superset of `TargetRef`; serde ignores the extra timestamp fields.

### Decision record — targets are append-only versions (ADR 0026)

Two server invariants must shape the CLI verbs, not be worked around:

- **Retire ≠ delete.** There is deliberately no `DELETE`; `PATCH :retire` closes a lineage while keeping its history so past weeks still read it. So the CLI verb is `engineer target retire`, never a destroy, and the confirmation copy should say "retire (keeps history)," not "delete."
- **Adjust may mint a successor.** `PATCH :id` returns the live row, whose `id` can differ from the one addressed; a stale/closed version id is a `422` (`Target::ClosedVersionError`). So the CLI **addresses a lineage by axis + scope, not by a cached id** — after an adjust, re-read rather than trusting the old id, and surface the 422 as "this target moved on; re-fetch and retry," not a hard error.

These are the same two decisions the omnibus asked to "raise, not silently resolve." They are resolved here, in the server's favour.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus cited no longer exist). Assemble from shipped atoms only: `bordered()` panels, the `pace_bar`/`progress_bar` block meters, `status_pill`, full-row inverse selection with `▌`, `notify` tiles. Keyboard-only, neovim-flavoured; the footer advertises the active keys. ASCII-only diagrams. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see the cross-cutting brief.

---

## 7. Out of scope

Editing actuals/segments here (that's the activities/audit modules); billing/rates/invoice export beyond generic `--json`; a stored client-side ledger (pace is derived); light mode. The pivot's deepest cross-tabs beyond domain/kind/intent/anchor wait until the base pivot earns them.

---

## 8. Phasing

1. **Targets-write (job 6) — shipped.** The `engineer target list|declare|adjust|retire` headless verbs and the Progress-screen interactive **adjust / retire** landed on the shipped `/api/v1/targets` routes above. Two deliberate deferrals were recorded rather than half-built; both are now **resolved** under epic #119:
   - **Interactive declare — RESOLVED (#121).** It shipped on the shared **fuzzy picker** as the scope chooser (choosing a domain/kind/intent is exactly the "fuzzy over navigate" widget the cross-cutting brief scoped). The shipped flow is deliberately *one* picker over every scope at once (domains + the kind/intent enums, each row labeled by its axis) rather than an axis-first radio step — strictly fewer keystrokes for the same near-zero-ceremony outcome — and it is queue-aware. The empty state teaches the keystroke first, `engineer target declare` second.
   - **The `:target` palette verb — RESOLVED (#97).** It shipped here as the **full word only** (`:target` → Progress + open declare), with **no `t`-prefixed alias**: `timer` carries the exact alias `t`, so `:t` still wins for the timer (exact beats prefix — the same idiom as `:w`/`:week`), and only `:ta…` reaches target. The muscle-memory `:t` → timer binding survives, guarded by a test. Adjust/retire stay on the Progress screen's `e`/`x`. (The command-palette brief's §8 deferral note is updated to match.)
2. **Pace headless twin (job 5's gap) — shipped.** `engineer progress` ships the `--json` + piped-plain per-target line and the status-bar reduction, the same duality `engineer timer` already has, bound by the quiet-by-default rule.
3. **Time-went glance + rollup (job 7) — shipped (#122).** The kind-mix line grew into a `Tab`-cycled **fold** (by kind → by domain → by intent) on the Progress screen, plus an additive `engineer progress --json` `rollup` object for scripted slicing — explicitly **not** a TUI pivot grid (that stays on the web; see the governing principle in `cross-cutting.brief.md`). Two honest notes on what shipped: the panel mock drew a **bar chart**, distilled to a one-line muted fold (recorded in #122 — one always-visible glance, not a collapsible section); and the **by-domain / by-intent facets render an absent note**, not data — `GET /api/v1/progress` rolls up only `kind_mix`, so per the backend-gap rule the CLI shows the kind facet and marks the other two as awaiting a server rollup rather than deriving a second ledger. That is the module's one **residual gap**: lighting the other two facets needs the server to add `domain_mix` + `intent_mix` to the progress payload — the ask is filed (dsaenztagarro/engineer#810), after which the CLI drops the absent notes with zero further derivation.
