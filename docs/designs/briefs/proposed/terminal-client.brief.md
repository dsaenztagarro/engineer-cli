# Design brief — engineer-cli: the study loop for a terminal power user (neovim / zellij, keyboard-only, scriptable)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../README.md` for why they're kept apart)
**Produces:** the terminal screens *and* the headless command surface that bring engineer's full study loop into the terminal — extending the `books.html` anchor mockup into the screens below, plus the one-shot/`--json` commands and the ambient status-bar presence that a TUI mockup can't show but the brief must specify.
**Do not edit** the shipped kit files as style references (`../README.md`, `../terminal-tokens.css`, `../books.html`) — extend from them; `books.html` is the style anchor every new screen must look like it belongs beside.
**Status:** handoff draft (proposed). Not yet tracked by an epic.

### How to read this brief (please read first)
This brief is **deliberately problem-first and light on solution.**
It tells you *who this is for*, *what jobs the surface must do*, and *what genuinely constrains you* — then gets out of your way.
It is an **omnibus**: it scopes the whole terminal client at once (every transferable engineer surface), because the point is a *coherent* terminal study loop, not a pile of ported screens. The phasing in §8 says what to build first and what waits on the web app.
Where it names an existing screen, the pre-built API client, a web surface, or a competitor's idea, that is **reuse context** ("here's what's already there — extend it"), not a prescribed answer.
A TUI is a character grid, keyboard-driven, dark-first — never translate web pixel chrome; translate the *information architecture and colour semantics* (the kit's translate/don't-translate rules bind).

---

## 1. Who this is for (the workflow — the heart of the brief)

> "I live in the terminal. neovim in one zellij pane, a shell in another, tests in a third. When I'm studying — reading a Rails internals chapter, grinding a data-structures problem, building a toy compiler — leaving all that to open a browser and click a timer is exactly the friction that makes me not track it. So I don't, and the log lies.
> What I want is engineer *where I already am*.
> A timer I start with a keystroke without leaving neovim, that shows up in my zellij status bar — ` systems 24:13 ● ` — so I always know it's running.
> My pace one keystroke away — not a dashboard, a few honest ASCII meters: `systems ███▍···· 4.2/6h · behind 1.8h`.
> The activities table with the same vim motions I use everywhere — `j/k`, `/`, `:`, `gg`.
> And everything scriptable: `engineer timer status --json | jq`, so I can wire the timer into my own status line, my own git hook, my own notify script. If it's not pipeable, it doesn't fit how I work.
> When I'm binding a session to a book or a repo, give me a fuzzy picker, not a menu I arrow through. When I write a note or a retro, drop me into `$EDITOR`, don't make me type prose into a ratatui box.
> And it has to survive a dropped connection — I study on trains. The timer is a local clock; reconcile it with the server when the network comes back, don't lose my session."

The throughline: **engineer already has the domain and the API; the terminal client's job is to remove every context-switch between studying-in-the-terminal and tracking-it.**
Two things make that real and neither exists yet: an **ambient, scriptable presence** (the timer and pace as text a status bar or script can consume) and a **modal, fuzzy, `$EDITOR`-respecting interface** that feels native to someone whose hands never leave the keyboard.
Get those right and the log stops lying, because tracking costs a keystroke instead of a context-switch.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

Organised by engineer's study loop — **plan → do → measure → retrospect** — so the client mirrors the pillar rather than being a bag of screens. For each, design *both* the interactive surface and (where it applies) the one-shot/`--json` form.

### Do — capture (the heart of a terminal client)
1. **Run the timer without leaving your work.** Start / pause / resume / stop / discard, bind to an activity (or run as an unnamed stopwatch), and *switch* what it's bound to — from a one-shot command (`engineer timer start`, `toggle`, `stop`) and from the TUI. Binding uses a **fuzzy picker** over the existing candidate search. The API client for all of this is already built; it has never had a face.
2. **Be aware of the timer everywhere.** A compact, colour-coded status string the user can drop into a zellij/tmux status bar or neovim statusline, and a keybind-friendly toggle so the multiplexer can drive it. This is the terminal's answer to the web's canonical timer pill: same idea (always-visible, identity-in-the-detail), different medium (a status-bar string, not a pixel pill).
3. **Log after the fact, fast.** Record a completed activity or a manual segment without having run the timer — a quick one-shot (`engineer log …`) and the existing new-activity form — for the sessions you forgot to start.
4. **Work the activities table.** The core domain surface: list, filter, sort, open, edit, complete, archive, duplicate — with the vim table grammar the client already uses. Segments of an activity are viewable and editable here. (Activity + segment API clients are already built.)

