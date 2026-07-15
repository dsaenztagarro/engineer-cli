# Design brief — engineer-cli · Cross-cutting concerns (the things every module inherits)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** not a screen — the shared concerns every module brief inherits rather than owns: **the governing product principle** (sterling, not a replica), offline-tolerance / local-clock reconciliation (the read half shipped; the write half is now owned in detail by `offline-write.brief.md`), one reusable fuzzy picker, the TUI ↔ headless contract, the accent-hue decision, and the `$EDITOR` handoff for prose. There's no `.dc.html` board of its own; this brief constrains all the others. Kit anchor is `../../design-system.dc.html`.
**Status:** proposed. These are decisions to **ratify** and follow-ups to **schedule** — not one shippable screen.

> **Module note.** Unlike the other per-module briefs (see `../README.md`), this one is not a screen. It collects the concerns that cut across [progress](progress.brief.md), [timer](../shipped/timer.brief.md), [notes](notes.brief.md), and [week-planning](week-planning.brief.md) — the things each of them leans on rather than defines. Where a concern is owned in detail by a single module, this brief states the shared principle and points there. It uses section headers (§A…§E) in place of the house workflow → jobs → API skeleton, but keeps the same voice and the same kit constraints (`../../README.md`, `../../design-system.dc.html`): character grid, keyboard-only, dark-first, ASCII-only diagrams, and the shipped chrome and widget idioms.

---

## The governing principle — sterling, not a replica (the glance-or-gesture test)

This is the law every other brief is measured against, and the one this whole client exists to honour. The exemplar is already shipped: the **timer** (`../shipped/timer.brief.md`). Its value is an ambient string you glance at, verbs that cost one keystroke, states that never lie, offers that never fire on their own, and a headless twin you pipe into a status bar. That is the bar. Think of it as an **Apple Watch** for the study loop, not a shrunk-down web app: the win is *sterling behaviour and glanceable value*, not feature-parity with the `engineer` web UI.

**A full terminal replica of the web is a non-goal — explicitly.** The web app is the cockpit: it owns editing depth, rich filtering, bulk operations, dashboards, and the long tail of CRUD. The terminal owns the *high-frequency, high-value* core of the study loop, distilled. When a feature could be either "port the web surface" or "distil the one thing that matters here," the terminal always takes the second.

**The test — a feature earns its place only if it is:**

1. **A glance or a gesture, not a session.** Value lands in one look (a complication — a pace meter, the timer string, a due count) or one keystroke (a verb), not a lean-back editing workflow. If it needs filtering, sorting, and paging to be useful, ask what single glance or gesture it's standing in for.
2. **Ambient & quiet.** Present without being opened; calm when on-track; a small signal when not; never a nag. On-pace is silence, `behind` is as loud as it gets — the shipped rule.
3. **Distilled, not ported.** It answers the *one question that matters in the terminal*. Depth (the full table, the analytics grid, the planning canvas, the settings form) stays on the web; the terminal keeps the daily core.
4. **Honest.** Paused / idle / over / behind / stale / offline render truthfully — the design never hides a state to look tidy.
5. **Composable — the terminal's edge over a watch.** "Distilled" does not mean "less powerful." Every read is pipeable and every action is a headless verb (§C), so power that a watch would lose to a small screen moves to `jq`, git hooks, and status bars instead of to more on-screen chrome.
6. **One-hand, keyboard-only, muscle memory.** `j/k`, `/`, `:`, `<Space>` leader, `i`/`Esc` — no mouse, no bespoke keymap.

**Where each module sits against the test (the map — details in each brief):**

- **Exemplars (keep as designed):** timer; **home** (the watch face — leads with the timer + pace complications, links out to depth); **progress** pace meters + adjust-in-place; **review**'s rate sitting (card → `f/z/s/i` → next → clean exit); **notes** capture + `$EDITOR`; the **command palette**; and this brief's machinery.
- **The one deliberate concession:** the **activities table** (`../shipped/activities.brief.md`) — the closest thing to a web surface in the client. It is shipped and it earns a place as *the one lean-back ledger you scan and act on by the row*, but it is **capped**: it must not grow toward web parity (rich saved filters, bulk edit, server-side search). Its watch-native core is **capture** (`engineer log`, `t` bind) and the **audit** (flagged-segment triage — a small list you clear to empty). That brief's §3 makes the cap a principle.
- **Reframes this principle forces (in the drifting briefs):** progress's **time-explorer** is a *glance + a `--json` rollup*, not a pivot grid (`progress.brief.md` §2 job 7); **week-planning** is a planned-vs-done *readout* + one-liner declare + start-a-planned-item + an `$EDITOR` reflection, **not** a planning canvas (`week-planning.brief.md`); **review browse-all** and the **notes browser** stay thin, explicitly-secondary indexes; the **assisted-capture** inbox stays a queue you clear, never an inbox manager.

