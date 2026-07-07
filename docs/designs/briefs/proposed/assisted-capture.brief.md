# Design brief — engineer-cli · Assisted-capture inbox & the git-source connect flow (Phase 2)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal draft-triage inbox — the pending-drafts list, the open-one-draft view, and the acknowledge / accept / reject verbs — *and* a terminal-native git-source connect flow (how a user who commits from the terminal wires their local git activity as a capture source), plus both surfaces' headless twins (`engineer inbox …`). Extend `../../design-system.dc.html` (the style anchor); an `assisted-capture.dc.html` board is the natural home for the mock, mirroring `timer.dc.html` and `progress.dc.html`.
**Status:** **proposed.** *Nothing* exists in the CLI yet — there is no automations API client and no inbox screen. But the `engineer` **server** API is shipped (Human-in-the-Loop automation tasks — verified below), so this is buildable the moment it's prioritised; it no longer "trails" a web brief the way the omnibus assumed.

> **Module note.** This brief is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the omnibus's (`terminal-client.brief.md`) Phase-2 **assisted-capture inbox** — the *one* Phase-2 surface the omnibus explicitly said warrants a full brief of its own, "a new draft-triage screen + a terminal-native git-source connect flow," because the git-activity source is native to this user (they commit from the terminal), so the terminal is arguably its best home. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "I commit from the terminal all day. Turning those commits into draft activities I can accept, edit, or reject — right where I already am — is how the log stops lying, instead of me reconstructing my week from memory.
> The drafts should wait for me quietly — a count in the corner, not a popup — and clearing them should feel like triage: open, glance, ⏎ to accept, `x` to reject, next.
> And connecting the source should be one terminal-native step: point engineer at the repos I already commit in, from the shell, not a web settings page."

The throughline: **the git-activity source is native here.** The user's commits already happen in this surface, so the assisted-capture pipeline's most natural front door is the terminal, not the web. This module builds the two halves that make that true — a draft-triage inbox that consumes the shipped Human-in-the-Loop tasks, and the connect flow that wires the local repos as a source in the first place.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**1. Triage the draft inbox.** List the pending drafts, open one to see what the automation proposes, and act — **acknowledge** (seen, keep for later), **accept** (write it — the server's `complete`), or **reject** (discard, optional reason) — reusing the **triage-list + due-badge grammar the Review screen already ships** (`src/app/screens/review.rs`: the "N due" count header, the `QUEUE · urgency order` list, the per-row badge). A draft carries an `expires_at`, so the due-badge grammar transfers directly. *Not built.*

**2. Connect the git source from the terminal.** A terminal-native flow to wire the user's local git activity as a capture source — the step that makes drafts *appear*. This is the genuinely-new, terminal-specific surface (the timer status string, `notify` tiles, and triage list are all inherited; this is not). Its client UX is in scope; the **server contract for registering a source is not in the tasks routes** and is the open question this brief raises (§5). *Not built.*

---

## 3. Principles that genuinely bind

- **Terminal-native, because the source is.** The whole reason this surface earns a full brief rather than an "apply the grammar to the API" build is job 2: the git-activity source is native to a user who commits from the terminal. Design the connect flow as a first-class shell step, not a port of a web settings page.
- **Capture is sacred.** Accepting a draft *writes an activity* (the server's `complete` fires the automation's `on_complete` hook). Treat that write with the same care the log verbs get: confirm what's being written, make reject cheap and reversible-feeling, and never let a mis-key silently commit a wrong segment. The inbox exists to make the log honest, not to add noise to it.
- **TUI ↔ headless duality is first-class.** Every read and verb the screen shows must also exist as a non-interactive one-shot — `engineer inbox` (list), `engineer inbox show <id>`, and `accept` / `ack` / `reject` — with `--json` (machine) and plain-text (pipe) forms, TTY-detected and `NO_COLOR`-respecting, meaningful exit codes, no ANSI when piped. The offline / local-clock tolerance this leans on is captured in `cross-cutting.brief.md`.
- **Quiet by default; a count, never a nag.** Pending drafts surface as an *ambient count* — a header cell / `notify` tile in the timer's ambient-presence idiom (`briefs/shipped/timer.brief.md` job 2's status-cell + `--short` string), plus a single status-bar reduction (`inbox 3`) — never an interrupt. A draft that expires unseen is the pipeline's problem to re-raise, not the CLI's to shout about.

---

## 4. What's already in the app (orientation)

- **Grammar to reuse (all shipped):** the Review screen's triage list — the "N topic(s) due · ~M min" count header, the `QUEUE · urgency order` section, and the queue-preview rows (`src/app/screens/review.rs`); the `status_pill` badge idiom (` reading ` / ` done ` / ` hold `, black ink on a semantic fill); `notify` tiles (`src/ui/notify.rs`); and the timer's ambient header status-cell + `--short` status string (`briefs/shipped/timer.brief.md`) — the exact ambient-count idiom job 1's badge should copy.
- **Read/write plumbing to reuse:** the API client harness (`src/api/mod.rs`, the `envelope` / `error` helpers) and the paginated `{ data, page, per_page, total }` envelope the other list clients already decode — the automations index/pending responses use the same shape.
- **What genuinely does not exist yet:** *everything this module owns.* There is no `src/api/automations.rs` (no automations client of any kind — verified: nothing under `src/` references automations), no inbox screen under `src/app/screens/`, no `:inbox` palette route, no `engineer inbox` headless verb, and no git-source connect flow. This is net-new client work on top of a shipped server API.

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