### Measure — the payoff (the terminal's promised superpower)
5. **Read pace as one-line meters.** Per weekly target: done-vs-intended and ahead/behind, as ASCII meters — the web Progress brief *explicitly* designed for this reduction ("survive being a line of text per target": `systems  ███▍····  4.2/6h`). Both a TUI screen and a one-shot for the status bar / a `--json` feed. (Note: unlike the timer, the progress/targets API client is **not** built yet — it's the one net-new client this brief implies.)
6. **Adjust targets in place.** Declare, adjust, and retire a weekly target from the command line (`engineer target …`) with the same near-zero ceremony the web brief demands.
7. **Explore where the time went.** Pivot completed time by domain / kind / intent / anchor over a period — a terminal pivot, reusing the table grammar and the meter widget.
8. **Review what's decaying.** The spaced-repetition dashboard, the due list, and the rate drill — surfaced as a terminal triage list with due-badges. (Review API client already built.)

### Plan / retrospect
9. **Plan and retrospect the week.** The plan side (what to study this week) and the retro (planned-vs-done). Long-form reflection opens in `$EDITOR`, not a ratatui text box.

### Cross-cutting spine
10. **The command palette as the universal verb line.** `:` is the power-user's primary interface — `:track systems`, `:progress`, `:plan`, `:review`, `:log 45m reading` — fuzzy-completing verbs *and* arguments (activities, books, repos). It ties every job above into one muscle-memory entry point; the event layer already opens `:` but only `:logs` routes today.
11. **Home, enriched.** Today at a glance — but now leading with the running timer and this week's pace, not just a list.

For each job the interesting question is *the smallest keystroke- or pipe-shaped thing that does it* — the information architecture is yours to invent within the kit.

---

## 3. Principles that genuinely bind (constraints, not solutions)

- **Modal, keyboard-only, neovim muscle memory — everywhere, consistently.** `j/k`, `gg/G`, `/`, `n/N`, `:cmd`, `<Space>` leader, `i`/`Esc`, and the footer always showing the active screen's keys. Every new surface obeys this grammar; a screen that needs a mouse or a bespoke keymap is wrong. (This is already the shipped convention — hold it.)
- **TUI ↔ headless duality is a first-class requirement, not an afterthought.** Every read the TUI shows must also exist as a **non-interactive one-shot** with a `--json` (machine) and a plain-text (human/pipe) form: TTY-detected, `NO_COLOR`-respecting, meaningful exit codes, no ANSI when piped. This is what makes it a power-user tool rather than a pretty TUI — it must compose in status bars, git hooks, and scripts. Design the one-shot output shapes with the same care as the screens.
- **Ambient presence over foreground attention.** The timer and pace are things the user wants *glanceable in a status bar*, not a screen they must open. Design the compact status string (running/paused/over states via colour + glyph) as a real deliverable. Follow the web's quiet-by-default rule: on-pace shows calm, behind shows a small signal, nothing nags.
- **Fuzzy over navigate.** Any pick from a set the user knows the name of — bind a timer, choose a book/repo/domain, jump to an activity — is a fuzzy filter (Telescope-flavoured), not an arrow-through menu. The timer candidate-search endpoint already exists to power this.
- **`$EDITOR` for prose.** Notes, activity descriptions, retro reflections open in the user's editor (the `git commit` pattern) — never rebuild a long-form text editor in ratatui.
- **Offline-tolerant, and the timer especially.** Terminal users work disconnected. The timer is a **local clock** (start time + paused seconds) that renders and controls locally and reconciles with the server when the network returns; reads cache their last-known value so the status bar is never blank offline. Optimistic local action with a sync queue, honestly surfaced when it diverges.
- **One clock.** Day/week attribution uses engineer's 4 AM study-day boundary and Monday-first week; a one-line meter and a status string must agree with the web to the minute.
- **Derived, never stored.** Pace, rollups, and explorer views are read-through from the server's on-read aggregates — the client renders them, it doesn't keep a second ledger.
- **The character grid is the whole medium.** One monospace font, 256-colour palette, dark-first, box-drawing for structure; no truecolor requirement, no shadows/radii/web-fonts. Honour the kit's palette (and note, don't silently resolve, the pending accent-hue decision — periwinkle `105` vs shipped sky-blue `75`).

