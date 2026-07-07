# Superseded — the omnibus was decomposed into per-module briefs

**Status:** retired. This file is a tombstone kept so inbound links resolve and the reasoning stays next to the work.

This was the **omnibus** brief — it scoped the whole terminal client at once (timer + ambient presence, pace meters, activities/segments, review, notes, the command-palette verb line, week planning, assisted-capture, and the TUI↔headless duality) as one document.

It has been **decomposed into one brief per engineer-cli module** — see the [briefs index](../README.md). Each module brief carries the omnibus's workflow and jobs for that surface, grounded in the API the module actually consumes (verified against `engineer/config/routes.rb`), and states the *shipped reality* rather than the pre-implementation snapshot the omnibus froze.

## Why it was retired (the decision)

The omnibus went **stale in both directions** faster than a single document could track:

- **The CLI shipped ahead of it.** By the time it was read, epics #7 (daily-loop) and #26 (timer v2) had shipped most of its Phase-1 jobs — the timer, activities table, progress meters, review, notes, segment audit, the headless `engineer timer` suite, and command-palette routing. Its §4 "what's already in the app" (e.g. "only `:logs` routes") described a world that no longer existed.
- **The engineer API shipped ahead of it.** The one net-new client it named — targets-write — turned out to be **server-shipped** (`resources :targets` + `patch :retire`), as were `GET /api/v1/today`, `GET /api/v1/weeks/:iso_week`, and the `/automations` assisted-capture endpoints. Its phasing (features "trailing" future web briefs) was overtaken.
- **Its style anchors were retired.** It cited `books.html` and `terminal-tokens.css` as do-not-edit anchors; both no longer exist. The live anchor is [`../../design-system.dc.html`](../../design-system.dc.html).

A single omnibus is the wrong altitude to keep current as the client grows. One brief per module — the proven `timer` template (a focused, API-grounded brief that drove a clean epic) — is what replaced it. The [briefs README](../README.md) is the index.

The pre-lifecycle [`daily-loop.brief.md`](../../daily-loop.brief.md) was decomposed the same way.