The Human-in-the-Loop task API is **shipped server-side** — the `namespace :automations` block inside `namespace :v1` (`routes.rb:437-448`), served by `app/controllers/api/v1/automations/tasks_controller.rb`. This module is CLI-only work, not blocked on the server. (This updates the omnibus, which sequenced assisted-capture as trailing an unshipped web brief; the *server* task endpoints exist — only the CLI face and the connect flow are missing.)

- `GET /api/v1/automations/tasks` — **list**, paginated `{ data, page, per_page, total }`, with `?status=` and `?automation=` filters, most-recent first.
- `GET /api/v1/automations/tasks/pending` — **the inbox default**: the `pending` scope, same envelope. This is what the ambient count and the triage list read.
- `GET /api/v1/automations/tasks/:id` — **show** one draft.
- `PATCH /api/v1/automations/tasks/:id/acknowledge` — **mark seen** (stamps `acknowledged_at`). No body. `422` (`"Cannot acknowledge task in <status> status."`) if the task isn't acknowledgeable.
- `PATCH /api/v1/automations/tasks/:id/complete` — **accept**. Optional body `{ "resolution": "completed", "response": { … } }` (permitted `response` keys: `value`, `selected_option`, `notes`, `reason`, `confirmed`, `subdomain_id`, `domain_id`, `classification`, `values[]`, `selected_options[]`). Fires the automation's `on_complete` hook — *this is the write that mints the activity.* `422` (title "Cannot complete task") otherwise.
- `PATCH /api/v1/automations/tasks/:id/reject` — **discard**, optional body `{ "reason": "…" }`. `422` (title "Cannot reject task") otherwise.
- **Task object shape** (both `show` and list rows): `{ id, automation, status, prompt, context, response, resolution, entity: { id, type, name }, created_at, acknowledged_at, completed_at, expires_at }`. `prompt` is the human-facing question, `context` the proposed draft's detail, `entity` the taskable it targets, and **`expires_at` is the due-badge source** — the field that lets the Review triage grammar transfer wholesale.

### Decision record — two things to raise, not silently resolve

- **The git-source connect flow has no verified server contract yet.** The `namespace :automations` block exposes only task *consumption* — list / show + acknowledge / complete / reject + pending — with **no route for registering a git source**. The controller's own header notes tasks are "created in-process by `AutomationJob`." So job 2's connect flow is designed against an unspecified server contract: does a source registration live server-side (a new endpoint the `engineer` team must add), or is it purely local client config that the server-side git detector reads? This is the one real cross-repo dependency in the module — surface it in the design pass and the handoff; don't invent an endpoint.
- **Accept is a server-side write; address the draft, re-read after.** `complete` runs `on_complete` on the server and leaves the draft in a terminal `completed` status; a second attempt is the `422` "Cannot complete task in <status> status." So the CLI treats accept as fire-then-re-read — refresh the pending list (the draft leaves the scope) rather than trusting a cached row, and surfaces the `422` as "this draft already moved on," not a hard error. Same shape as the Progress module's "adjust may mint a successor" rule.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html` / `terminal-tokens.css` the omnibus cited no longer exist). Assemble from shipped atoms only: `bordered()` panels, the Review triage list + queue-preview rows, `status_pill` badges (the `expires_at`-driven due-badge), full-row inverse selection with `▌`, `notify` tiles for the ambient count and the post-accept confirmation. Keyboard-only, neovim-flavoured (`j` / `k` / `⏎` to move and open, `x` to reject, `a` to acknowledge); the footer advertises the active keys. ASCII-only diagrams. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see the cross-cutting brief.

---

## 7. Out of scope

Designing the server's automation **sources / detectors** — how a commit becomes a draft (that's the `AutomationJob` pipeline in `engineer`; the CLI *consumes* tasks, it doesn't redesign the pipeline). Redesigning the **web** assisted-capture surfaces (a sibling surface, not this one). Editing the accepted activity after the fact (that belongs to the activities / audit modules). **Creating or deleting** tasks (the API is index/show + the three state verbs + pending — no `create` / `destroy`; the CLI never authors drafts). Light mode.

---

## 8. Phasing

This is Phase 2, and it **warrants its own design pass** — an `assisted-capture.dc.html` board — as the omnibus called out. It inherits this repo's terminal grammar (the Review triage list, `notify` tiles, the timer's ambient count, the headless-twin contract) and specifies only what's genuinely new: the draft-triage screen and the git-source connect flow.

1. **The draft-triage screen (job 1) — unblocked.** The pending list + open-one view + acknowledge / accept / reject verbs + the `:inbox` palette route + the `engineer inbox …` headless twins + the ambient count, all grounded on the shipped `/api/v1/automations/tasks` routes above. Track as its own epic (the repo's pattern: a `*.dc.html` design pass → gap brief → epic, as `timer.dc.html` + timer did).
2. **The git-source connect flow (job 2).** The terminal-native step that makes drafts *appear* — designed once the source-registration contract from §5's decision record is settled with the `engineer` team. The genuinely-terminal-specific half of the brief, and its one cross-repo dependency.
