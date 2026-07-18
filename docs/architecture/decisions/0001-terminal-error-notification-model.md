# 0001 — The terminal error, notification & search-state model

**Status:** Accepted (EPIC #161)

## Context

`docs/designs/design-system.dc.html` (§ERROR & NOTIFICATION MODEL, §NOTIFICATION & SEARCH STATES, §SIGN IN · SERVER ERROR) defines one model every screen must speak: every message the client shows lands in exactly one of three tiers, chosen by *scope × lifetime*, and the wording is identical across the TUI tile, the inline panel line, and the headless `stderr` a script greps (§C — "one spelling per outcome").

Before this epic only **Tier 1** (the footer notify tile, `src/ui/notify.rs`) existed. Tiers 2–3 and the search-state contract were absent or ad-hoc, and — the concrete failure this epic set out to fix — **12 of 16 screens collapsed a read *failure* into an *empty* result** (a fetch closure sending `Loaded(vec![])` on error), so "the server is down" and "you have no books" rendered identically. That violates the model's one hard rule (distinguish empty from failed) and the governing honesty principle (§0·4: offline / stale / failed always render, never hidden to look tidy).

## Decision

Build the model as **small shared atoms in `src/ui/` plus a copy catalogue**, adopted by every screen and the QuickCapture overlay:

```
Tier 1  footer notify tile      ui::notify        transient · a keystroke's outcome     (already shipped)
Tier 2  inline panel state      ui::panel         persistent · one region's read failed
Tier 3  blocking screen         ui::blocking      persistent · whole screen unusable
        search states           ui::search        query-in-title · highlight · n/N
        one spelling (§C)        crate::messages   fail_reason / load_failed / offline / …
```

Four decisions shaped the design:

1. **Tier 2 is a pure-presentation `PanelState` enum + `render_panel_state` fn, not a generic `LoadState<T>` wrapper.** The 16 screens are heterogeneous: six hold `Vec<T>`, but six-plus hold `Option<Box<Aggregate>>` mapping **one** read to **many** panels (Home → five bands, Progress, Week, Review's three stages, BookDetail, Audit). A `LoadState<Vec<T>>` newtype forces a `match` at every access site, fights the `ListState`/`TableState` selection that must be borrowed mutably beside the data at render time, and cannot model "one failed read, five panels." So each screen keeps its own fields (`items` / aggregate + `loading` + `failure: Option<PanelFailure>`) and computes a `PanelState` at render; the failed/empty/loading *body* is the only shared thing.

2. **Tier 3 has exactly two owners.** A whole-screen blocking state is the loudest, rarest tier, reserved for when the screen is meaningless without something that failed. Only **Login** (its own read *is* the session) and a **global 401 → re-auth interceptor** (`Action::SessionExpired`, handled in `App`) route there. Every other screen's failure is a Tier-2 panel while its header timer, footer, and nav stay live; at launch offline, reads fall back to cached read-only (§A) rather than blocking. This keeps the blocking surface tiny and the common failure recoverable in place.

3. **`o open last-cached` ships hidden.** The Tier-2 mock offers `r retry · o open last-cached`, but no screen keeps a read-cache yet. `PanelFailure.cached` is `false` everywhere, so the `o` affordance never renders — advertising a cache that isn't there would fail §0·4. The atom is cache-ready; wiring a real read-cache is a named follow-on.

4. **Search's server/client split is resolved by role.** `/` stays whatever a screen already is — a server re-query (Books, Notes, Review browse) or an in-place filter (Activities) — and never changes. `n`/`N` never touch the network: they step the cursor over the *loaded* rows whose label matches, and `ui::search::highlight` paints the run. Timer's `query` is a bind autocomplete, not list search, and is left alone — so there are four search adopters, not five.

## The §C reconciliation

Full parity was the stated goal, but reconciling the headless twins revealed that **not all "drift" is drift**. Two outcomes are genuinely shared and were unified:

- **Auth** — `"not authenticated — run \`engineer login\`"` was spelled verbatim in five CLIs and a TUI helper; it now has a single source, `messages::not_authenticated()`, so it can never diverge.
- **Read failure** — the TUI Tier-2 panel and its CLI twin describe the same failed read from `messages::fail_reason`.

But the **offline refusals are intentionally verb-specific and richer than a generic template** — `"offline — can't resolve \"{q}\"; start bare or retry online"` (timer), `"…capture loose or retry online"` (note), `"…log a new one, or retry online"` (log). Collapsing these into one `messages::offline(verb)` string ("offline — {verb} needs the server; retry online") would *regress* the guidance a user gets, not improve parity. §C asks that the *same outcome* share one spelling; a verb telling you exactly how to proceed offline is a *different, better* outcome than a generic refusal. So these are deliberately **not** flattened. `messages::offline(verb)` remains the catalogue entry for the plain case (the TUI screens that have no richer thing to say).

## Alternatives considered

- **A generic `LoadState<T>` wrapper** — rejected (decision 1): it fits the flat lists but fights the aggregate screens and the selection borrow.
- **Every screen can go Tier 3** — rejected (decision 2): a failed list read blanking the whole screen is exactly the "hidden state to look tidy" the model forbids; Tier 2 keeps the chrome live.
- **Mechanically route every CLI string through the catalogue** — rejected for the offline refusals: it trades richer, actionable guidance for uniformity the design does not actually ask for.
- **A §C sweep smeared across each screen ticket** — rejected in favour of one deliberate reconciliation pass (#172), so the CLI-copy decision is made once, in one place, with this record.

## Consequences

- A read that fails renders as itself everywhere — a loud Tier-2 panel with a retry key, never an empty list; the honesty principle holds by construction.
- New screens inherit the model cheaply: keep a `failure: Option<PanelFailure>`, route `Err(Unauthorized) → SessionExpired` and other errors to a `*LoadFailed(messages::fail_reason(...))`, and render `render_panel_state` when the region has no rows.
- The copy for a given outcome lives in `src/messages.rs`; a twin that needs to match a screen calls the same function, so a script greps what the screen shows.
- `o open last-cached` and a real read-cache are a tracked follow-on, as is the option to enrich the TUI's generic offline copy toward the CLIs' verb-specific guidance.
