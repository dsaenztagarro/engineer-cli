# Design briefs — engineer-cli

Handoff briefs for the **terminal** client, mirroring the structure of [`engineer/docs/designs/briefs/`](../../../engineer/docs/designs/briefs/). A brief is a problem-first **input** to design; the rendered terminal mockups (`docs/designs/*.html`, ratatui-faithful) and the shipped Rust screens are the **outputs**.

Read [`../README.md`](../README.md) first — it is the terminal **design kit** (palette mapping, chrome conventions, translate/don't-translate rules, and the `design-system.dc.html` style anchor). A brief here says *what to build and why*; the kit says *how it must look and feel in a character grid*. The two are read together.

Briefs are organised **one per engineer-cli module** (timer, progress, home, …), so a brief maps to the surface a reader is working on. Each module brief carries the same house format — workflow → jobs → binding principles → orientation (shipped reality) → the API it consumes (verified routes) → visual-language pointer → phasing — and folds in any earlier focused brief for that module (e.g. the timer brief absorbed the standalone timer gap analysis). This replaces the earlier single omnibus brief, which scoped the whole client at once and went stale in both directions as the CLI and the `engineer` API each shipped ahead of it.

Every module brief is measured against one **governing principle — sterling, not a replica**: the terminal owns the study loop's high-frequency core as *glances and gestures* (the shipped timer is the exemplar), not a port of the `engineer` web UI. Depth stays on the web. It is the lead section of [`proposed/cross-cutting.brief.md`](proposed/cross-cutting.brief.md) — read it before designing or growing any surface.

## Lifecycle — where a brief lives tells you its status

```
proposed/  ->  (Claude Design produces the .html screens, the CLI implements them)  ->  shipped/
```

- **`proposed/`** — written, awaiting design and/or implementation.
- **`shipped/`** — designed and live in the CLI. Kept, not deleted, so the reasoning stays next to the code.

When a proposed brief ships, `git mv` it `proposed/ -> shipped/` and flip its row below. The move is the status signal.

## Index

One brief per module. Folder tells status: `proposed/` = has open design/implementation work; `shipped/` = designed and live (kept for the reasoning, and still the place to note residual gaps).

### Proposed

| Module brief | What it covers | Open work |
|---|---|---|
| [`proposed/progress.brief.md`](proposed/progress.brief.md) | The Measure pillar: pace meters (shipped), `engineer target` declare/adjust/retire, the pace headless twin, the time-explorer pivot. | targets-write (**first to build**), pace `--json` one-shot, pivot |
| [`proposed/notes.brief.md`](proposed/notes.brief.md) | Five-second capture + the notes browser + one-line anchor read-back. | `$EDITOR`-for-prose (replaces the in-TUI textarea for long-form) |
| [`proposed/week-planning.brief.md`](proposed/week-planning.brief.md) | Plan the week + the retro (planned-vs-done) via `GET /api/v1/weeks/:iso_week`; planning writes go through the activities API. | the whole surface |
| [`proposed/assisted-capture.brief.md`](proposed/assisted-capture.brief.md) | The draft-triage inbox over `/api/v1/automations/tasks` + a terminal-native git-source connect flow (Phase 2). | the whole surface |
| [`proposed/cross-cutting.brief.md`](proposed/cross-cutting.brief.md) | Concerns every module inherits: offline-tolerance / local-clock reconciliation, fuzzy pickers, the TUI↔headless contract, the pending accent-hue decision. | offline/local-clock; broaden fuzzy; ratify accent hue |

### Shipped

| Module brief | What it covers | Shipped in |
|---|---|---|
| [`shipped/timer.brief.md`](shipped/timer.brief.md) | Run the timer without leaving your work + ambient status-bar presence; the full timer face, states, focus rhythm, idle reclaim, overrun, segment audit, and the headless verb suite. Absorbs the earlier timer gap analysis (Section C now resolved). | timer v2 (v0.3.0), guards & rhythm (v0.4.0) |
| [`shipped/activities.brief.md`](shipped/activities.brief.md) | The core activities table (list/filter/sort/complete/archive/duplicate) and the segment drill + audit. | epic #7 daily-loop |
| [`shipped/review.brief.md`](shipped/review.brief.md) | The spaced-repetition dashboard, the due triage, the rate sitting, and browse-all. | epic #7 daily-loop |
| [`shipped/command-palette.brief.md`](shipped/command-palette.brief.md) | The `:` verb line — nav, timer actions, `:note` capture, completion and unknown-verb handling. Notes the `:log`/`:target` verbs still to add. | epic #7 daily-loop |
| [`shipped/home.brief.md`](shipped/home.brief.md) | Home, enriched — lead with the running timer + this week's pace over one `GET /api/v1/today` read, with a global `g`-goto grammar and the `engineer today` headless twin. | epic #61 (v0.6.0) |

*(Some of the CLI's earliest screens — Login, Books, Book detail — predate the briefs workflow; see the kit README's screen inventory. The retired omnibus `terminal-client.brief.md` and the pre-lifecycle `daily-loop.brief.md` were decomposed into the module set above.)*

## Writing a brief

Match the house format the module briefs share (For / Produces / Do-not-edit / Status header; a first-person workflow; jobs-as-outcomes; binding principles; an orientation section that states the *shipped reality*; the API the module consumes with **verified** routes; a hard visual-language constraint; out-of-scope; phasing) — but bind the visual language to **this repo's kit** (`../README.md`, `../design-system.dc.html`), not the web design system. Keep it problem-first and non-prescriptive: name existing screens, widgets, and the pre-built API client as *reuse context*, never as the prescribed answer. When a module is new or growing, put its brief in `proposed/`; when a proposed brief's work ships, `git mv` it to `shipped/` and flip its row above — the move is the status signal.