---

## 4. What's already in the app (orientation, so you extend rather than reinvent)

From a survey of the current codebase — behaviour, not schema:

- **Built screens:** Home (today's activities + reading books), Books (filter/search/status pills), Book detail (chapters, inline page edit), the new-activity form (vim insert mode, field errors), Sign-in. These are the style-and-interaction precedent; new screens must feel like siblings.
- **The API client is far ahead of the screens.** Clients for the **timer (start/pause/resume/stop/bind/discard/candidates — all 7)**, **segments** (list/add/edit/delete), **review** (dashboard/topics/rate), **notes**, **videos**, and the fuller **activities** verbs (complete/archive/duplicate) are **already implemented and unused** — waiting for a face. Several jobs in §2 are "wire the screen to a client that already exists," not "build from zero."
- **Auth and config are solved.** OAuth2 PKCE loopback + OS-keyring refresh tokens; XDG config with prod/dev presets and per-key env overrides. New surfaces inherit this; don't reinvent it.
- **Chrome, widgets, and interaction are shipped and documented** (kit README): the three-row header/body/footer, `bordered()` panels, `status_pill`, the block `progress_bar` (`███▍····· 42%`), auto-expiring `notify` tiles, full-row inverse selection with a `▌` marker. The pace meters and status pills you need are largely *this widget set aimed at time*.
- **The command line exists but is inert:** `:` opens and the event layer parses it, but only `:logs` routes — the palette is a spine waiting to be connected.
- **What genuinely does not exist yet:** any JSON / non-TTY / machine-readable output; any offline or local caching (state is in-memory, lost on exit); a progress/targets API client; a timer screen or status-bar string; the activities table; a segment UI; review/notes screens; command-palette routing; fuzzy pickers; `$EDITOR` integration.

**Read before designing:** `../README.md` (palette, chrome, translate/don't-translate — the binding kit), `../books.html` (the style anchor), and — for information architecture only, not layout — the web briefs this client mirrors: `engineer/docs/designs/briefs/shipped/progress.brief.md` (pace as a line of text is designed *for* the terminal there) and `.../week-planning.brief.md`.

---

## 5. The surface (framed as problems, with room to solve them your way)

Design the screens *and* their headless twins. In whatever layout the grid makes clearest:

- **The timer, three ways.** (a) The **status-bar string** — running / paused / over, colour + glyph, fixed-ish width, identity-in-the-detail not the string; (b) the **one-shot verbs** and their output; (c) the **TUI control** (bind via fuzzy picker, switch, the elapsed read). Show the over-target and paused states honestly.
- **Pace meters.** The one-line-per-target meter (TUI list and piped form), the ahead/behind framing, and the empty state that teaches "declare a target." Show it reduced to a single status-bar line.
- **The activities table.** The core surface with the vim toolbar (filter/sort/open/edit/complete/archive/duplicate) and a drill into an activity's **segments** (view/add/edit). Reuse the table grammar the client already implies.
- **The command palette.** The `:` verb line: how it fuzzy-completes verbs and arguments, how it echoes results (a `:progress` inline summary, a `:track` confirmation), and how it degrades to a one-shot of the same verb.
- **Review triage.** The due list + rate drill in Review's badge grammar.
- **Home, enriched.** Timer + week pace leading, today's activities beneath.
- **The headless contract.** A small spec of the one-shot command shapes and their `--json`/plain output — the deliverable that lets a user wire engineer into their own zellij/tmux/nvim/scripts.

A `before → after` against today ("track = leave the terminal, open a browser, click — so I don't") will sell the client.

---

## 6. Visual language (this is a hard constraint — do not drift)

Bind to **this repo's kit**, not the web design system: `../README.md` (chrome + palette mapping + translate/don't-translate), `../terminal-tokens.css` (the executable palette), `../books.html` (the anchor screen).
Assemble from shipped terminal atoms only: the header/body/footer chrome, `bordered()` panels, `status_pill`, the block `progress_bar`, `notify` tiles, full-row inverse selection with `▌`. **No new medium** — one monospace font, weight (`BOLD`) + colour for hierarchy, 256-colour semantics (accent / success / warn / danger / muted / border), box-drawing for structure, sparse unicode glyphs / status dots `●`/`○`.
Keyboard-only, neovim-flavoured; the footer always advertises the active keys.
ASCII only in any diagram (`+ - | v ^ >`).
The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is a decision to *raise*, not silently resolve — the mockups may use the recommended `105`, but flag it.

---

## 7. Out of scope

Mouse support, truecolor/24-bit requirement, and screen-reader a11y (a character grid is inherently keyboard/visual); rebuilding a rich text editor in ratatui (use `$EDITOR`); the web pixel chrome (shadows/radii/web-fonts/responsive breakpoints); billing, rates, and invoice/CSV export beyond generic `--json`; the engineer web app's own surfaces (this client *consumes* their APIs, it doesn't redesign them). The **advanced** surfaces that depend on unreleased web features (idle/focus/audit, weekly recap/overrun, assisted-capture) are **in scope as consumers but sequenced** — see §8; do not design them ahead of their web briefs.

