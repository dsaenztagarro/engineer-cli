# 0003 — The TUI ↔ headless contract: every read is also a one-shot

**Status:** Accepted (proven by `engineer timer`, held by every verb the design roadmap added; recorded here at #180)

## Context

This client has two faces over one domain: an interactive TUI and a set of non-interactive verbs.
The cheap way to build the second is to let each verb invent its own output — some colour, some JSON, some prose — and to add a one-shot only when someone asks for it.
That is how a CLI ends up unscriptable: a status line whose columns move between releases, ANSI escapes in a pipe, an exit code that means "it printed something", and a refusal message that reads differently on `stderr` than the same refusal does on screen.

The terminal's distinctive advantage over a small-screen client is that its output composes — into `jq`, into git hooks, into a zellij or tmux status bar ([ADR 0002](0002-sterling-not-a-replica.md) criterion 5).
That advantage only exists if the composable form is a *contract*, not a courtesy.

`engineer timer` (`src/timer_cli.rs`) was built to that standard first and became the reference implementation.

## Decision

**The TUI ↔ headless duality is first-class: every read the TUI shows must also exist as a non-interactive one-shot, and every verb inherits the same five obligations.**
The twin is not a follow-up — it is half the feature, and it lands in the same slice as the screen.

1. **A machine form and a pipe form.** `--json` emits a stable object. The bare/`status` form emits a **stable, field-ordered plain line** whose column order never changes and which uses `-` placeholders for absent fields, so `awk`/`cut` scripts do not break. Where a status-bar reduction makes sense, a `--short` form carries it (`short_status`).
2. **TTY-detected output.** Colour is applied only on a terminal and never when piped — `std::io::stdout().is_terminal()` gates every escape.
3. **`NO_COLOR`-respecting.** The same gate ANDs in `std::env::var_os("NO_COLOR").is_none()`, so even a TTY gets clean text when the variable is set.
4. **Meaningful exit codes.** Codes answer one question about the domain, not "did it print". The timer's answer *is it counting?* — `0` counting · `1` nothing running · `3` idle, reclaim pending · `4` not counting (paused / focus break). Write verbs exit `0` on success and `1` on refusal, with the reason on `stderr`.
5. **Never a silent divergence from the screen.** The refusal a verb prints and the notify tile the screen shows for the same mistake are one spelling of one outcome. This is the rule [ADR 0001](0001-terminal-error-notification-model.md) implements: the shared catalogue in `src/messages.rs` is the single source, so a script greps exactly what the screen displays.

Additive growth only: when a verb gains a new dimension it gains a *field*, never a new shape. The offline write side added one `queued` flag to existing payloads rather than a second output format.

## Alternatives considered

- **Per-verb output, standardised later.** Rejected: the plain form is a public interface the moment someone pipes it. Retrofitting stable column order into a shipped verb is a breaking change with no deprecation channel.
- **`--json` only, no plain form.** Rejected: it forces `jq` into a status-bar loop and into shell one-liners where a single `cut -f2` should do. The plain form is the one people actually bind to a keystroke.
- **A shared output framework / trait every CLI implements.** Rejected as over-abstraction for the size of the surface: the obligations are five rules, verifiable by reading a `*_cli.rs` file, and `src/timer_cli.rs` is a better teaching artefact than a trait. The one thing genuinely worth sharing is the *copy* — and that is `src/messages.rs`, not a rendering layer.
- **A headless twin for every screen, without exception.** Rejected as a reflex. Review is the deliberate exception: its loop is rate-and-advance, inherently interactive, and a one-shot would have no job to do. The rule is that every *read* has a twin; a screen whose value is entirely in the interaction does not manufacture one. If a status-bar "N due" reduction is ever wanted it reuses the dashboard read and inherits this contract.

## Consequences

- `src/timer_cli.rs` is the reference implementation. A new verb is written by reading it, not by inventing a shape.
- Every verb the roadmap added holds the contract with no exceptions: `engineer target` (`src/target_cli.rs`), `engineer log` (`src/log_cli.rs`), `engineer progress` (`src/progress_cli.rs`), `engineer week` / `plan` / `reflect` (`src/week_cli.rs`), `engineer note` (`src/note_cli.rs`), `engineer inbox` and its `sources`/`connect`/`disconnect`/`sync` family (`src/inbox_cli.rs`), `engineer today`, and `engineer queue` with `resolve`/`drop` (`src/queue_cli.rs`).
- A verb that cannot honestly complete offline refuses with the way forward on `stderr` rather than synthesizing a result — and the refusal copy is verb-specific where verb-specific guidance is genuinely better than a generic template (ADR 0001, "the §C reconciliation").
- The palette is the third face of the same grammar: `:timer start` degrades to the identical `engineer timer start`. The `:` verb and the shell verb are one muscle, not two — so a new headless verb should gain its palette row, and a palette verb that has a shell twin must not drift from it.
- **Definition of done for any ticket that adds a read:** the screen, the `--json`, the plain line, the exit codes, and the shared copy — in one slice.
