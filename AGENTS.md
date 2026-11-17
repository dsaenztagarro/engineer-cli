# Working in engineer-cli

Conventions an agent (or a new contributor) needs before touching this repo. The architecture is the code; this file is about how work is *kept*.

## The input lifecycle — an input ends when the thing it produced ships

**An artefact written to *produce* something is an input, and its lifecycle ends when the thing it produced ships.**

A backend design is deleted at epic close-out. A review is folded into a decision record and deleted. **A design brief is deleted once the surface it asked for is live.**

Everything in an input is written in the future tense — *design this, draw that, here is what it must do* — so the moment the surface exists, every sentence is a claim about the past in the wrong tense, and the document starts telling its reader to do work already done. Git history keeps the text: `git log --diff-filter=D -- <path>` finds the deleting commit, `git show <sha>:<path>` prints the file.

Deleting is not losing, because the durable half lands somewhere first:

| What the input carried | Where it goes before the input is deleted |
|---|---|
| A decision — a trade-off weighed, a path chosen, a thing deliberately *not* built | Its theme's decision record in [`docs/adr/`](docs/adr/) |
| What a surface looks like | The area's page and the design kit in [`engineer-cli-ds`](https://github.com/dsaenztagarro/engineer-cli-ds) |
| What a surface does | A test |
| Work that is still open — a residual gap, a deferral, a server ask | A GitHub issue |

Two rules follow from this, and both are load-bearing:

- **Status belongs on the issue tracker, never in a folder name or a document header.** A file's location says whether it is still an open input — nothing more. What shipped is recorded by the CHANGELOG, the issues, and the tags.
- **Code cites a decision or a test — never an input.** A source comment pointing at a brief is pointing at something scheduled for deletion. Cite the decision record for *why* and the test for *what*.

