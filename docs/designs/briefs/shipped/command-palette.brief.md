# Design brief — engineer-cli · The command palette (the `:` verb line, the universal spine)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the `:` command line itself — the footer verb line, its four line-states (empty → partial → resolved → unknown), Tab completion, and `:help`. It has no dedicated mock board: the palette is a footer element every screen's `.dc.html` renders, so its look is settled by the chrome and `render_line` rather than a standalone canvas. Anchored on `../../design-system.dc.html` (the style anchor).
**Status:** **shipped.** The `ENTRIES` grammar table and the dispatch / Tab-completion / inline-hint / `:help` surfaces it drives are live (epic #7 daily-loop, `src/app/command.rs`). This brief is kept as the module record; §8 tracks the verbs each *other* module still owes the grammar as it lands.

> **Module note.** This is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the command-palette work of the retired omnibus (`terminal-client.brief.md` job 10 — "the `:` line as the universal verb line, tying every job into one muscle-memory entry point, degrading to a one-shot of the same verb") and grounds it against the shipped `src/app/command.rs`. The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "I don't want to remember which screen a verb lives on. I want one key — `:` — then the verb, and it takes me there or does the thing, from wherever I am.
> `:prog` jumps to pace, `:t start` starts the clock without leaving the notes browser, `:note closures are objects` throws a thought into capture mid-read.
> Tab finishes what I started; if I fat-finger a verb it tells me what I meant instead of scolding me.
> And the verb I type after `:` is the *same* verb I'd type at the shell — `:timer start` and `engineer timer start` are one muscle, not two."

The throughline: **the grammar is the product spine, on purpose.** `:` is the single entry point that ties every job into one reflex, and every screen's footer advertises the same verbs, so learning one surface teaches all of them. Where a verb has a headless twin it *is* that twin; where a screen has no one-shot, the palette is still the fastest in-TUI path to it. The remaining work is not to the spine but to its reach: it must gain a verb as each module lands its own write (see §8).

---

## 2. The jobs the design must do (outcomes, not mechanisms)

This is omnibus **job 10**, decomposed into the outcomes the shipped grammar delivers.

**10a. Reach any screen by name, from any screen.** A bare verb navigates. *Shipped:* `:home :books :activities :notes :review :progress :settings :audit :timer` all route to their screen, resolved exact-verb-first then by unique prefix, so `:prog`, `:ac`, `:au` land without a full type.

**10b. Run the common actions inline, without arrow-driving a menu.** *Shipped:* `:timer start|pause|resume|stop` dispatched from **any** screen against the app's timer snapshot (prefix-resolved: `:t start`, `:timer p`), and `:note [text]` opens the quick-capture overlay, prefilled verbatim when text follows. These are the verbs that would otherwise cost a screen change and a hunt for a keybind.

**10c. Fail helpfully, never hostilely.** *Shipped:* Tab extends the buffer to the longest common prefix (vim's `wildmode=longest`); an ambiguous prefix lists its candidates rather than guessing (`:l` → logs · logout); an unknown verb points at `:help`; a bad or ambiguous sub-verb suggests the accepted set. The line teaches its own grammar as you type — four line-states, one of them soft-warn-tinted, none of them an error the user has to dismiss.

**10d. Be the same muscle as the shell.** *Shipped for the verbs that have twins:* `:timer start` degrades to the identical `engineer timer start` one-shot. Each new verb should carry the same duality where an equivalent headless one-shot exists — the palette and the CLI are two faces of one grammar, not two grammars.

---

## 3. Principles that genuinely bind

- **One table is the single source of truth.** `ENTRIES` (`src/app/command.rs`) drives dispatch, Tab completion, the inline hints, **and** `:help` — so the grammar can never drift between what runs and what the UI advertises. Adding a verb is one row plus its `Command`/`Target` wiring; completion, hints, and `:help` update for free because they read the table.
- **Vim-flavoured resolution, deliberately.** Exact verb or alias wins; then a unique prefix resolves; else the candidates are reported, never guessed at (`:act` → activities, `:t start` → timer start, `:l` → logs/logout listed). Argument resolution reuses the *same* exact-then-prefix rule against a fixed sub-verb set (`:timer s` → start · stop reported, `:timer p` → pause).
- **The grammar IS the spine, by design — hold it.** Every screen's footer hints reference this same grammar; `:` is the one entry point that ties every job into one reflex. This is a design decision, not an incidental feature — new surfaces route through the palette rather than inventing a parallel command path.
- **Unknown-verb responses are helpful, not hostile.** An unrecognised verb is a soft `WARN`-tinted hint that routes to `:help`, not a red error. The palette assumes a typo, not a fight.
- **TUI ↔ headless duality is first-class.** Each verb should degrade to an equivalent headless one-shot where one exists — the same duality the timer suite already ships (`:timer start` ↔ `engineer timer start`).
- **The palette routes; it does not own data.** It dispatches Goto (nav) and timer/capture actions and lets those modules own their writes and their network. It holds no ledger and speaks to no endpoint of its own — see §5.

---

## 4. What's already in the app (orientation — shipped reality)

- **The grammar table** — `src/app/command.rs`, the `ENTRIES` table (≈ lines 72–193) is the single source of truth. Around it: `parse()` → a `Parse` outcome (`Empty` / `Run(Command)` / `Unknown` / `Ambiguous` / `BadArg` / `AmbiguousArg`); the runnable `Command` enum (`Nav` / `Timer(TimerVerb)` / `Note` / `Quit` / `Write` / `Logs` / `Logout` / `Help`); `complete()` (the `wildmode=longest` Tab step, no cross-keystroke cycle state); `hint()` (the four inline line-states); `render_line()` (the footer `:input█` + inline hint); and `help_summary()` (the `:help` one-liner, assembled from `ENTRIES` so it cannot drift). A full unit-test suite pins each behaviour.
- **Verbs that route today** (this **corrects the omnibus's stale "only `:logs` routes"** — that is false now; the whole inventory dispatches):
  - **NAV** — `:home :books :activities :notes :review :progress :settings :audit :timer`; a bare verb goes to the screen.
  - **TIMER ACTIONS** — `:timer start|pause|resume|stop`, prefix-resolved and dispatched from any screen against the app's timer snapshot.
  - **ACTIONS** — `:note [text]`, opening/prefilling the quick-capture overlay.
  - **HOUSEKEEPING** — `:w` / `:write` (submit the current form), `:logs` (show the log directory), `:logout` (prints a shell hint — it does **not** actually log out), `:q` / `:quit`, and `:help`.
- **The empty-line hint** advertises the primary nav verbs plus `:help` and the Tab affordance; the resolved-verb hint echoes each entry's own `help` string, so the advertisement is the table talking.
- **What genuinely does not exist yet:** `:log` (waits on the activities log-after-the-fact verb); `:target` (its module *has* shipped, but the verb is **deliberately deferred** — it collides with the shipped `:t` → timer prefix; see §8); and general argument fuzzy-completion beyond the timer's server-side candidate match. Those are §8, not gaps in the spine.

---

## 5. The API it consumes (there isn't one — and that's the point)

The palette has **no direct HTTP surface** — there is no route table to verify here, and that is the design, not an omission. It is a router over the app's *own* command types: it dispatches `Command::Nav(ScreenKind)` (a Goto) and the timer/capture actions (`Command::Timer(TimerVerb)`, `Command::Note`), and hands each off to the module that owns it. Its only indirect network touches are the timer actions and `:note` capture, and those live behind the timer and notes/activities modules — their endpoints, their reconciliation, their tests (`timer.brief.md`, `notes.brief.md`). So when a verb needs to grow, the contract to check is the *target module's* API, cited in that module's brief; the palette just adds the row that reaches it.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus once cited no longer exist). The command line renders in the footer row (the same slot the hints/notify tile occupy): `render_line()` draws a focused-accent `:`, the typed buffer, a muted `█` cursor block, and — after a few spaces — the inline hint, muted for the normal states and `WARN`-tinted for the unknown / bad-argument states. Keep the four line-states legible as one continuous surface (empty → advertises the nav verbs · `:help` · Tab; partial → matches or the completed stem; resolved → `→ <help>` and the argument shape; unknown → the soft warning). ASCII-only, keyboard-only, neovim-flavoured; the footer across every screen must speak this same grammar so the spine reads as one thing everywhere. The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one to *raise*, not silently resolve — see the cross-cutting brief.

---

## 7. Out of scope

The screens and writes the palette dispatches to — those belong to each target module's brief, not here; the palette only routes. No direct API surface (§5). No argument fuzzy-picker beyond the timer's server-side candidate match — a shared fuzzy picker over activities/books/repos/domains is a cross-cutting concern (`cross-cutting.brief.md`), not a palette-local feature. No command history / scrollback, no macro or scripting language, no light mode. Verbs for modules whose work has not shipped are §8, deliberately not stubbed in the table (`ENTRIES` never advertises a verb that does not run).

---

## 8. Phasing — the spine is shipped; it grows one verb per module

The palette is done as a spine. What remains is *reach*: it must gain a verb as each module lands its write, and each addition is one `ENTRIES` row + its `Command`/`Target` wiring (completion, hints, and `:help` follow for free).

1. **`:log`** — the activities quick-capture verb, arriving with that module's log-after-the-fact work (cross-ref `activities.brief.md`). It is the sibling of `:note` for a completed segment.
2. **`:target`** — **deliberately deferred**, not merely pending. Progress's targets-write slice has shipped (interactive adjust/retire + the `engineer target …` headless twins), but a `:target` verb was **not** added: any verb or alias beginning with `t` collides with the shipped `:t` → timer prefix (a tested muscle-memory binding that `unambiguous_prefixes_resolve` guards). Adding `target` would make `:t` ambiguous — a regression on a shipped shortcut, taken for a new verb the user did not ask to remap. Targets are reached instead via `:progress` / `:p` (the Progress screen owns declare/adjust/retire) and the headless verb. Revisiting needs a **disambiguation rule or a different mnemonic** (e.g. keep `:t` pinned to timer and require `:tar`, or a distinct leader) — a real grammar decision to make on its own, not a silent collision. (Same for any other future `t*` verb.)
3. **General argument fuzzy-completion** — for activities / books / repos / domains, beyond today's single case: only the timer bind fuzzy-matches server-side right now. A shared fuzzy picker the palette (and every argument slot) draws from is a cross-cutting concern (cross-ref `cross-cutting.brief.md`); until it lands, argument completion stays limited to the fixed sub-verb sets the table already knows.