---

## 8. Phasing & dependencies (this is also the answer to "what order?")

The client splits cleanly by what the **engineer API already offers** versus what a **new web brief must ship first**.

- **Phase 1 — bring the existing loop across. Independent; can start now.** Jobs 1–11 above depend only on engineer APIs that already exist (timer, activities, segments, progress/targets, review, books, week-planning). No dependency on the three new web briefs. Suggested internal order: (i) the **headless/`--json` foundation** + config, then (ii) **timer + status-bar presence** (API ready, highest leverage for this user), then (iii) **pace meters** (add the one missing API client), (iv) **activities/segments table**, (v) **command-palette routing + fuzzy**, (vi) **review + library**. Offline-tolerance and `$EDITOR` are cross-cutting, folded in as each surface lands.
- **Phase 2 — surface the new web features as they ship.** Each trails its web brief by one release:
  - CLI **idle-reclaim / focus / segment-audit** ← after `engineer` **timer-hygiene** ships (design + API).
  - CLI **weekly recap / overrun ping / thin-week prompt** ← after `engineer` **nudges** ships. (The overrun ping is especially natural as a status-bar state and a `notify` tile.)
  - CLI **assisted-capture inbox** ← after `engineer` **assisted-capture** ships — and the **git-activity source is native to this user** (they commit from the terminal), so the terminal is arguably its best home.

So: **engineer-cli Phase 1 is independent of the pending web briefs and parallelisable with them; engineer-cli Phase 2 is a per-feature consumer that trails each web brief.** The full cross-repo order lives in the handoff message accompanying these briefs.

**Each Phase-2 surface gets its own small follow-up brief** (`cli-<feature>.brief.md` in `proposed/`), authored *when* its web brief is designed — so it can cite the shipped web surface and the real API rather than guessing. Keep them one-per-feature (not one bundled Phase-2 brief), because they ship staggered behind different web briefs and each needs its own `proposed -> shipped` lifecycle row. They stay short: they inherit *this* brief's terminal grammar — the timer status string, `notify` tiles, the triage list, the headless twins — and specify only what's genuinely new and terminal-specific (idle-reclaim on a local/offline clock; a focus break in an unwatched status bar; how loud an overrun is when you're not looking; the git-source connect flow). If a surface turns out fully determined by "apply the established grammar to the new API," skip the brief and build it. Of the three, the **assisted-capture inbox** clearly warrants a full brief of its own (a new draft-triage screen + a terminal-native git-source connect flow); idle/focus/audit and recap/overrun/thin-week are expected to be light briefs or design notes.

---

## 9. Where the detailed model lives (so you don't have to invent it — but also aren't bound by it)

The engineering shape lives in the engineer-cli tracker (screen wiring against the pre-built API clients; the one net-new progress/targets client; the headless output contract; the offline/optimistic timer reconciliation). The web-side API shapes live in the engineer app and its briefs. These are intentionally **not** part of this handoff, so your layout isn't anchored to a particular command tree or output schema.
If your design implies a cleaner verb set, meter shape, or status-string format, say so — that feedback is welcome and will update the plan.

## 10. How to use this brief

Extend `books.html` into the screens in §5 (and specify the headless twins), in the terminal kit's visual language (§6), solving the jobs in §2 under the principles in §3, respecting the phasing in §8.
Iterate on the mockups until they're right; the Rust implementation turns them into screens afterward, wiring mostly to API clients that already exist.
This client is the terminal face of the same study loop the web `engineer` briefs design — it should feel like the *same product, native to a character grid and a power user's hands*, not a port.
