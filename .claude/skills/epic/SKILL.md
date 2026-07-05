---
name: epic
description: Turn a docs/designs/*.dc.html design doc into a GitHub epic — decompose the design into self-contained sub-issues with a phased plan, then implement them one ticket at a time (branch → tests → PR → squash-merge), carrying every pragmatic decision forward on the epic issue so later tickets inherit the context. User-invoked only; the user reviews the work at the very end. Run `/epic docs/designs/<file>.dc.html` to start, or `/epic resume #<epic>` to continue.
argument-hint: "<docs/designs/file.dc.html> | resume #<epic>"
---

# Run an epic from a design doc

When the user invokes this skill, take one design document from `docs/designs/`
and drive it end-to-end: open a GitHub **epic** issue, split the design into meaningful
self-contained **sub-issues** with a phased plan, then implement the sub-issues **one at a
time** until the epic is done. The user reviews the work only at the very end — so the
**epic issue is the shared memory** between tickets, and the terminal stays quiet.

This skill is **user-invoked only**. It creates real GitHub issues and merges real PRs, so
never auto-trigger it; run it only when the user types `/epic`.

## Operating principles (hold these the whole run)

- **One ticket at a time.** Never implement sub-issues in parallel. Finish, test, and ship
  the current ticket before starting the next.
- **The epic carries the context.** Every pragmatic decision goes on the epic's **Decisions
  Log** _and_ on the sub-issue. Before each ticket, re-read the log.
- **Terse terminal, rich issues.** No diff dumps, no test-log dumps, no plan-to-do narration.
  Emit one short status line per ticket. The detailed record lives in the issues and the epic.
- **Document where it belongs.** Pragmatic ticket decision → sub-issue + epic log.
  Architecture-level decision → _also_ a decision record (see step 3.5).
- **Never publish a release.** Pushing a version tag triggers cargo-dist: it builds installers
  and publishes a Homebrew formula to the public tap. The run may _prepare_ a release (version
  bump + CHANGELOG), but the tag push is handed back to the user; never `git push --tags`.
- **Follow the repo's conventions.** There is no AGENTS.md; the sources of truth are:
  - `docs/README.md` + `docs/ui-rendering.md` — the TEA architecture: state + `Action` enum +
    reducer + pure render. New screens live in `src/app/screens/`, register in the reducer,
    and reuse the chrome/widgets in `src/ui/`.
  - `docs/api-layer.md` — API client conventions: typed models per resource module, the
    `envelope`/`List` wrappers, `ApiError`, tracing of every call, wiremock tests.
  - `docs/designs/README.md` — the terminal design kit: palette, chrome (header/body/footer),
    widget idioms, keyboard grammar (`j/k`, `gg/G`, `/`, `:`, `Space` leader, `i`/`Esc`).
    Mockups are ratatui-faithful; implement what they show, in these idioms.
  - `CHANGELOG.md` — Keep a Changelog; every user-visible ticket adds to `[Unreleased]`.
  - Conventional Commits (`type(scope): subject`), as the git log shows.
- **Design fidelity is verified, not assumed.** Every interactive workflow shown in the design
  must have a test: wiremock tests for each API call the screen drives, and reducer tests
  (Action in → state out) for the keyboard workflows. A merged ticket without a workflow test
  is an open gap, not a closed one — and a _closed_ epic is not proof that its design is fully
  implemented. When re-running on a design a prior epic already touched, trust the **diff
  against the current code**, not the old checklist.

## 0. Resolve the argument (new vs resume)

| Argument | Mode |
|---|---|
| a `docs/designs/*.dc.html` path the codebase has **never** built against | **new epic** |
| a `docs/designs/*.dc.html` path a **prior epic already touched** (open or closed) | **re-diff** (reconciliation) |
| `resume #<n>` or a bare epic issue number | **resume** an in-flight epic |
| _(none)_ | **ask** which design doc; do not guess |

**Idempotency guard (path mode):** before creating anything, run
`gh issue list --label epic --search "<design filename>"` across **both** states
(`--state open` and `--state closed`) to find any epic that referenced this design doc.

- An **open** epic exists → switch to **resume** for it instead of opening a duplicate.
- A **closed/merged** epic exists (the design shipped once already, or has since been
  updated) → switch to **re-diff mode** below. A closed epic is _not_ proof the current
  design is fully or faithfully implemented — re-diff to find what changed or was never
  finished. Open a **fresh** epic scoped to the delta; do not reopen the old one.
- Nothing references it → **new epic**.

**Resume mode:** read the epic body — its phased checklist tells you which sub-issues are
done (`[x]`), skipped, or outstanding (`[ ]`). Read the **Decisions Log** in full, then jump
straight to step 3 for the first outstanding ticket. Do not recreate issues. If the design
HTML has been **edited** since the epic was created, run the **re-diff pass (step 1.5)** over
the outstanding scope first, so renamed/added/removed workflows are caught — don't trust a
stale checklist.

**Re-diff (reconciliation) mode:** the design was implemented before, fully or partly, and
you are reconciling the current code against its **latest** version. Plan as a new epic
(steps 1–2) but make the **re-diff pass (step 1.5)** the basis of decomposition: the epic's
tickets are the workflows that are missing, broken, partial, or untested — not a greenfield
rebuild. Reference the prior epic in the new epic's Summary so the history is traceable.

## 1. Plan the epic (read the design → decompose)

1. Read the design doc. These are Claude Design **canvas** docs (`*.dc.html`): the sections
   are the labelled screen blocks (`data-screen-label="…"`) plus the annotated captions and
   key-hint footers around them. Enumerate every screen, state, and keyboard workflow the doc
   specifies — including headless/`--json` command panels, which are as much scope as the TUI
   screens. Refer to sections by their screen label (e.g. `§Timer hero`, `§Idle reclaim`).
2. Ground the design against the codebase where a section maps to existing code — prefer
   `codegraph_*` tools when the index exists (`.codegraph/`), else the `Explore` agent. You
   are looking for what already exists so tickets reuse it instead of rebuilding it. In this
   repo the API clients are typically **ahead of the screens** — a "new" screen is often
   wiring to an existing `src/api/*` client, not a new endpoint.
3. **In re-diff / resume modes, run the re-diff pass (step 1.5) now** — it replaces a blank-page
   decomposition with a gap-driven one.
4. Decompose into **meaningful, self-contained issues**. Each issue should be shippable on
   its own and map to one or more design sections. Avoid issues that can't merge without
   another unmerged issue; when two pieces are inseparable, make them one ticket. In re-diff
   mode, each issue is a **gap** from step 1.5 (missing / broken / partial / untested), never
   a workflow already classified `covered+tested`.
5. Order the issues into **phases by dependency** — foundation / shared vocabulary first
   (e.g. an API client or a shared widget), then the surfaces that build on it.
6. Draft the epic body using the **Epic body template** below.
7. **Approval gate (the one interactive pause):** show the user the proposed epic title and
   the phased issue list — titles + the section each maps to — and get a yes before creating
   anything on GitHub. This is the cheapest point to course-correct, and creating a dozen
   issues is outward-facing and hard to undo. After the yes, run autonomously to the end.

### 1.5 Re-diff pass (re-diff & resume modes only)

When a prior epic already touched this design — or the design HTML changed since the epic was
created — **diff the current implementation against the latest design before decomposing.** The
prior epic being closed means nothing; verify against the code.

1. **Enumerate** every UI surface, element, and **user workflow** the latest design specifies
   (each interactive affordance: every list, toggle, picker, inline edit, keyboard shortcut,
   headless command form, empty/edge state). A subagent (`Explore`) reading the design HTML
   end-to-end is the cheapest way to get an exhaustive list.
2. **Classify** each workflow against the current code (`codegraph_*` when indexed, `Explore`):
   - `covered+tested` — implemented **and** has a test exercising it → **out of scope**.
   - `covered-untested` — implemented but no test → in scope (backfill the test).
   - `partial` — implemented but diverges from the design (fidelity gap) → in scope.
   - `broken` — implemented but doesn't work → in scope, fix first.
   - `missing` — not implemented → in scope.
3. **The classification _is_ the decomposition.** Group the not-`covered+tested` workflows into
   phased tickets (step 1, items 4–5). Order: `broken` fixes and shared atoms first, big surface
   reworks next, cross-surface integration last.
4. **Surface, never drop.** Items the prior epic skipped or left partial are listed explicitly
   in the new epic — silently inheriting "done" from a stale checkbox is the failure mode this
   mode exists to prevent.
5. **Verify the classification.** Treat your own "covered+tested" calls skeptically — a passing
   suite that never exercises the workflow proves nothing. When unsure, read the test and
   confirm it asserts the workflow (the Action → state transition, the request body), not just
   that a function returns Ok.

## 2. Create the epic and its sub-issues

1. Create the epic: `gh issue create --label epic --title "EPIC: <title>" --body-file <tmp>`.
   Capture the epic number `#E`. (If the `epic` label is missing, create it once:
   `gh label create epic`.)
2. Create one issue per planned ticket with `gh issue create`, body =
   - first line: `Part of EPIC #E · Design [§<label>](docs/designs/<file>.dc.html)`
   - `## Goal` — what this ticket delivers
   - `## Acceptance` — checkbox criteria
   - `## Technical notes` — files/patterns to reuse, constraints
   - `## Decisions` — placeholder, filled as the ticket is implemented
3. Rewrite the epic's **Phased plan** so each line references the real sub-issue number:
   `- [ ] #<sub> — §<label> — <title>`, grouped by phase. Edit the epic with `gh issue edit #E`.

## 3. Implement tickets — strictly one at a time

Loop over the outstanding tickets **in phase order**. For each:

### 3.1 Pre-flight context
- Read the sub-issue in full (`gh issue view #<sub>`).
- Re-read the epic's **Decisions Log** (`gh issue view #E`). Pull any entry whose _affects:_
  tag names this ticket, and honor it.
- Ensure a clean working tree on an up-to-date `master` (`git switch master && git pull`).

### 3.2 Blocked check (skip rule)
If a **critical step is blocking** and **no pragmatic decision can unblock it**:
- comment `Skipped: <reason>` on the sub-issue,
- append a `⚠ skipped` entry to the epic Decisions Log (with the reason and what would
  unblock it),
- leave the epic checkbox **unchecked**,
- move on to the next ticket. **Never silently drop a ticket.**

A backend gap is the canonical blocker here: this client renders what the Engineer API
serves. If a designed workflow needs an endpoint the API doesn't expose (check
`docs/api-layer.md` and `src/api/`), don't fake it client-side — skip the ticket and record
what the API must ship first.

If a pragmatic decision _can_ unblock it, take the smallest reasonable one and record it
(step 3.9) — don't skip.

### 3.3 Branch
`git checkout -b <sub>-<slug>` from `master` (e.g. `312-timer-screen`).

### 3.4 Implement
Follow the repo conventions (see Operating principles). Reuse before you build. In
particular: new screens are TEA modules under `src/app/screens/` wired through the `Action`
enum and reducer; presentation reuses `src/ui/` chrome and widgets (`bordered`, `status_pill`,
`progress_bar`, `notify`) — no bespoke chrome; API calls go through `ApiClient` with typed
models and are traced; errors surface as `notify` tiles, never panics; keyboard handling
follows the neovim grammar and the footer must advertise the active keys. When the ticket
changes the API layer, update `docs/api-layer.md`; when it changes commands or flags, update
`README.md`'s Commands section; every user-visible change adds a `CHANGELOG.md` `[Unreleased]`
entry. If the ticket ships the last surface of a `docs/designs/briefs/proposed/*.brief.md`,
`git mv` the brief to `shipped/` and flip its index row (the move is the status signal).

### 3.5 Decision-record gate (architecture-level decisions only)
If this ticket made a decision that's architecturally meaningful (a data/sync contract such
as offline reconciliation, a cross-cutting integration choice, a security boundary — not a
local code choice), record it durably:
- add a decision record under `docs/architecture/decisions/` as `NNNN-<slug>.md` (create the
  folder with a one-paragraph README on first use),
- fill Context / Decision / Alternatives considered / Consequences, ASCII diagrams only,
- reference the sub-issue and epic in the record.

### 3.6 Test gate
`cargo test` must pass, plus `cargo fmt --all -- --check` and
`cargo clippy --all-targets --all-features -- -D warnings` — the same three gates CI runs.
Fix failures before going further.

### 3.7 Commit
Conventional Commits with scope and the issue ref:
`type(scope): <subject> (#<sub>)`.

### 3.8 Ship the ticket (merge — no release here)
- `git push -u origin HEAD`
- `gh pr create` with a body that ends `Closes #<sub>` and links `Part of EPIC #E`
- CI runs on PRs in this repo — wait for `gh pr checks --watch` to go green (the local gate
  in 3.6 should make this a formality), then squash-merge:
  `gh pr merge --squash --delete-branch`
- `git switch master && git pull`

Release/versioning does **not** happen per ticket — it happens once at epic completion
(step 4).

### 3.9 Record + carry the decision forward
For every pragmatic decision taken on this ticket:
- comment it on the sub-issue (`gh issue comment #<sub>`), and
- append one line to the epic **Decisions Log** (`gh issue edit #E`), tagging which future
  tickets it may affect.
Then check the ticket's box on the epic Phased plan and annotate it `shipped (PR #X)`.

### 3.10 Status line
Emit one short line, e.g. `✓ #312 timer screen — shipped (PR #318)` or
`⚠ #314 idle reclaim — skipped (needs the timer-hygiene API, not shipped yet)`. Nothing more.

## 4. Close out the epic

When every ticket is shipped or consciously skipped:
1. **Prepare** one release for the epic's merged work: bump the version in `Cargo.toml`,
   move `CHANGELOG.md` `[Unreleased]` into a dated release section, commit and merge that as
   a normal PR. One release per epic — not per ticket.
2. Close the epic issue if nothing is outstanding; otherwise leave it open with an
   `## Outstanding` note listing the skipped tickets and what would unblock them.
3. **Do not push the tag.** Tagging publishes installers and the Homebrew formula via
   cargo-dist. Print the handoff so the user can release themselves:
   `! git tag vX.Y.Z && git push origin vX.Y.Z`
4. Print a brief final summary (a few lines): tickets shipped, tickets skipped + why,
   decision records created, the headline pragmatic decisions, the prepared version, and the
   tag handoff.

## Epic body template

```markdown
# EPIC: <title>

**Design:** [docs/designs/<file>.dc.html](docs/designs/<file>.dc.html)

## Summary
<2-3 sentences: what this epic delivers and why>

## Core principles
- <invariants every ticket must uphold — naming, contracts, design fidelity>

## Phased plan
**Phase 1 — <name>**
- [ ] #<sub> — §<label> — <title>
- [ ] #<sub> — §<label> — <title>

**Phase 2 — <name>**
- [ ] #<sub> — §<label> — <title>

## Decisions Log
<!-- append-only; newest last. One line per pragmatic or skip decision. -->
- **#<sub> (§<label>):** <decision> — _why:_ <rationale> — _affects:_ #<a>, #<b> (or "none")
```

## Notes for the agent

- The **approval gate in step 1.7 is the only mid-run pause.** Everything after it runs to
  completion without asking — the user reviews the finished work at the end, as they asked.
- The Decisions Log is **append-only**. Don't rewrite earlier entries; add new ones. An
  accepted decision record is likewise immutable — supersede with a new one, never edit in
  place.
- Resume safety: identify done/skipped/outstanding tickets from the epic's checklist state,
  not from local git — a ticket is "done" only when its box is checked and its PR is merged.
- If `gh` reports the design doc isn't referenced by any open epic but a half-built one looks
  related, ask the user whether to resume it rather than opening a second epic.
- Keep the design doc link relative (`docs/designs/<file>.dc.html`) so it resolves in the
  repo; name the specific screen label (`§<label>`) the ticket implements.

### Design versioning & incremental work

- A design doc is a **living file**. The same `docs/designs/<file>.dc.html` may be re-run
  after it's been edited (Claude Design iterates), or after a prior epic shipped part of it.
  The skill must reconcile, not rebuild — that's what **re-diff mode** (step 0) and the
  **re-diff pass** (step 1.5) are for.
- "The last epic closed" is the trap. A closed epic + green CI says the tickets merged; it
  does **not** say the design is faithfully implemented or that every workflow has a test.
  Always diff against the current code.
- When re-diffing, the new epic's Summary should name the **prior epic** and the design
  version it reconciles against, so the lineage is traceable from issue history.
- Carry the prior epic's **Decisions Log** forward as context (read it), but the new epic
  gets its **own** log — don't edit the closed epic's.
- **Design ↔ backend sync gate:** before decomposing, sanity-check the design against what
  the Engineer API actually serves (`docs/api-layer.md`, `src/api/`). If the design shows
  workflows the backend can't power yet, surface that as a `docs/designs/MISSING.md` note for
  Claude Design / the web repo instead of silently narrowing scope — designed-but-unbuildable
  surfaces become skip-rule tickets (step 3.2) so they stay visible on the epic.