Every new feature, and every growth of an existing one, is checked against this before it is designed. A surface that fails the test belongs on the web.

---

## §A — Offline-tolerance & the local clock

> "It has to survive a dropped connection — I study on trains: the timer is a local clock; reconcile it with the server when the network comes back, don't lose my session." — the timer workflow (`../shipped/timer.brief.md`)

Offline-tolerance splits in two halves. The **read** half **shipped** (#91 → v0.7.0): reads cache their last-known value, so the header cell and the `--short` string show the last-known clock with a staleness marker — never blank offline (`src/timer_cache.rs`; `stale` / `stale_age_s` in `--json`). The **write** half is **not built**: the timer is not yet a controlling local clock, and there is **no offline write queue** — a `start`/`pause`/`stop` with no network is just an error, not a deferred intent. What smooths the running clock today (`live_elapsed`, the ~15s `TIMER_POLL_INTERVAL`, the ~60s `HEARTBEAT_INTERVAL`, all in `src/app/`) is *display-smoothing, not a source of truth*.

This concern is now **owned in detail** by [`offline-write.brief.md`](offline-write.brief.md) (which specifies the timer as a genuine local clock, the persisted optimistic write queue every mutation rides, and the reconcile/divergence surfaces — the largest single change to the client's I/O model, tracked as its own epic, #96). It is stated here only as the shared principle, because it constrains every module: **the design never draws the write side as shipped, and a reconciliation that has to drop or merge something says so — it never silently loses a segment.** The same queue-and-reconcile shape a `target` adjust, a `log`, or a note write will ride is *one* mechanism, briefed there rather than smuggled into any one module (the repo pattern: a `.dc.html` pass → brief → epic).

---

## §B — Fuzzy pickers (Telescope-flavoured)

**Today there are two unrelated match surfaces, and neither is a local fuzzy finder:**

- **Server-side candidate match** for the timer bind — `timer_candidates` → `GET /api/v1/timer/candidates?q=…` (`src/api/timer.rs`), which the interactive bind flow and `engineer timer start <query>` / `bind <query>` both drive.
- **Client-side `/` narrowing** on the Activities and Notes lists — but this is a plain case-insensitive **substring** filter over the already-loaded page (`matches_query` in `src/app/screens/activities.rs` is `to_ascii_lowercase().contains(q)`), not a fuzzy rank. It narrows what's on screen; it does not *find* across a corpus.

There is **no local fuzzy finder over books / repos / domains** anywhere. Choosing a book to bind, a repo to attribute, or a domain for a target is either a server round-trip (timer candidates) or an arrow-through list — the two things the kit's **"fuzzy over navigate"** principle exists to replace.

**The concern:** every module reaches for the same gesture — bind a timer, choose a book / repo / domain, jump to an activity — and each is currently reinventing (or lacking) it. Specify **one reusable picker widget** the way `bordered()` and `pace_bar` are reusable atoms:

- A Telescope-flavoured overlay any screen can invoke with a source and a callback: `j`/`k` to move, type to filter, `⏎` to pick, `Esc` to cancel — the neovim grammar the footer already advertises.
- A **fuzzy** rank (subsequence match with a sensible score), not the substring `contains` the list narrows use today — so `dda` finds "Designing **D**ata-Intensive **A**pplications".
- Source-agnostic: fed a local slice (books, repos, domains, loaded activities) **or** a server candidate stream (the timer's `candidates`) behind one interface, so a module picks a source, not a bespoke screen.
- Rendered from shipped atoms only — full-row inverse selection with `▌`, dim-vs-bright contrast, one font size — so it reads as the same app in every module that mounts it.

Owning this as a widget (not a per-screen re-implementation) is what lets `progress`'s "choose a domain for a target", `timer`'s bind, and `week-planning`'s "pick a planned item" all feel identical without four copies of the logic.

---

## §C — The TUI ↔ headless contract

The duality is **first-class**: every read the TUI shows must **also** be a non-interactive one-shot, in a machine form (`--json`) and a pipe form (plain text). This is **shipped and proven** for `engineer timer` — treat `src/timer_cli.rs` as the reference implementation, and this section as the checklist the other module briefs point back to.

**What the reference already does (copy it):**

- **TTY-detected output.** Colour is applied only on a terminal and never when piped: `std::io::stdout().is_terminal()` gates ANSI, so a pipe gets clean text (`src/timer_cli.rs`).
- **`NO_COLOR`-respecting.** The same gate ANDs in `std::env::var_os("NO_COLOR").is_none()` — set `NO_COLOR` and even a TTY gets no escapes.
- **A machine form and a pipe form.** `--json` emits a stable object; the bare/`status` form emits a **stable, field-ordered plain line** — `<state> <elapsed_s> <mode> <activity_id> <kind> "<title>"` — whose column order never changes and uses `-` placeholders for absent fields, so `awk`/`cut` scripts don't break (`plain_status`). A `--short` reduction (glyph + clock, empty when nothing runs) is the status-bar deliverable (`short_status`).
- **Meaningful exit codes.** They answer one question — *is the clock counting?* — `0` counting · `1` nothing running · `3` idle, reclaim pending · `4` not counting (paused / focus break) (`exit_code` / `state_word`). Write verbs exit `0` on success, `1` on refusal, with the reason on **stderr**.
- **Never a silent divergence from the screen.** The refusal copy a verb prints matches the notify-tile the screen shows for the same mistake (the unbound-stop message is shared) — one spelling of every outcome.

**Every NEW verb inherits this contract, no exceptions:**

- `engineer target` (progress) — declare / adjust / retire, plus a `list` read, each with `--json` and a piped-plain row (`progress.brief.md` §8.1).
- `engineer log` (activities) — the after-the-fact capture verb (`../shipped/timer.brief.md` names it; owned by the activities module).
- A **pace one-shot** (`engineer progress` / `engineer pace`) — the headless twin of the shipped pace meters, one per-target line piped and a single status-bar reduction (`progress.brief.md` §8.2), bound by that module's *quiet-by-default* rule.

If a module adds a read the TUI renders, it adds the one-shot in the same slice — the twin is not a follow-up, it's half the feature. This section is the definition of done those briefs cite.

---

## §D — The accent-hue decision (raise, don't resolve)

The web brand is **indigo** (`#3B40CC`, a blue-violet). `#3B40CC` is too dark to read on a dark terminal, so it must be lightened — but the shipped Rust palette lightens it all the way to a **sky-blue**: `src/ui/theme.rs` sets `ACCENT` to `Color::Indexed(75)` (`#5FAFFF`), which is a different **hue** than indigo (~210°/cyan vs indigo's ~237°/violet). It reads as a different brand colour than the web.

The kit recommends lightening **along the indigo hue** instead — periwinkle `256 #105 = #8787FF` — which keeps the brand identity while staying bright on dark, with the selection background moving from steel `67` to indigo-dim `61` to match. **The mockups already use `105`.** Adopting it is a **one-line change** in `theme.rs` (`ACCENT` / `ACCENT_DIM`); it is deliberately *not* applied in code today.

This is a decision to **ratify**, not one for any single module to silently resolve — every screen inherits the accent, so it belongs here. The full reasoning (hue math, the palette table, why it wasn't just committed) is in `../../README.md` under **"The accent decision"**. Until it's ratified, module briefs should design *to the mockups' `105`* and flag the shipped `75` as the open divergence, exactly as `progress.brief.md` §6 does — not quietly pick one.

---

## §E — `$EDITOR` for prose

Long-form prose — note bodies, retro reflections, anything past a single line — opens in the user's **`$EDITOR`** via the **`git commit` pattern** (spawn the editor on a temp file, read it back on save), rather than rebuilding a long-form text editor inside ratatui. A TUI is a character grid with a one-line input idiom; a multi-line prose editor with the user's own keymaps, syntax, and muscle memory already exists in `$EDITOR`, and reimplementing a worse one is the wrong medium.

This is **owned in detail** by `notes.brief.md` and `week-planning.brief.md` (which specify the temp-file handoff, the save-vs-abort semantics, and the empty-buffer-cancels convention); it's stated here only as the shared principle so no module invents an in-TUI textarea. Short, single-line inputs (a title, a query, an activity name) stay in the app's `i`/`Esc` insert grammar — the `$EDITOR` handoff is for the *body*, not the *label*.