Design briefs and their lifecycle: [`engineer-cli-ds/briefs/README.md`](https://github.com/dsaenztagarro/engineer-cli-ds/blob/master/briefs/README.md). Closing one out once its surface ships is the `design:brief-closer` agent.

## The design boundary

Design content lives in [`engineer-cli-ds`](https://github.com/dsaenztagarro/engineer-cli-ds): the pages, the design kit, the briefs, and the palette. It is not copied here ([scope](docs/adr/scope.md)).

**Exactly one file crosses**, and it is data. `design/tokens.toml` is a mirror of `engineer-cli-ds/dist/tokens.toml`, and `tests/tokens.rs` generates `src/ui/tokens.rs` from it. Edit neither: refresh the palette with

```sh
cp ../engineer-cli-ds/dist/tokens.toml design/tokens.toml
UPDATE_TOKENS=1 cargo test --test tokens
```

A colour is named through `src/ui/theme.rs`, which maps onto the generated decisions; no other file spells an index or an ANSI colour, and the same test fails when one does.

A page shows the **surface**. What the code *does* is decided here — a behaviour a page draws that nothing here has settled is an open question, not a spec.

**This client renders what the Engineer API serves.** A designed workflow that needs an endpoint the API does not expose is never faked client-side: its ticket waits, naming what the API must ship first, and the gap goes to the area's brief, so the surface stays visible as unbuilt.

### The names the design skills read

The `design` plugin's skills and agents read these names and no others; a name missing here is asked for, never guessed.

- **Design repository:** `engineer-cli-ds`
- **Design checkout:** `$ENGINEER_CLI_DS` (default `../engineer-cli-ds`, beside this repository)
- **Regenerate command:** `rake tokens`
- **Design suite:** `rake`
- **Artefact form:** token set
- **Artefact source:** `dist/tokens.toml`
- **Artefact vendored at:** `design/tokens.toml`, from which `src/ui/tokens.rs` is generated
- **Cascade layer:** none
- **Vendor command:** the copy and `UPDATE_TOKENS=1 cargo test --test tokens` above; it stamps no revision
- **Triage command:** none
- **Own component styles:** `src/ui/`, where `theme.rs` maps the tokens onto the widgets
- **Shared design system:** `cli-ds`
- **Shared system channel:** vendored into `engineer-cli-ds` by its `bin/vendor-sync` and folded into the token set; it never reaches this repository on its own
- **Shared system checkout:** `$CLI_DS` (default `../cli-ds`)
- **Shared release ref:** the `cli-ds` tag `v<version>`
- **Shared release notes:** cli-ds's GitHub releases, `gh release view v<version> -R dsaenztagarro/cli-ds`
- **Pin command:** `bin/vendor-sync`, run in the design checkout
- **Token sync command:** none; the shared tokens arrive inside the token set
- **Decision records:** `docs/adr/`

## Code documents nothing a test can hold

A comment that states behaviour is a spec written where nothing can fail. Before writing one down, ask **what test would fail if this were false?** — then write that test, named as the rule, and stop.

In order, each step reached only when the one above cannot carry it:

1. **A test.** `grep 'fn ' <file>` over a test module should reconstruct what the thing does.
2. **A document under `docs/`**, for a contract no test can hold. The bar is high; a prose shadow of the code drifts.
3. **A comment**, for the local delta only: an invariant invisible from the signature, a footgun the obvious rewrite walks into, a workaround and why. A module header is one line saying what the module *is*.

**Never a design reference, in any form** — a page path, a section label, or the same thing spelled out in words: a page regenerates, and its labels renumber. **A decision-record citation is fine**: it records *why*, which no test can, and it is hand-owned here. It names the theme in words — "the client-state record", "engineer's api-wire record" — never a number.

## Decision records

**A few broad themes, not a record per decision.** See [`docs/adr/README.md`](docs/adr/README.md) for the themes.
A new decision amends the record that owns its theme; it does not open a sibling, and a new theme has to argue why no existing one fits.
Every record names its decision-log issue, and an amendment adds one dated line there; changing a position without logging it is never permitted.
The log is kept off the record so a record's size tracks its current position, not its history.

A rejected option earns its space only if someone would reach for it tomorrow — one line, the option and why it lost.
A fork deliberately left open always stays, with the condition that would reopen it.

Write the record **as the decision is made**, not when an input is about to be deleted: the record is the backstop that lets the input go.

## The standards a change is measured against

- [Scope](docs/adr/scope.md) — sterling, not a replica: the six-point glance-or-gesture test a surface must pass to live in the terminal, and the standing non-goals. Check a growth request against it *before* designing. Also the design boundary: the design project draws the surface; tests and records here hold the truth.
- [Terminal surface](docs/adr/terminal-surface.md) — every read is also a one-shot, in the same slice (`--json`, a stable plain line, TTY-detect, `NO_COLOR`, meaningful exit codes); a read that failed never renders as "empty"; one spelling per outcome across the screen, the panel and `stderr`; `$EDITOR` for prose and the `i`/`Esc` grammar for a line; one generated palette.
- [Client state](docs/adr/client-state.md) — derived, never stored, and the write queue as its one exception. Before adding a write, answer: *can this be synthesized honestly offline?*

## House conventions

- **Reuse before you build.** New screens are TEA modules under `src/app/screens/` wired through the `Action` enum and reducer; presentation reuses `src/ui/` chrome and widgets (`bordered`, `status_pill`, `progress_bar`, `panel`, `notify`, `picker`) — no bespoke chrome. API calls go through `ApiClient` with typed models, and every call is traced. Errors surface as notify tiles or Tier-2 panels, never panics.
- **Every workflow a design draws has a test.** Each API call a screen drives is tested against wiremock, and each keyboard workflow in the reducer, Action in and state out; a merged ticket without one is an open gap.
- **Never push a release tag.** A version tag triggers cargo-dist, which builds the installers and publishes the Homebrew formula to the public tap. A release is prepared in a pull request — the version in `Cargo.toml` and the CHANGELOG section — and the tag push is handed to the user.
- **Keyboard grammar.** neovim-flavoured — `j`/`k`, `gg`/`G`, `/`, `n`/`N`, `:cmd`, `<Space>` leader, `i`/`Esc` in forms. The footer must advertise the active keys.
- **Keep the README and CHANGELOG in step with the change.** Changing commands or flags updates the Commands section of [`README.md`](README.md); every user-visible change adds a `CHANGELOG.md` `[Unreleased]` entry.
- **Markdown prose is one line per paragraph** (or [semantic line breaks](https://sembr.org) at sentence and clause boundaries) — never hard-wrapped at a fixed column, so diffs stay word-level.
- **Local tests are the gate.** `cargo test`, `cargo fmt --all -- --check`, and `cargo clippy --all-targets --all-features -- -D warnings` all pass before a PR merges.

## Runs on the agent-sdlc template

This repository's agentic workflow follows the [agent-sdlc](https://github.com/dsaenztagarro/agent-sdlc) template.
Its skills and agents arrive as the template's `sdlc` and `design` Claude Code plugins, installed at user level; this repository keeps no copy of them, and what is particular to engineer-cli is stated here.
A design page becomes an epic through `/sdlc:plan`.
When you find a **reusable, project-agnostic** improvement to the workflow itself, don't apply it only here: run the `/sdlc:template-feedback` skill to propose it and, on the maintainer's OK, add it to the template's open feedback issue. Project-specific rules stay here.
