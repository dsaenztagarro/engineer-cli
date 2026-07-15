# Design brief — engineer-cli · Offline write side (the local-clock timer + the optimistic write queue)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal states and headless surfaces of writing offline — the timer as a genuine **local clock** that controls locally and reconciles on reconnect, a persisted **write queue** every mutation rides, and the **divergence/reconcile** moments when local and server disagree. Extend `../../design-system.dc.html` (the style anchor); the timer states are the natural extension of `../../timer.dc.html`, and the queue/reconcile surfaces want their own board (`offline-write.dc.html`, mirroring how `timer.dc.html` is one board per area).
**Status:** proposed — this is the **write** half of offline-tolerance; the **read** half shipped (`src/timer_cache.rs`, #91 → v0.7.0). Tracking issue **#96** (follow-up to **#81**). It is *not* one shippable screen: it is the largest single change to the client's I/O model and decomposes into its own epic (the repo pattern — `.dc.html` pass → brief → epic).

> **Module note.** This brief graduates **§A of `cross-cutting.brief.md`** ("Offline-tolerance & the local clock") into its own module brief, the way `timer.brief.md` absorbed the standalone timer gap analysis — because §A itself said this work "deserves its own gap brief and epic." The cross-cutting brief now keeps §A as the one-paragraph shared principle and points here for the detail. The shared house format — workflow → jobs → principles → orientation (shipped reality) → the API it consumes → visual language → phasing — is common to every module brief, and the kit constraints (`../../README.md`, `../../design-system.dc.html`) bind here as everywhere: character grid, keyboard-only, dark-first, ASCII-only diagrams, shipped chrome and widget idioms.

---

## 1. Who this is for (the workflow)

> "I study on trains — the wifi drops in a tunnel and comes back at the next station.
> When it's gone I still want to start, pause, and stop my timer with a keystroke; the clock keeps counting in my zellij status bar, just marked stale — it doesn't freeze and it doesn't throw an error.
> When the signal comes back, everything I did offline syncs on its own.
> And if the server and I end up disagreeing, tell me plainly and let me choose — never silently drop a segment I logged."

The throughline: the timer workflow (`../shipped/timer.brief.md`) already promises "the timer is a local clock; reconcile it with the server when the network comes back, don't lose my session." Today that promise is only half-kept — reads survive a drop (the last-known clock renders stale instead of blank), but **every write is still a live round-trip that just errors when the wire is down.** Closing the write half is what makes the train case true end-to-end: control offline, queue the intent, reconcile honestly on reconnect. It is briefed as its own module — not folded into timer — because the same queue-and-reconcile shape is what a `log`, a `target` adjust, or a note write will ride; the timer is only the most acute case.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**1. Control the timer entirely offline.** Start / pause / resume / stop / bind / discard act against **local state** and never refuse the keystroke for lack of network. The clock is a start time plus accumulated paused seconds that renders *and* advances locally, so the hero, the header cell, and the `--short` string all keep moving offline — not just the display-smoothed tick between polls, but the source of truth while the wire is down.

**2. Never go blank offline (reads).** The header cell and the `--short` status string show the last-known clock with a staleness marker rather than nothing. ***Shipped*** in #91 — `src/timer_cache.rs`, the `stale` / `stale_age_s` fields in `--json`, the ` ~` glyph and `· offline (last known 2m ago)` suffix. It is job 2 of three; this brief designs jobs 1 and 3 around it, and the write side must keep the read cache honest (a queued-but-unsynced local clock is a *different* honesty state than a merely stale read — see §3).

**3. Queue every write as a deferred intent, and replay on reconnect.** A `start` / `pause` / `stop` with no network becomes a **pending intent**, persisted (so it survives exit, which in-memory state does not), replayed in order when the network returns — not an error. The pending state is visible, never invisible: the user can always see there is unsynced work and roughly how much.

**4. Reconcile honestly on divergence.** When replay finds the server has moved on — a session started on the web, a segment the server rejects, a clock that drifted — the reconciliation **says so and lets the user resolve it**; it never silently drops or merges a segment. This is the honesty deliverable, and the hardest screen: design the *conflict* moment, not just the happy replay.

*(These mirror #96's acceptance criteria: timer start/pause/resume/stop work fully offline and reconcile on reconnect; queued writes replay on reconnect with conflicts surfaced, never silently dropped.)*

---

## 3. Principles that genuinely bind

- **Honest states, extended (the governing test, `cross-cutting.brief.md` #4).** `paused / idle / over / behind / stale / offline` already render truthfully; this work adds **`queued` (unsynced local intents pending)** and **`diverged` (reconciliation needs a choice)** to that vocabulary. The design never hides one to look tidy — "the honest status quo is: … fail the write when the wire is down" is what we are replacing, not papering over.
- **Never silently loses a segment.** The load-bearing promise of §A: a reconciliation that has to drop or merge something *tells the user and lets them choose*. Silent data loss is the one failure this whole track exists to prevent — a conflict surfaced loudly beats a segment vanishing quietly.
- **A queue of intents is not a second ledger.** The Measure pillar's rule is "derived, never stored — the client never keeps a second ledger" (`progress.brief.md` §3). The write queue is the deliberate, scoped exception: it stores **pending intents**, not derived actuals, and only until they sync. The server stays authoritative; the moment an intent lands, it leaves the queue. Reconcile toward the server, re-read rather than trust local, and keep the queue as small and short-lived as the network allows.
- **One clock, offline too.** The local clock must use engineer's study-day boundary and the same elapsed/paused arithmetic the server does (the `Timer` shape — `started_at`, `elapsed_seconds`, `paused_seconds`, `paused_at`), so that when it reconciles, a locally-run session agrees with the server to the second. Local advancement is not a second definition of time; it is the same definition, computed client-side while offline.
- **Glance or gesture, not a sync console.** Queued and diverged are a **glanced complication** (a marker on the status string, a small count) and a **one-gesture resolve** (pick a side, ⏎), never a lean-back sync-manager UI. Depth — a full history of every reconciled write — stays on the web. This passes the sterling-not-a-replica test or it belongs on the web.
- **The headless twin is half the feature (`cross-cutting.brief.md` §C).** `queued` / `diverged` must surface in `--json` (a machine field), in the plain status line, and in exit codes, exactly as `stale` already does — a script watching the status bar must be able to see "3 writes pending, offline" without the TUI.

---

## 4. What's already in the app (orientation)

- **Shipped (the read half, #91):** `src/timer_cache.rs` — `store(&Timer)` on every successful headless read, `load() -> StaleTimer { timer, age_secs }` on a network miss, persisted as `timer-cache.json` in the XDG state dir (best-effort, errors silent). Wired at `src/timer_cli.rs::fetch_timer`, which falls back to the cache **only on `ApiError::Transport`** (network) — auth/other errors still propagate. Its module header already names this brief's scope: *"the full local clock (control offline) and the optimistic write queue are their own follow-up."* This is the pattern the queue mirrors — a small persisted sibling module, not a framework.
- **Display-smoothing, not control (to be upgraded):** `live_elapsed` (`src/app/screens/timer.rs`) ticks the *displayed* elapsed from the last snapshot + a monotonic `Instant`, freezing while paused; `TIMER_POLL_INTERVAL` (~15s) and `HEARTBEAT_INTERVAL` (~60s) in `src/app/mod.rs` re-poll and beat presence. These smooth the seconds between polls but are **not** a source of truth — job 1 turns this display tick into a controlling local clock.
- **State is in-memory and lost on exit.** There is no persistence layer, no intent log, no queue anywhere (`grep queue|offline|optimistic|reconcile|pending|sync` finds only `timer_cache.rs` and unrelated key-chord state). The queue is net-new.
- **Two client surfaces inherit this, not one.** Writes fire from the **TUI event loop** (`src/app/screens/timer.rs` `spawn_op`/`spawn_stop`/`spawn_start_blank`, plus activities/audit/notes/etc.) *and* the **headless CLIs** (`src/timer_cli.rs`, `target_cli.rs`, `log_cli.rs`, `week_cli.rs`, `inbox_cli.rs`). The read cache lives only in the headless path today; the write queue has to serve both, which is a real design question (a shared queue both surfaces enqueue to, reconciled by whichever process is live).
- **Transport already distinguishes the offline case.** Every write funnels through `src/api/mod.rs` (`post`/`patch`/`delete`/`post_empty`/`patch_empty` → `send()`); a pre-response network failure is `ApiError::Transport`, a server rejection is `ApiError::Problem { status, title, detail, … }` (RFC 7807). The queue keys "defer" on `Transport` exactly as `fetch_timer` keys "fall back to cache" on it — the seam already exists.

---

## 5. The API it consumes (verified against `src/api/*.rs`)

Every write is one of ~33 typed mutation methods; this track's first cut is the **timer + segment + activity** subset (#96 names these three), enqueued rather than issued live when offline. Client-side routes verified in the source:

- **Timer** (`src/api/timer.rs`) — `POST /api/v1/timer` (start), `POST /api/v1/timer/{pause,resume,stop,bind,heartbeat,phase,mode,reclaim}`, `DELETE /api/v1/timer` (discard). `stop`/`reclaim` return a distinct `TimerStopped`; the rest return the updated `Timer`.
- **Segments** (`src/api/segments.rs`) — `POST /api/v1/activities/:activity_id/segments` (create), `PATCH` / `DELETE /api/v1/activities/:activity_id/segments/:id`. A timer `stop` writes a segment, so the timer path already reaches into this module.
- **Activities** (`src/api/activities.rs`) — `POST /api/v1/activities` (create), `POST …/:id/{complete,duplicate}`, `PATCH …/:id/{archive,unarchive}`.

**The blast radius is the typed responses, not the transport.** A queue cannot be a blind byte-level interceptor at `send()`: each write returns a *different typed resource the caller synchronously consumes* to update the UI or set an exit code (`start_timer → Timer`, `stop_timer → TimerStopped`, `create_segment → a segment`, `create_activity → Activity`). Offline, the optimistic layer has to **synthesise a plausible local response** (a local clock, a provisional segment) so the caller can proceed, then reconcile it against server truth on replay. Designing what the client shows for a *provisional* vs a *confirmed* write is the core of jobs 3–4.

### The open server contract (raise, don't resolve — the canonical backend gap)

Replay-and-reconcile needs the **server** to be safe under it, and that contract is **not verified here** (it lives in the `engineer` repo, and this client only renders what the API serves):

- **Idempotency under replay.** If a queued `POST /api/v1/timer` or a `create_segment` is re-sent after the server already recorded it (ack lost on a flaky link), does the server dedupe (an idempotency key), or does it double-write? Without a guarantee, replay can duplicate segments — the opposite of "never lose one."
- **A structured conflict the client can render.** When the server has moved on (a session started elsewhere, a stale timer version), does it return a machine-readable conflict (like targets' `422 ClosedVersionError`, `progress.brief.md` §5) the reconcile screen can turn into a choice — or just a generic error?

Per the `/epic` skill's backend-gap rule (`.claude/skills/epic/SKILL.md` step 3.2), any endpoint or contract the API doesn't expose is **recorded as a blocked ticket, not faked client-side.** So the design should mark the reconciliation semantics it *needs* from the server as an explicit request, exactly as the timer gap analysis's Section C did before the hygiene API shipped — the queue's client mechanics can land ahead of it, but honest divergence resolution may be gated on it.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor). Assemble from shipped atoms only: `bordered()` panels, `status_pill`, `notify` tiles, full-row inverse selection with `▌`, and the existing **staleness idiom** the read half already renders (the ` ~` glyph, the `· offline (last known 2m ago)` suffix) — `queued` and `diverged` should read as the same family, not a new visual language. Keyboard-only, neovim-flavoured; the footer advertises the active keys; ASCII-only diagrams. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to design *to `105`* and flag, not silently resolve — see `cross-cutting.brief.md` §D. The offline/queued/stale markers must stay **quiet** (a small ambient signal, `behind`-loud at most), never a nag — the sterling-not-a-replica rule.

---

## 7. Out of scope

The read cache itself (shipped, #91 — extend it, don't redesign it). The other cross-cutting concerns own their own detail: the fuzzy picker (`cross-cutting.brief.md` §B), the accent-hue ratification (§D), the `$EDITOR` handoff (§E). A full sync-history / audit-of-every-reconciled-write surface — that is web depth, not a terminal glance. Web-side CRUD depth generally. Light mode. The long-tail write paths (notes / targets / books / review / inbox) are **not out of scope but phased** — they ride the same queue in a later phase (§8), not a different mechanism.

---

## 8. Phasing

Ordered foundation-first, each phase carrying **both** its TUI states and its headless twin (the §C contract is half the feature, not a follow-up). The epic decomposes these into self-contained, individually-shippable tickets per the `/epic` skill.

1. **Foundation — the persisted intent queue + reconciler.** A small module sibling to `timer_cache.rs`: a durable, ordered log of pending write intents in the XDG state dir, a replay-on-reconnect pass keyed on `ApiError::Transport`, and the `queued` / `diverged` state vocabulary surfaced in `--json` + plain + exit codes. Shared by both client surfaces. This is shared machinery every later phase builds on — the "shared vocabulary first" the epic orders foundation before surfaces.
2. **Phase A — the timer as a local clock (the train case; highest value, standalone).** Turn `live_elapsed` and the poll/heartbeat loop into a controlling local clock; make timer start/pause/resume/stop/bind/discard act locally and enqueue; design the reconcile-on-reconnect and the divergence moment for the timer specifically (a session started elsewhere, a drifted clock). Touches the 10 timer methods + `create_segment`/`delete_segment` (stop writes a segment). Ships the headline "study on trains" value on its own.
3. **Phase B — segment & activity writes on the same queue.** `engineer log` (create_activity + create_segment), `engineer week plan`, the activities table and segment audit — the same queue-and-reconcile shape, now that the timer proved it.
4. **Phase C — the long tail.** notes / targets / books / review / inbox writes ride the identical mechanism. Framed here (not smuggled into each module brief) precisely so it is one queue, not five.

Gating note: Phases A–C can land the client-side queue mechanics incrementally, but **honest divergence resolution (job 4) may be gated on the server contract** in §5 — if the API can't express idempotency or a structured conflict, that becomes a blocked ticket recorded in the epic, not a client-side fake.
