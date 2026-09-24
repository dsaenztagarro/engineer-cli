# 0002 — Sterling, not a replica: the glance-or-gesture test

**Status:** Accepted (ratified across the design roadmap, EPIC #82 → #126; recorded here at #180)

## Context

`engineer` has two clients over one domain: a web app and this terminal client.
The obvious way to build the second one is to port the first — walk the web app's screen inventory and render each surface in a character grid.
That instinct shaped the client's first omnibus design brief, and it is wrong in a way that is expensive to discover late: a TUI that chases web parity ends up with a worse version of every web surface and no reason to exist.

The terminal's actual advantage is not that it can show the same things — it is that it is **already open**, costs one keystroke to reach, and pipes.
The web app is the cockpit; the terminal is the instrument you glance at while your hands stay on the keys.

This law was ratified over the whole design roadmap and every module was measured against it before it was designed, but it lived only inside a design brief — an input document.
It is recorded here because it is the standard *new* work is still measured against, long after the briefs that carried it were consumed.

## Decision

**A full terminal replica of the `engineer` web UI is a non-goal, explicitly.**
The terminal owns the *high-frequency, high-value* core of the study loop, distilled into **glances** (complications) and **gestures** (one-keystroke verbs).
Depth — rich filtering, bulk edit, dashboards, planning canvases, settings forms — stays on the web.
Think of it as an **Apple Watch for the study loop**, not a shrunk-down web app.

A feature earns its place in the terminal only if it is all six of:

1. **A glance or a gesture, not a session.** Value lands in one look (a pace meter, the timer string, a due count) or one keystroke (a verb) — not a lean-back editing workflow. If it needs filtering, sorting, and paging to be useful, ask what single glance or gesture it is standing in for.
2. **Ambient & quiet.** Present without being opened; calm when on-track; a small signal when not; never a nag. On-pace is silence; `behind` is as loud as it gets.
3. **Distilled, not ported.** It answers the *one question that matters in the terminal*. The full table, the analytics grid, the planning canvas, the settings form stay on the web.
4. **Honest.** Paused / idle / over / behind / stale / offline / queued / diverged all render truthfully. The design never hides a state to look tidy. (This is the principle [ADR 0001](0001-terminal-error-notification-model.md) enforces at the level of individual messages, and [ADR 0004](0004-derived-never-stored-and-the-write-queue.md) at the level of unsynced writes.)
5. **Composable — the terminal's edge over a watch.** "Distilled" does not mean "less powerful." Every read pipes and every action is a headless verb ([ADR 0003](0003-tui-headless-contract.md)), so power a watch would lose to a small screen moves to `jq`, git hooks, and status bars instead of to more on-screen chrome.
6. **One-hand, keyboard-only, muscle memory.** `j`/`k`, `/`, `:`, `<Space>` leader, `i`/`Esc` — no mouse, no bespoke keymap.

When a feature could be either "port the web surface" or "distil the one thing that matters here," the terminal always takes the second.
A surface that fails the test belongs on the web.

## Alternatives considered

- **Feature parity with the web app.** Rejected: it produces a strictly worse cockpit and gives the terminal no distinct value. It also loses on its own terms — the web will always out-run a character grid on editing depth.
- **A thin read-only status client (a pure "watch").** Rejected as too narrow: it throws away the terminal's real edge, which is that every read pipes and every verb scripts. Criterion 5 exists to keep that edge explicit.
- **Deciding surface-by-surface, at design time, without a written test.** Rejected: it is exactly how the omnibus drifted. The six criteria are written down so a growth request can be *refused with a reason* rather than argued from taste.

## Consequences

**The exemplars — keep them as designed.** The timer is the bar (an ambient string you glance at, verbs that cost one keystroke, states that never lie, offers that never fire on their own, a headless twin you pipe into a status bar).
With it: Home (the watch face — leads with the timer and pace complications, links out to depth), the Progress pace meters and adjust-in-place, Review's rate sitting (card → `f`/`z`/`s`/`i` → next → clean exit), Notes capture and the `$EDITOR` hand-off ([ADR 0005](0005-editor-for-prose.md)), and the command palette.

**The one deliberate concession, and its cap.** The Activities table (`src/app/screens/activities.rs`) is the closest thing to a web surface in the client.
It earns its place as *the one lean-back ledger you scan and act on by the row* — but it is **capped**: no rich saved-filter matrix, no bulk edit, no server-side `POST /activities/search` (the route exists server-side and is deliberately unclaimed; the `/` narrow over the loaded page is the honest terminal answer until a real workflow outgrows it).
Its watch-native core is **capture** (`engineer log`, `t` to bind the timer) and the **audit** (a short flagged-segment list you clear to empty). Deepen those; hold the table where it is.

**The standing non-goals.** Each of these was decided, not deferred — re-proposing one is a decision to reopen, not an omission to fill:

- **The Review heatmap.** `Dashboard.heatmap` is parsed by the API layer and deliberately not rendered: a heat grid does not reduce to a scannable line in a character grid, and the streak plus this-month counts already convey cadence. A future pass must not "add it back" as a fix. If richer cadence is ever wanted, the honest terminal move is a one-line sparkline reduction (as the timer and progress rails do from a day series), and that is a decision to raise.
- **A Progress pivot grid.** "Where the time went" is a one-line fold plus an `engineer progress --json` rollup you slice yourself — not a TUI pivot table with axes, periods, and pagination.
- **A week planning canvas.** The week board is a planned-vs-done readout, a one-liner declare, and a start-on-a-planned-item gesture. Drag-the-plan, copy-week-forward, and calendar mutations stay on the web.
- **A sync console.** Queued and diverged are a glanced complication and a one-gesture resolve; a full history of every reconciled write is web depth.
- **An inbox manager.** The assisted-capture inbox is a queue you clear, never a surface you administer.
- **A rich long-form editor in ratatui.** See [ADR 0005](0005-editor-for-prose.md).
- **Timer settings editing.** The CLI shows the knobs read-only and points at the web. This one is also API-forced — `GET /api/v1/timer/settings` has no write route — so it is a settled decision on both counts, not an omission.

**Light mode is never in scope.** The terminal client is dark-first by medium.

**Where the visual half of this law lives.** [`references/terminal-design-kit.md`](https://github.com/dsaenztagarro/engineer-cli-ds/blob/master/references/terminal-design-kit.md) in `engineer-cli-ds` is the design kit — the palette mapping, the chrome conventions, and the translate / don't-translate rules that say *how* a distilled surface must look in a character grid. This record says *what* earns a surface; the kit says how to draw it. The two are read together, and the kit links here.
