# Working in engineer-cli

Conventions an agent (or a new contributor) needs before touching this repo. The architecture itself is [`docs/README.md`](docs/README.md); this file is about how work is *kept*.

## The input lifecycle — an input ends when the thing it produced ships

**An artefact written to *produce* something is an input, and its lifecycle ends when the thing it produced ships.**

A backend design is deleted at epic close-out. A review is folded into a decision record and deleted. **A design brief is deleted once the surface it asked for is live.**

Everything in an input is written in the future tense — *design this, draw that, here is what it must do* — so the moment the surface exists, every sentence is a claim about the past in the wrong tense, and the document starts telling its reader to do work already done. Git history keeps the text: `git log --diff-filter=D -- <path>` finds the deleting commit, `git show <sha>:<path>` prints the file.

Deleting is not losing, because the durable half lands somewhere first:

| What the input carried | Where it goes before the input is deleted |
|---|---|
| A decision — a trade-off weighed, a path chosen, a thing deliberately *not* built | An ADR in [`docs/architecture/decisions/`](docs/architecture/decisions/) |
| What a surface looks like | The area's page and the design kit in [`engineer-cli-ds`](https://github.com/dsaenztagarro/engineer-cli-ds) |
| What a surface does | A test |
| Work that is still open — a residual gap, a deferral, a server ask | A GitHub issue |

Two rules follow from this, and both are load-bearing:

- **Status belongs on the issue tracker, never in a folder name or a document header.** A file's location says whether it is still an open input — nothing more. What shipped is recorded by the CHANGELOG, the issues, and the tags.
- **Code cites a decision or a test — never an input.** A source comment pointing at a brief is pointing at something scheduled for deletion. Cite the ADR for *why* and the test for *what*.

Design briefs and their lifecycle: [`engineer-cli-ds/briefs/README.md`](https://github.com/dsaenztagarro/engineer-cli-ds/blob/master/briefs/README.md).

## The design boundary

Design content lives in [`engineer-cli-ds`](https://github.com/dsaenztagarro/engineer-cli-ds): the pages, the design kit, the briefs, and the palette. It is not copied here ([ADR 0006](docs/architecture/decisions/0006-the-design-boundary.md)).

**Exactly one file crosses**, and it is data. `design/tokens.toml` is a mirror of `engineer-cli-ds/dist/tokens.toml`, and `tests/tokens.rs` generates `src/ui/tokens.rs` from it. Edit neither: refresh the palette with

```sh
cp ../engineer-cli-ds/dist/tokens.toml design/tokens.toml
UPDATE_TOKENS=1 cargo test --test tokens
```

A colour is named through `src/ui/theme.rs`, which maps onto the generated decisions; no other file spells an index or an ANSI colour, and the same test fails when one does.

A page shows the **surface**. What the code *does* is decided here — a behaviour a page draws that nothing here has settled is an open question, not a spec.

## Code documents nothing a test can hold

A comment that states behaviour is a spec written where nothing can fail. Before writing one down, ask **what test would fail if this were false?** — then write that test, named as the rule, and stop.

In order, each step reached only when the one above cannot carry it:

1. **A test.** `grep 'fn ' <file>` over a test module should reconstruct what the thing does.
2. **A document under `docs/`**, for a contract no test can hold. The bar is high; a prose shadow of the code drifts.
3. **A comment**, for the local delta only: an invariant invisible from the signature, a footgun the obvious rewrite walks into, a workaround and why. A module header is one line saying what the module *is*.

**Never a design reference, in any form** — a page path, a section label, or the same thing spelled out in words: a page regenerates, and its labels renumber. **An ADR citation is fine**: it records *why*, which no test can, and it is hand-owned here. `tests/design_references.rs` fails on a design reference in `src/`.

## Decision records

Non-trivial architecture and cross-cutting choices get an ADR under [`docs/architecture/decisions/`](docs/architecture/decisions/) — see that folder's README for the format. Write the record **as the decision is made**, not when an input is about to be deleted: the record is the backstop that lets the input go.

A record shows the evaluation — the options considered, the trade-offs, the chosen path — not just the outcome. Knowing when *not* to ship something, and writing down why, is as valuable as the code. A documented "we evaluated X and deliberately deferred it, here's how to do it right" beats a half-built feature; don't ship inert scaffolding that demonstrates an anti-pattern.

Records are immutable once accepted: supersede with a new record rather than editing an old one in place.

## The standards a change is measured against

- [ADR 0001](docs/architecture/decisions/0001-terminal-error-notification-model.md) — the error, notification & search-state model. A read that failed never renders as "empty"; one spelling per outcome across the screen, the panel, and `stderr`.
- [ADR 0002](docs/architecture/decisions/0002-sterling-not-a-replica.md) — sterling, not a replica. The six-point glance-or-gesture test a surface must pass to live in the terminal, and the standing non-goals. Check a growth request against it *before* designing.
- [ADR 0003](docs/architecture/decisions/0003-tui-headless-contract.md) — the TUI ↔ headless contract. Every read is also a one-shot, in the same slice: `--json`, a stable plain line, TTY-detect, `NO_COLOR`, meaningful exit codes.
- [ADR 0004](docs/architecture/decisions/0004-derived-never-stored-and-the-write-queue.md) — derived, never stored, and the write queue as its one exception. Before adding a write, answer: *can this be synthesized honestly offline?*
- [ADR 0005](docs/architecture/decisions/0005-editor-for-prose.md) — `$EDITOR` for prose; the in-app `i`/`Esc` grammar for a line.
- [ADR 0006](docs/architecture/decisions/0006-the-design-boundary.md) — the design boundary. The design project draws the surface; tests and ADRs here hold the truth; one data file crosses.

## House conventions

- **Reuse before you build.** New screens are TEA modules under `src/app/screens/` wired through the `Action` enum and reducer; presentation reuses `src/ui/` chrome and widgets (`bordered`, `status_pill`, `progress_bar`, `panel`, `notify`, `picker`) — no bespoke chrome. API calls go through `ApiClient` with typed models. Errors surface as notify tiles or Tier-2 panels, never panics.
- **Keyboard grammar.** neovim-flavoured — `j`/`k`, `gg`/`G`, `/`, `n`/`N`, `:cmd`, `<Space>` leader, `i`/`Esc` in forms. The footer must advertise the active keys.
- **Keep the docs in step with the change.** Touching the API layer updates [`docs/api-layer.md`](docs/api-layer.md); changing commands or flags updates the Commands section of [`README.md`](README.md); every user-visible change adds a `CHANGELOG.md` `[Unreleased]` entry.
- **Markdown prose is one line per paragraph** (or [semantic line breaks](https://sembr.org) at sentence and clause boundaries) — never hard-wrapped at a fixed column, so diffs stay word-level.
- **Local tests are the gate.** `cargo test`, `cargo fmt --all -- --check`, and `cargo clippy --all-targets --all-features -- -D warnings` all pass before a PR merges.
