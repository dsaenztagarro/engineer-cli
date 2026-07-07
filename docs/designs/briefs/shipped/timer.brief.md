# Design brief — engineer-cli · Timer (run the timer without leaving your work + ambient presence)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md`).
**Produces:** the timer face and states, the ambient status-bar string, the fuzzy bind flow, and the headless verb suite — mocked in `../../timer.dc.html`, anchored on `../../design-system.dc.html`.
**Status:** **shipped.** The interactive timer, header status-cell, focus rhythm, idle reclaim, overrun ping, segment audit, and the full `engineer timer` headless suite are live (epic #26 timer-v2 → v0.3.0; guards & rhythm → v0.4.0). This brief is kept as the module record; the gap analysis in §A/§B/§C is the diff-vs-`engineer` that drove the design and is now closed.

> **Module note.** This is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It folds the run-the-timer and ambient-presence workflow from the retired omnibus (`terminal-client.brief.md` jobs 1–2) into the timer gap analysis that originally shipped as `timer-gaps.brief.md`. The house format is shared across module briefs: workflow → jobs → the gap record → the API it consumes.

## Workflow (why the timer is the heart of a terminal client)

> "A timer I start with a keystroke without leaving neovim, that shows up in my zellij status bar — ` systems 24:13 ● ` — so I always know it's running. Binding it to a book or a repo is a fuzzy picker, not a menu I arrow through. And it has to survive a dropped connection — I study on trains: the timer is a local clock; reconcile it with the server when the network comes back, don't lose my session."

## Jobs

1. **Run the timer without leaving your work.** Start / pause / resume / stop / discard, bind to an activity (or run as an unnamed stopwatch), and *switch* what it's bound to — from a one-shot command (`engineer timer start`, `toggle`, `stop`) and from the TUI. Binding uses a fuzzy picker over the candidate search. *(Shipped.)*
2. **Be aware of the timer everywhere.** A compact, colour-coded status string the user drops into a zellij/tmux status bar or neovim statusline, plus a keybind-friendly `toggle` — the terminal's answer to the web's canonical timer pill. *(Shipped: the header status-cell + the `--short` headless string.)*

*(The adjacent "log after the fact, fast" verb — `engineer log …` — is a capture verb tracked in `activities.brief.md`, not here. Offline-tolerance / local-clock reconciliation, a principle this workflow leans on, is captured in `cross-cutting.brief.md`; today the timer renders on a display-smoothed local tick but every write is live.)*

---

## The gap record — `timer.dc.html` vs engineer's shipped timer

**Method:** `timer.dc.html` was diffed against engineer's shipped timer behavior — the live timer + idle guard + focus + segment audit (`engineer` epic #717, `engineer/docs/features/timer-hygiene.md`), the canonical pill spec (`engineer/docs/designs/navigation-bar.html` §M), the overrun ping (`engineer/docs/designs/nudges.html` §B), the settings knobs (`engineer/docs/designs/settings.html`), and the `api/v1` timer/segments endpoints the CLI consumes.

> **Design pass of 2026-07-06 — sections A and B are closed.** Every item below now has a panel (or an explicit caption) in `timer.dc.html`:
> M1 → §Start a timer (just-start row) + §Bind at stop + §Status line (unnamed row) · M2 → §Start conflict · M3 → §Paused + §Status line (paused row) · M4 → §Focus offers (offers, long break, mode switch) · M5 → §Overrun + §Status line (over row) · M6 → §Segment audit (Looks right action; Trim specced as a segment-edit PATCH preset) · M7 → §Timer settings (view-only in CLI, edit on web — decided) + settings-driven copy throughout · M8 → §Headless (write verbs) + §Headless contract (full status-string table, `--short`, exit codes 0/1/3/4) · M9 → §Saved & undo · M10 → caption on §Idle reclaim (presence = TUI keystrokes; detached/closed = absent; entered on next interaction or launch) ·
> F1 → §Idle reclaim remapped to the server verbs (trim keeps running / keep / stop at last input, + discard escape) · F2 → see M6 · F3 → see M7 · F4 → hero rail relabelled THIS WEEK.
> **Section C (backend API gaps) — now RESOLVED.** The `engineer` server shipped the timer-hygiene API (epic #754): `POST /api/v1/timer/{reclaim,phase,mode,heartbeat}` and `GET /api/v1/timer/settings` (`engineer/config/routes.rb:380-391`), plus the segment-audit list + acknowledge (`GET /api/v1/progress/audit`, `PATCH /api/v1/progress/audit/segments/:id/acknowledge`). The CLI consumes all of them (`src/api/timer.rs`, `src/api/audit.rs`), and the interactive + headless surfaces shipped. Settings stayed `GET`-only, so **"view-only in the CLI, edit on the web" is API-forced, not just a UX choice** — the decision M7 records.

The kit rules still bind (`../../README.md`, `../../design-system.dc.html`): character grid, keyboard-only, `j/k`/`⏎`/`Esc` grammar, the shipped chrome and widget idioms.

---

## A. Missing screens & states

### M1 — Unnamed stopwatch & bind-at-stop

The server supports a timer with **no activity** (start bare, label optional, bind later); stopping an unbound timer is a 422 — it *must* be bound (or discarded) first.
Web precedent: nav-bar §M state "running-unnamed" — italic *Untitled*, and pressing stop **freezes the clock and opens the name/attach flow** instead of saving silently.
Missing in `timer.dc.html`:

- A "just start, name it later" path in §Start a timer (the picker currently forces choosing or creating an activity).
- The unnamed running state on the hero and the status line (italic *untitled* label).
- The **bind-at-stop** moment: `s` on an unbound timer → frozen clock + the activity picker (reuse §Start's list) → save; `d` remains the discard escape.

### M2 — Start-while-running conflict (stop & switch)

Only one timer can run; a second start is a **409** unless `switch=true`, which stops-and-saves the current one first.
Web precedent: §M conflict prompt — "A timer is already running on X (12:47). Start tracking Y instead?" → [Keep running] / [Stop & switch].
Missing: §Start a timer only shows the "nothing running" state. Design the running-conflict variant (banner or inline row state + the two choices), and the headless twin (`engineer timer start --switch`, plus the non-switch failure output/exit code).

### M3 — Paused state (plain pause, not just focus break)

Pause freezes the clock, the dot stops pulsing, the paused gap is excluded from the total, and **a paused timer never goes idle**.
Missing: the paused face on the hero (stopwatch *and* focus variants) and a plain paused status-line row — the current §Status-line only shows the focus-break flavor of "not counting". Clarify `SPC` as pause/resume toggle and what the resume affordance looks like.

### M4 — Focus phase boundaries: offers, long break, mid-session mode switch

Shipped focus behavior (timer-hygiene §B): transitions **never fire on their own** — at 0:00 the UI reveals an *offer* ("Interval complete" → [Start 10m break] / [Keep working]; "Break's over" → [Back to work]); every Nth break is a **long break** (`focus_long_break_every`, default every 4th, 20m); a running timer can **switch mode** stopwatch ↔ focus mid-session.
Missing in `timer.dc.html`:

- The interval-end **offer moment** in the TUI — on the hero, and how it surfaces when the user is on another screen (status-line glyph change? notify tile?). `n skip interval` exists as a key but the offer flow it short-circuits is undrawn.
- The **long break** state (rhythm track, pomodoro dots, offer label reading the configured duration).
- The **mode switch** on a running timer (the picker's Tab toggle only covers a fresh start).

### M5 — Overrun ping (bound timer past its plan)

Shipped (nudges §B, `overrun_ping_enabled` default on): when a bound activity has a planned duration and **cumulative** time (already-logged segments + live elapsed) crosses it, the clock turns amber and a card offers [Wrap up & save] / [Keep going] — once per timer, never auto-stopping.
Missing entirely: no over state in §Status-line, no offer moment, no headless representation (an `over` flag in `--json`? its own exit code? the plain-text form). The terminal-client brief calls this "especially natural as a status-bar state and a notify tile" — design it here, it's a timer state, not a separate surface.

### M6 — Audit: the acknowledge action, and verb alignment

Shipped audit row actions are **Fix** (opens the activity edit), **Looks right** (stamps `audit_acknowledged_at`, permanently clearing the two amber duration flags), and **Delete**. There is **no "trim" verb on a completed segment** anywhere in the backend.
`timer.dc.html` §Segment audit shows `[Trim]` on long rows and has no acknowledge at all.
Fix in design: add the **Looks right / acknowledge** action (key + what happens to the row), and either drop `[Trim]` or explicitly spec it as a segment-edit preset (a `PATCH` that shortens the duration) so implementation knows what it writes. Also state whether the CLI gets an ambient "N to audit" badge (web: chip on Progress, `audit_badge_enabled`) or the count lives only in this screen.

### M7 — Timer settings: where knobs live in the CLI

Engineer has per-user knobs, all with a settings UI on the web: default mode, work/short-break/long-break minutes, long-break-every-N, idle guard on/off, idle threshold, default reclaim action, audit long/short thresholds, audit badge, overrun ping.
`timer.dc.html` hardcodes their defaults into copy ("50m work · 10m break · ×4", "over ~6h", "under 60s").
Needed: (a) make the copy read as **settings-driven**, not constants; (b) decide and design the CLI's settings story — a settings screen / `engineer config` verbs, or an explicit "view-only here, edit on the web" pointer. Either is fine; it must be a decision, not an omission.

### M8 — Headless: the write verbs and the full status-string contract

§Headless covers the reads (`timer`, `timer --json`, `timer status`) and one write (`stop --reclaim=trim`). The brief requires **every** verb to have a one-shot twin. Missing:

- `engineer timer start` (fuzzy bind argument, `--switch`), `toggle` (the keybind-friendly form for multiplexers), `pause` / `resume`, `bind`, `discard`, and `candidates --json`.
- The plain-text status string for **every** state — paused, idle, focus work/break, over, unnamed, nothing-running — with the fixed-footprint promise §Status-line makes in-app extended to the piped form (this *is* the zellij/tmux status-bar deliverable).
- The complete exit-code table (0 running / 3 idle-pending-reclaim / 1 nothing running exist; state codes for paused/over if scripts should branch on them).

### M9 — Stop confirmation, undo, and discard confirm

Web: stopping a bound timer saves immediately and confirms with a toast + **Undo** ("12m added to … · total tracked now 1h 30m"); discard asks for confirmation past ~2 minutes.
Missing: the post-save moment in the TUI (a notify tile with the written segment — and whether undo exists, which the segment-delete API makes possible), and the discard confirmation state for `d`.

### M10 — Idle presence semantics for a terminal (design note, small)

Web idle = absence of `pointerdown`/`keydown` on the page, heartbeated at most once per minute. The CLI must define its equivalent: what counts as presence (keystrokes inside the TUI only? does a detached/closed TUI count as absent?), and when the §Idle reclaim screen is entered (on launch with an idle timer? on `g t`? on any keypress, mirroring the web's next-interaction rule). One caption on the reclaim screen can settle this.

---

## B. Design ↔ server alignment flags (drift to resolve, not new screens)

- **F1 — Reclaim options don't match the server's verbs.** Shipped reclaim verbs: **keep** (tail counts, timer continues), **trim** (idle span moved to paused time, **timer keeps running**), **stop** (save up to last input, timer ends). `timer.dc.html` shows four options (*Trim to last activity / Split into work + idle / Keep all & resume / Discard the idle tail*) under "⏎ apply & stop" — i.e. every option ends the timer, "Trim" behaves like the server's *stop*, and "Split … logged idle" implies writing an idle segment, which the server never does (idle time is *never* logged). Rename/remap so each option is one server verb — including a continue-running trim — or explicitly call out the new semantics as an API change request.
- **F2 — Audit `[Trim]` / missing acknowledge** — see M6.
- **F3 — Hardcoded knob values in copy** — see M7.
- **F4 — Rail nit:** the hero rail block labeled **TODAY** renders a `mon → sun` sparkline — a week series under a "today" label. Clarify which it is.

---

## C. Backend API gaps — RESOLVED (kept as the record of what was blocked, and how it unblocked)

When this analysis was written, the v1 JSON API the CLI consumes exposed only the timer read (with `idle`, `mode`, `phase`, `intervals_completed`), start/pause/resume/stop/bind/candidates/discard, and segment CRUD. Heartbeat, reclaim, focus phase transition, mode switch, the segment audit (list/acknowledge/delete-flow), and settings were **web-only, no API** — so `engineer timer stop --reclaim=…`, the §Idle reclaim screen, focus transitions (M4), audit actions (M6), and any settings surface (M7) were all blocked on the server.

Those gaps have since shipped in the `engineer` repo (epic #754) and the CLI now consumes them:

- `POST /api/v1/timer/reclaim` (`trim` | `keep` | `stop`), `POST /api/v1/timer/phase`, `POST /api/v1/timer/mode`, `POST /api/v1/timer/heartbeat`, `GET /api/v1/timer/settings` — `engineer/config/routes.rb:380-391`, consumed in `src/api/timer.rs`.
- Segment audit: `GET /api/v1/progress/audit` and `PATCH /api/v1/progress/audit/segments/:id/acknowledge` — `routes.rb:421-423`, consumed in `src/api/audit.rs`. Trim/Delete go through the ordinary segments API (`PATCH`/`DELETE /activities/:id/segments/:id`).
- Settings is `GET`-only — there is no write route, which is why the CLI settings surface is read-only (see M7). That is a settled decision, not an omission.

---

## D. Adjacent timer-touching surfaces intentionally *not* in `timer.dc.html`

Tracked elsewhere, per the terminal-client brief's phasing (§8) — don't fold them in here: the weekly recap line (`engineer recap --last`) and thin-week invite (a `cli-nudges` follow-up), the enriched Home leading with timer + pace, week-planning's "Start binds the timer to a planned item", and the Progress pace/rollup screens the audit subtabs point at.
