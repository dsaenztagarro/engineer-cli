# Scope

**Status:** Accepted · **Decision log:** [#209](https://github.com/dsaenztagarro/engineer-cli/issues/209)

What earns a place in the terminal and what stays on the web, and where the line runs between the design project, which draws the surface, and this repository, which decides what the surface does.

## Context

`engineer` has two clients over one domain: a web app and this terminal client.
The obvious way to build the second one is to port the first — walk the web app's screen inventory and render each surface in a character grid.
That instinct is wrong in a way that is expensive to discover late: a TUI that chases web parity ends up with a worse version of every web surface and no reason to exist.

The terminal's actual advantage is not that it can show the same things — it is that it is **already open**, costs one keystroke to reach, and pipes.
The web app is the cockpit; the terminal is the instrument you glance at while your hands stay on the keys.

The client is designed in a Claude Design project and built here.
The design project cannot see this repository, so a rule it draws cannot be corrected where it is drawn: the next export restores every stale sentence.
Design content copied into a repository that ships a binary grows without bound, and a source comment citing a canvas or a section label points at something that regenerates and renumbers.

## Decisions

### A full terminal replica of the web UI is a non-goal

The terminal owns the *high-frequency, high-value* core of the study loop, distilled into **glances** (complications) and **gestures** (one-keystroke verbs).
Depth — rich filtering, bulk edit, dashboards, planning canvases, settings forms — stays on the web.
Think of it as an **Apple Watch for the study loop**, not a shrunk-down web app.

### A surface earns its place by passing all six points of the glance-or-gesture test

1. **A glance or a gesture, not a session.** Value lands in one look (a pace meter, the timer string, a due count) or one keystroke (a verb) — not a lean-back editing workflow. If it needs filtering, sorting, and paging to be useful, ask what single glance or gesture it is standing in for.
2. **Ambient & quiet.** Present without being opened; calm when on-track; a small signal when not; never a nag. On-pace is silence; `behind` is as loud as it gets.
3. **Distilled, not ported.** It answers the *one question that matters in the terminal*. The full table, the analytics grid, the planning canvas, the settings form stay on the web.
4. **Honest.** Paused / idle / over / behind / stale / offline / queued / diverged all render truthfully. The design never hides a state to look tidy. The [terminal-surface](terminal-surface.md) record enforces this for individual messages, and the [client-state](client-state.md) record for unsynced writes.
5. **Composable — the terminal's edge over a watch.** "Distilled" does not mean "less powerful." Every read pipes and every action is a headless verb ([terminal-surface](terminal-surface.md)), so power a watch would lose to a small screen moves to `jq`, git hooks, and status bars instead of to more on-screen chrome.
6. **One-hand, keyboard-only, muscle memory.** `j`/`k`, `/`, `:`, `<Space>` leader, `i`/`Esc` — no mouse, no bespoke keymap.

When a feature could be either "port the web surface" or "distil the one thing that matters here," the terminal always takes the second.
A surface that fails the test belongs on the web.

### The timer is the bar, and the exemplars stay as designed

The timer is an ambient string you glance at, verbs that cost one keystroke, states that never lie, offers that never fire on their own, and a headless twin you pipe into a status bar.
With it: Home (the watch face — leads with the timer and pace complications, links out to depth), the Progress pace meters and adjust-in-place, Review's rate sitting (card → `f`/`z`/`s`/`i` → next → clean exit), Notes capture and the `$EDITOR` hand-off, and the command palette.

### The Activities table is the one deliberate concession, and it is capped

The Activities table (`src/app/screens/activities.rs`) is the closest thing to a web surface in the client.
It earns its place as *the one lean-back ledger you scan and act on by the row* — but it is capped: no rich saved-filter matrix, no bulk edit, no server-side `POST /activities/search` (the route exists server-side and is deliberately unclaimed; the `/` narrow over the loaded page is the honest terminal answer).
Its watch-native core is **capture** (`engineer log`, `t` to bind the timer) and the **audit** (a short flagged-segment list you clear to empty). Deepen those; hold the table where it is.

### The standing non-goals were decided, not deferred

Re-proposing one is a decision to reopen, not an omission to fill:

- **The Review heatmap.** `Dashboard.heatmap` is parsed by the API layer and deliberately not rendered: a heat grid does not reduce to a scannable line in a character grid, and the streak plus this-month counts already convey cadence. A future pass must not "add it back" as a fix.
- **A Progress pivot grid.** "Where the time went" is a one-line fold plus an `engineer progress --json` rollup you slice yourself — not a TUI pivot table with axes, periods, and pagination.
- **A week planning canvas.** The week board is a planned-vs-done readout, a one-liner declare, and a start-on-a-planned-item gesture. Drag-the-plan, copy-week-forward, and calendar mutations stay on the web.
- **A sync console.** Queued and diverged are a glanced complication and a one-gesture resolve; a full history of every reconciled write is web depth.
- **An inbox manager.** The assisted-capture inbox is a queue you clear, never a surface you administer.
- **A rich long-form editor in ratatui.** Prose opens in `$EDITOR` ([terminal-surface](terminal-surface.md)).
- **Timer settings editing.** The CLI shows the knobs read-only and points at the web. This one is also API-forced — `GET /api/v1/timer/settings` has no write route — so it is settled on both counts.

### Light mode is never in scope

The terminal client is dark-first by medium.

### This record says what earns a surface; the design kit says how to draw it

[`references/terminal-design-kit.md`](https://github.com/dsaenztagarro/engineer-cli-ds/blob/master/references/terminal-design-kit.md) in `engineer-cli-ds` holds the palette mapping, the chrome conventions, and the translate / don't-translate rules that say *how* a distilled surface must look in a character grid.
The two are read together, and the kit links here.

### The design project ships surface only; what the surface does is decided here

What a screen looks like is the design's; what it does is this repository's — a test for the *what*, a decision record for the *why*.
When drawing a state forces a behaviour fork a brief did not settle, the design returns it as an open question, never a ruling.
A behaviour a page draws and nothing here tests is not a specification of this client: it becomes an issue, or it is corrected on the page.

### Code never references a design, in any form

Not a page path, not a section label, not the same thing spelled out in words.
A decision-record citation is fine: it is hand-owned, stable and correctable here.
`tests/design_references.rs` fails on a design reference in `src/`.

### Design content lives in `engineer-cli-ds`

The pages, the kit, the briefs and the palette live there and are not copied here.
`/epic` reads a page from a sibling checkout, `${ENGINEER_CLI_DS:-../engineer-cli-ds}/pages/`, the same convention `engineer-cli-ds` uses to find `cli-ds`.
Exactly one file crosses the boundary, and it is data: the generated palette ([terminal-surface](terminal-surface.md)).

## Rejected

- **Feature parity with the web app.** It produces a strictly worse cockpit and gives the terminal no distinct value; the web will always out-run a character grid on editing depth.
- **A thin read-only status client.** It throws away the terminal's real edge — every read pipes and every verb scripts — which criterion 5 exists to keep explicit.
- **Deciding surface-by-surface without a written test.** A growth request is then argued from taste instead of refused with a reason.
- **Re-pointing each design citation at the page's new location.** It is the same defect with a different path, and a page regenerates.
- **A prose specification beside the code for every rule.** Prose that describes behaviour drifts, and a test cannot; it stays available for a contract no test can hold.
- **`/epic` fetching a page by URL, or through an index kept here.** A URL needs `gh` auth and a network read per page, and an index is one more file to drift; the sibling checkout is what the design repositories already assume.

## Left open

- **Richer Review cadence.** Reopens if the streak and this-month counts stop conveying cadence; the honest terminal move is then a one-line sparkline reduction, as the timer and progress rails do from a day series — never the heat grid.
- **Server-side activity search.** Reopens when a real workflow outgrows the `/` narrow over the loaded page.

## References

[`engineer-cli-ds`](https://github.com/dsaenztagarro/engineer-cli-ds) · [the terminal design kit](https://github.com/dsaenztagarro/engineer-cli-ds/blob/master/references/terminal-design-kit.md) · `tests/design_references.rs` · `src/app/screens/activities.rs`
