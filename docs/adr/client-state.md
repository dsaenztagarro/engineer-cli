# Client state

**Status:** Accepted · **Decision log:** [#211](https://github.com/dsaenztagarro/engineer-cli/issues/211)

What the client may hold locally: nothing derived, pending write intents only until they sync, and the last-known timer as a read fallback.

## Context

Every number this client shows — pace, freshness, planned-vs-done, today's totals, the segment-audit flags — is recomputed server-side from segments on every read.
The client renders them and keeps no second copy.
That rule is what makes the terminal and the web agree to the minute, and it is why a bug in a rollup is fixed in one place.

Offline breaks the rule's easy version.
The user studies on trains: the wifi drops in a tunnel and comes back at the next station.
A write that is a live round-trip simply errors when the wire is down, so a `stop` in a tunnel would lose the session.
Making writes survive a drop requires the client to hold *something* locally, which is exactly what "never keep a second ledger" forbids.
The whole question is what it is allowed to hold, and for how long.

## Decisions

### The client keeps no derived state

Nothing computed — a total, a pace reading, a schedule — is ever written to disk.
Reconcile *toward* the server; re-read rather than trust local.

### Pending intents are the one exception, held only until they sync

The queue (`src/queue/`) stores *what the user asked for*, never an actual.
The moment an intent lands on the server it leaves the queue, and a replayed write is confirmed by the server's response, not by local optimism.

### Queued work is composed at read time, never written into a cache

Pending intents are folded over fetched pages at render (`queue::fold_activities`), so queued work is visible where you look: a provisional row reads `◔ … provisional · queued`, and a queued segment's minutes ride beside the confirmed duration rather than being summed into it.
Rows settle to plain on the next fetch after the drain.

### One clock, offline too

The local clock uses `engineer`'s study-day boundary and the same elapsed/paused arithmetic the server does, so a locally-run session agrees with the server to the second on reconcile (`src/timer_clock.rs`).
Local advancement is the same definition of time computed client-side, not a second one.

### A reconciliation never silently loses a segment

A reconciliation that has to drop or merge something says so and lets the user choose; a conflict surfaced loudly beats a segment vanishing quietly.
A `422` on replay becomes the edit / drop / skip choice (`src/queue/resolve.rs`), never a discard, and parking a diverged intent is a user-chosen outcome (`skip`), never an automatic one.

### Replay is per-stream FIFO: a divergence blocks its stream, not the queue

In-stream order is never violated, and independent streams keep replaying past a stuck one.
An intent still referencing a provisional (negative) id is held explicitly rather than posted against a guess.
A transport failure halts the whole pass, because offline is global (`src/queue/replay.rs`).

### `queued` and `diverged` are first-class honest states

They join `paused / idle / over / behind / stale / offline`, and surface in `--json`, the plain status line and exit codes exactly as `stale` does, so a script watching a status bar sees "3 writes pending, offline" without the TUI ([terminal-surface](terminal-surface.md)).

### A write is queued only if its outcome can be synthesized honestly

Where the client cannot synthesize a *truthful* provisional response, the verb refuses offline with the way forward rather than inventing an outcome.
Adding a write means answering that question first: if yes it rides the queue; if no it refuses with guidance and joins this table.

| Live-only write | Why a queued outcome would be a lie |
|---|---|
| A review rating | The server computes the next-due schedule the rating sets |
| Inbox triage (accept / reject / acknowledge) | Accept mints an activity via a server hook; a stale-draft `422` cannot be predicted |
| Capture-source connect / disconnect / sync | The registration and the scan are server-side |
| Note and segment hard deletes | Destructive and terminal — there is no honest provisional "deleted" |
| Timer reclaim | The idle verdict is the server's |
| The presence heartbeat | Meaningless offline |

### One queue, at the typed-method seam

Notes, books, targets, week notes, plan items, the timer and the activity lifecycle verbs all ride the same `QueuedClient` seam over the typed API methods, and the same reconcile shape.
Each write returns a different typed resource the caller synchronously consumes, so the queue sits one layer above the transport, where it can hand the caller a provisional one.

### `duplicate` replays plain, and a lost-ack double-fire is accepted

`duplicate` mints a new resource and is not in the server's `Idempotency-Key` opt-in set (engineer's [api-wire record](https://github.com/dsaenztagarro/engineer/blob/master/docs/adr/api-wire.md)), so a lost-ack re-fire on replay can make a second copy.
That is accepted over refusing the gesture offline: a duplicated activity is a visible, archivable planned copy, never double-*counted* like a logged segment, so never silently losing the gesture wins.

### The provisional-to-real id map lives on the intents

It is never a second ledger — derived state stays derivable — and it is stitched in the same writer-locked mutation that removes the acked create, so an interrupted drain resumes consistently.

### The queue board and `engineer queue` render one read

`src/app/screens/queue.rs` and `engineer queue` render the same `queue::pending()` read, shaped once in `queue::view`, so the board and the verb can never drift.

### The last-known timer is restored only on a transport error

The read half (`src/timer_cache.rs`) is a small persisted cache, restored **only** on `ApiError::Transport`, so auth and server errors still propagate rather than being papered over with stale data.

### The refresh token is the one secret the client persists

It lives in the OS keyring under the identity host; the access token stays in memory and is refreshed on demand, and no token is ever written to the log. Losing the process loses nothing a refresh cannot rebuild, and nothing on disk or in a log can act as the user.

## Rejected

- **A byte-level interceptor at the transport.** Each write returns a different typed resource (`start_timer -> Timer`, `stop_timer -> TimerStopped`, `create_activity -> Activity`), and a blind interceptor cannot hand the caller anything to proceed with.
- **A full local mirror of the domain, synced both ways.** It is the second ledger, with every divergence bug that implies, to buy offline support for a client whose reads are already cached.
- **Refusing all writes offline.** It fails the honesty and glance-or-gesture bar for the client's headline case; it remains the correct answer for the live-only set above.
- **Parking a divergence silently and draining the rest.** A diverged intent is loud, kept and resolvable; silence loses a segment.
- **Halting the entire drain at the first divergence.** A diverged activity write would hold the timer stream hostage.
- **A token file beside the config.** A plaintext file any process of the user's can read, for no gain over the keyring the platforms already provide.

## References

`src/queue/` · `src/timer_clock.rs` · `src/timer_cache.rs` · `src/app/screens/queue.rs` · `src/auth/` · `tests/log_redaction.rs`
