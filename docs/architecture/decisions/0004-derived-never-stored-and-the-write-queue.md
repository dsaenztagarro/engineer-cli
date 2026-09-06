# 0004 — Derived, never stored — and the offline write queue as its one exception

**Status:** Accepted (read half #91 → v0.7.0; write half EPIC #98 → v0.8.0 + v0.10.0; recorded here at #180)

## Context

Every number this client shows — pace, freshness, planned-vs-done, today's totals, the segment-audit flags — is recomputed server-side from segments on every read.
The client renders them and keeps no second copy.
That rule is what makes the terminal and the web agree to the minute, and it is why a bug in a rollup is fixed in one place.

Offline breaks the rule's easy version.
The user studies on trains: the wifi drops in a tunnel and comes back at the next station.
Reads survived that from #91 — the last-known timer renders stale rather than blank — but **every write was a live round-trip that simply errored when the wire was down**, which meant a `stop` in a tunnel lost the session.
Making writes survive a drop requires the client to hold *something* locally, which is exactly what "never keep a second ledger" forbids.
The whole question is what it is allowed to hold, and for how long.

## Decision

**The client keeps no derived state. It keeps *pending intents* — and only until they sync.**

That is the one deliberate, scoped exception, and its boundaries are what make it safe:

- **Intents, not actuals.** The queue (`src/queue/`) stores *what the user asked for*, never a computed total, a pace reading, or a schedule. Nothing derived is ever written to disk.
- **Short-lived by construction.** The moment an intent lands on the server it leaves the queue. Reconcile *toward* the server; re-read rather than trust local.
- **The server stays authoritative.** A replayed write is confirmed by the server's response, not by local optimism.
- **Read-time composition, never a cached write.** Pending intents are folded over fetched pages at render (`queue::fold_activities`) so queued work is visible where you look — a provisional row reads `◔ … provisional · queued`, a queued segment's minutes ride beside the confirmed duration rather than summed into it. Nothing provisional is written into a cache; rows settle to plain on the next fetch after the drain.

Four rules govern the mechanism:

1. **One clock, offline too.** The local clock uses `engineer`'s study-day boundary and the same elapsed/paused arithmetic the server does, so a locally-run session agrees with the server to the second on reconcile. Local advancement is not a second definition of time — it is the same definition computed client-side (`src/timer_clock.rs`).
2. **Never silently lose a segment.** A reconciliation that has to drop or merge something *says so and lets the user choose*. This is the load-bearing promise; a conflict surfaced loudly beats a segment vanishing quietly. A `422` on replay becomes the edit / drop / skip choice (`src/queue/resolve.rs`), never a discard.
3. **Per-stream FIFO — a divergence blocks its stream, not the queue.** In-stream order is never violated; independent streams keep replaying past a stuck one; an intent still referencing a provisional (negative) id is held explicitly rather than posted against a guess; a transport failure halts the whole pass, because offline is global (`src/queue/replay.rs`).
4. **`queued` and `diverged` are first-class honest states.** They join `paused / idle / over / behind / stale / offline` in the vocabulary, and they surface in `--json`, the plain status line, and exit codes exactly as `stale` does — a script watching a status bar can see "3 writes pending, offline" without the TUI ([ADR 0003](0003-tui-headless-contract.md)).

**The can-we-synthesize-honestly test — which writes stay live-only.** A write is queued only if the client can synthesize a *truthful* provisional response. Where it cannot, the verb refuses offline with the way forward spelled out, rather than inventing an outcome:

| Live-only write | Why a queued outcome would be a lie |
|---|---|
| A review rating | The server computes the next-due schedule the rating sets |
| Inbox triage (accept / reject / acknowledge) | Accept mints an activity via a server hook; a stale-draft `422` cannot be predicted |
| Capture-source connect / disconnect / sync | The registration and the scan are server-side |
| Note and segment hard deletes | Destructive and terminal — there is no honest provisional "deleted" |
| Timer reclaim | The idle verdict is the server's |
| The presence heartbeat | Meaningless offline |

## Alternatives considered

- **A byte-level interceptor at the transport (`send()`).** Rejected: each write returns a *different typed resource the caller synchronously consumes* (`start_timer → Timer`, `stop_timer → TimerStopped`, `create_activity → Activity`). A blind interceptor cannot hand the caller anything to proceed with. The queue therefore sits one layer up, as the `QueuedClient` seam over the typed methods.
- **A full local mirror of the domain, synced both ways.** Rejected outright: it is the second ledger, with every divergence bug that implies, to buy offline support for a client whose reads are already cached.
- **Refuse all writes offline (the status quo ante).** Rejected: it fails the honesty and glance-or-gesture bar for the client's headline case. Note that this remains the *correct* answer for the live-only set above — the decision is not "queue everything", it is "queue what can be synthesized honestly".
- **Park a divergence silently and drain the rest.** Rejected: it violates rule 2. A diverged intent is loud, kept, and resolvable; parking is a user-chosen outcome (`skip`), never an automatic one.
- **Halt the entire drain at the first divergence** (the first implementation). Superseded by per-stream FIFO: a diverged activity write should not hold the timer stream hostage.

## Consequences

- One queue, not five: notes, books, targets, week notes, plan items, the timer, and the activity lifecycle verbs all ride the same `QueuedClient` seam and the same reconcile shape.
- `duplicate` is a knowing exception on the idempotency side: it mints a new resource and is not in the server's `Idempotency-Key` opt-in set (`engineer` ADR 0036), so a lost-ack re-fire on replay can make a second copy. That is accepted over refusing the gesture offline — a duplicated activity is a visible, archivable planned copy, never double-*counted* like a logged segment — so the never-silently-lose-the-gesture invariant wins.
- The provisional→real id map lives on the intents, never in a second ledger — derived state stays derivable — and is stitched in the same writer-locked mutation that removes the acked create, so an interrupted drain resumes consistently.
- The queue's own face (`src/app/screens/queue.rs`, `engineer queue`) renders the *same* `queue::pending()` read the CLI table prints, shaped once in `queue::view`, so the board and the verb can never drift.
- The read half (`src/timer_cache.rs`) is the sibling pattern and stays as it is: a small persisted cache, restored **only** on `ApiError::Transport`, so auth and server errors still propagate rather than being papered over with stale data.
- Adding a write means answering one question first: *can this be synthesized honestly offline?* If yes it rides the queue; if no it refuses with guidance and joins the table above.
