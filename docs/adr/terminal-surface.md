# Terminal surface

**Status:** Accepted · **Decision log:** [#210](https://github.com/dsaenztagarro/engineer-cli/issues/210)

How the client speaks: the TUI and its headless twins, the three tiers every message lands in, one spelling per outcome, where prose is written, and the palette pipeline every colour comes from.

## Context

This client has two faces over one domain: an interactive TUI and a set of non-interactive verbs.
The cheap way to build the second is to let each verb invent its own output and add a one-shot only when someone asks.
That is how a CLI ends up unscriptable: a status line whose columns move between releases, ANSI escapes in a pipe, an exit code that means "it printed something", and a refusal that reads differently on `stderr` than the same refusal does on screen.
The terminal's advantage is that its output composes — into `jq`, git hooks, a zellij or tmux status bar ([scope](scope.md), criterion 5) — and that advantage only exists if the composable form is a contract, not a courtesy.

Every message the client shows has a scope and a lifetime.
A screen that sends an empty list when its read fails makes "the server is down" and "you have no books" render identically, which is the most common way a terminal UI lies.

A TUI is a character grid with a one-line input idiom.
A multi-line prose editor with the user's own keymaps, undo, syntax and muscle memory already exists — it is their `$EDITOR` — and `git commit` established the pattern for handing off to it.

A palette hand-adapted from a design token file drifts silently, and CSS to 256 colours is lossy: contrast has to be measured where the palette is authored.

## Decisions

### Every read has a headless twin, in the same slice

Every read the TUI shows also exists as a non-interactive one-shot, and every verb inherits five obligations:

1. **A machine form and a pipe form.** `--json` emits a stable object. The bare/`status` form emits a **stable, field-ordered plain line** whose column order never changes and which uses `-` placeholders for absent fields, so `awk`/`cut` scripts do not break. Where a status-bar reduction makes sense, a `--short` form carries it (`short_status`).
2. **TTY-detected output.** Colour is applied only on a terminal and never when piped — `std::io::stdout().is_terminal()` gates every escape.
3. **`NO_COLOR`-respecting.** The same gate ANDs in `std::env::var_os("NO_COLOR").is_none()`.
4. **Meaningful exit codes.** Codes answer one question about the domain, not "did it print". The timer's answer *is it counting?* — `0` counting · `1` nothing running · `3` idle, reclaim pending · `4` not counting (paused / focus break). Write verbs exit `0` on success and `1` on refusal, with the reason on `stderr`.
5. **Never a silent divergence from the screen.** The refusal a verb prints and the notify tile the screen shows for the same mistake are one spelling of one outcome.

The twin is half the feature, not a follow-up: a ticket that adds a read ships the screen, the `--json`, the plain line, the exit codes and the shared copy in one slice.
Growth is additive — a new dimension is a new *field*, never a new shape; the offline write side added one `queued` flag to existing payloads rather than a second output format.
`src/timer_cli.rs` is the reference implementation: a new verb is written by reading it, not by inventing a shape.
Review is the deliberate exception: its loop is rate-and-advance, inherently interactive, and a one-shot would have no job to do. Every *read* has a twin; a screen whose value is entirely in the interaction does not manufacture one.

### The command palette is the third face of the same grammar

`:timer start` degrades to the identical `engineer timer start`; the `:` verb and the shell verb are one muscle, not two.
A new headless verb gains its palette row, and a palette verb that has a shell twin must not drift from it.

### One outcome has one spelling, from `src/messages.rs`

The wording is identical across the TUI tile, the inline panel line, and the headless `stderr`, so a script greps exactly what the screen shows.
A twin that must match a screen calls the same function: `messages::not_authenticated()` is the single source of the auth refusal, and `messages::fail_reason` describes a failed read for both the panel and its CLI twin.
The same *outcome* shares one spelling; a different outcome does not. A verb-specific offline refusal — `offline — can't resolve "{q}"; start bare or retry online` — tells the user exactly how to proceed, which is a different and better outcome than a generic refusal, so it is not flattened into `messages::offline(verb)`. That entry remains for the plain case, where there is nothing richer to say.

### A verb that cannot complete offline refuses with the way forward

It prints the refusal on `stderr` rather than synthesizing a result.

### Every message lands in exactly one of three tiers, chosen by scope and lifetime

```
Tier 1  footer notify tile   ui::notify       transient - a keystroke's outcome
Tier 2  inline panel state   ui::panel        persistent - one region's read failed
Tier 3  blocking screen      ui::blocking     persistent - whole screen unusable
        search states        ui::search       query-in-title, highlight, n/N
        one spelling         crate::messages  fail_reason / load_failed / offline / ...
```

The atoms live in `src/ui/`, and every screen and the quick-capture overlay adopts them.

### A read that failed never renders as empty

A failed read renders as itself — a loud Tier-2 panel with a retry key, never an empty list — and offline, stale and failed always render rather than being hidden to look tidy.
A new screen inherits this cheaply: it keeps a `failure: Option<PanelFailure>`, routes `Err(Unauthorized)` to `SessionExpired` and other errors to a `*LoadFailed(messages::fail_reason(...))`, and renders `render_panel_state` when the region has no rows.

### Tier 2 is a presentation enum, not a generic load-state wrapper

`PanelState` and `render_panel_state` are pure presentation.
Screens are heterogeneous: some hold `Vec<T>`, others hold an `Option<Box<Aggregate>>` mapping **one** read to **many** panels (Home's bands, Progress, Week, Review's stages, BookDetail, Audit).
So each screen keeps its own fields — the items or aggregate, `loading`, `failure: Option<PanelFailure>` — and computes a `PanelState` at render; the failed / empty / loading *body* is the only shared thing.

### Tier 3 has exactly two owners

A whole-screen blocking state is the loudest, rarest tier, reserved for when the screen is meaningless without what failed.
Only **Login** (its own read *is* the session) and the **global 401 re-auth interceptor** (`Action::SessionExpired`, handled in `App`) route there.
Every other screen's failure is a Tier-2 panel while its header timer, footer and nav stay live; at launch offline, reads fall back to cached read-only rather than blocking.

### `o open last-cached` ships hidden until a read-cache exists

The Tier-2 atom offers `r retry · o open last-cached`, but `PanelFailure.cached` is `false` everywhere, so the `o` affordance never renders — advertising a cache that is not there would hide a state.

### Search keeps each screen's `/`; `n`/`N` never touch the network

`/` stays whatever a screen already is — a server re-query (Books, Notes, Review browse) or an in-place filter (Activities).
`n`/`N` step the cursor over the *loaded* rows whose label matches, and `ui::search::highlight` paints the run.
Timer's `query` is a bind autocomplete, not list search, and is left alone.

### Long-form prose opens in `$EDITOR`; a line stays in the app's `i`/`Esc` grammar

A note's long-form body and the week's retro reflection open in the user's editor via the `git commit` pattern (`src/editor.rs`): spawn on a temp file, honour `$VISUAL` then `$EDITOR`, suspend the alt-screen around the child and restore it on exit, and read the saved buffer back.

- A one-line thought stays in the quick-capture overlay where five-second capture lives; a paragraph opens the editor. The in-TUI textarea is *replaced* for prose, not augmented.
- **Capture is sacred across the boundary.** Writing the buffer *is* the save — there is no autosave — and an aborted editor changes nothing.
- An empty buffer means what the target's contract says, distinguished from an abort: for the week note an empty body is a deliberate *clear*; a quit-without-write is not.
- Titles, queries and activity names stay in the app. The hand-off is for the *body*, not the *label*.

`src/editor.rs` is shared machinery, and any future prose surface uses it rather than inventing a panel; the fragile suspend/restore dance lives in exactly one place.
The same hand-off carries structured edits where a form would be heavier than the task: the queue's `e` edit-times on a rejected write opens the intent payload in `$EDITOR` and re-pends it with a fresh idempotency key ([client-state](client-state.md)); a parse-refused buffer changes nothing.

### The palette is generated from the one file that crosses from the design repository

```
engineer-cli-ds                            engineer-cli
---------------                            ------------
system/tokens/colors.css
  | rake tokens
  v
dist/tokens.toml  ------ mirror ------>    design/tokens.toml
                                             | tests/tokens.rs
                                             v
                                           src/ui/tokens.rs -> src/ui/theme.rs
```

`tests/tokens.rs` generates `src/ui/tokens.rs` — one `u8` per decision token, never an option — and fails the build when the two disagree; hand-editing either generated file fails CI.
`theme.rs` maps the application's names onto those decisions, and no other source file spells a colour index.
Ink on a fill is `text-inverse` rather than ANSI black, because indices 0-15 are the user's theme's to redefine.

## Rejected

- **A generic `LoadState<T>` wrapper.** It fits the flat lists but fights the aggregate screens and the `ListState`/`TableState` selection borrowed mutably beside the data at render.
- **Letting any screen go Tier 3.** A failed list read blanking the whole screen hides state to look tidy; Tier 2 keeps the chrome live.
- **Routing every CLI string through the catalogue.** It trades actionable, verb-specific offline guidance for a uniformity the model does not ask for.
- **Per-verb output, standardised later.** The plain form is a public interface the moment someone pipes it, and retrofitting column order is a breaking change with no deprecation channel.
- **`--json` only, no plain form.** It forces `jq` into a status-bar loop where a single `cut -f2` should do.
- **A shared output framework or trait every verb implements.** The obligations are five rules checkable by reading one `*_cli.rs` file; the one thing worth sharing is the copy, in `src/messages.rs`.
- **A headless twin for every screen, without exception.** A screen whose value is the interaction would manufacture a one-shot with no job.
- **A rich in-TUI textarea.** It competes with the user's editor and loses — no personal keymaps, no undo tree, no syntax — while adding a surface to maintain.
- **A bundled minimal modal editor of our own.** The same loss, with more code.
- **Prose only on the web.** It breaks the daily loop at exactly the moment the user has something worth writing.
- **Honouring `$EDITOR` only, not `$VISUAL`.** Git's precedence is what users already have configured.
- **`cargo xtask tokens`.** An xtask makes the crate a workspace, and cargo-dist reads the workspace to build releases; a test runs in the gate CI already has.
- **A `build.rs` generating into `OUT_DIR`.** The palette would never appear in a diff, so a colour change would reach a release unread.

## Left open

- **`o open last-cached` and the read-cache behind it.** Reopens when a screen keeps a read-cache; the Tier-2 atom is already cache-ready.
- **Verb-specific offline copy on the TUI screens.** Reopens when a screen has something richer to say than `messages::offline(verb)`, as the CLIs do.
- **A status-bar "N due" reduction for Review.** Reopens if one is wanted; it reuses the dashboard read and inherits the headless contract.
- **Work-state token decisions.** The token set has none, so `theme.rs` maps work states onto the notice decisions (`SUCCESS`, `WARN`, `DANGER`) — the right hues, named for a different purpose. Reopens when the design system adds them ([engineer-cli-ds#2](https://github.com/dsaenztagarro/engineer-cli-ds/issues/2)).

## References

`src/timer_cli.rs` · `src/messages.rs` · `src/ui/notify.rs`, `panel.rs`, `blocking.rs`, `search.rs` · `src/editor.rs` · `tests/tokens.rs` · [`AGENTS.md`](../../AGENTS.md) (refreshing the palette) · [UI rendering](../ui-rendering.md)
