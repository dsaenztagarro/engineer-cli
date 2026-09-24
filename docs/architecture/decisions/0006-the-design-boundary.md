# 0006 — The design boundary: the design project draws the surface, this repository holds the truth

**Status:** Accepted (#189, #190)

## Context

The terminal client is designed in a Claude Design project and built here.
Until this record, the two were joined by copies and citations:

- `docs/designs/` carried the project's canvases, the design kit and a brief — 920 KB of design content in a repository that ships a binary, and growing without bound.
- 37 source comments cited a canvas or one of its section labels, and eight of those named files that live in the web client's repository and never existed here.
- The canvases carried **behaviour**. An audit of all ten (#189) extracted 69 rules. Eleven had been deliberately re-decided while building and recorded here, and none of those could be corrected on the canvas: the design project cannot see this repository, so the next export restored every stale sentence. Nine more were **work status** — "blocked on API", "no weeks client is built yet" — false the moment the work moved.
- `src/ui/theme.rs` was hand-adapted from a token file that had since moved, and its header cited a path that no longer existed. Once the palette was authored where contrast is measured, two of its colours turned out to fail their floors.

## Decision

```
  Claude Design project          engineer-cli-ds                    engineer-cli
  ---------------------          ---------------                    ------------
  draws the surface   --export-> pages/*.dc.html   (read by /epic)
                                 system/tokens/colors.css
                                   | rake tokens
                                   v
                                 dist/tokens.toml  --mirror-->  design/tokens.toml
                                                                  | tests/tokens.rs
                                                                  v
                                                                src/ui/tokens.rs -> theme.rs
```

1. **The design project ships surface only.** What a screen looks like is the design's; what it does is this repository's — a test for the *what*, an ADR for the *why*. When drawing a state forces a behaviour fork a brief did not settle, the design returns it as an open question, never a ruling.
2. **Code never references a design, in any form** — a canvas path, a section label, or the same thing spelled out in words. An ADR citation is fine: it is hand-owned, stable and correctable here. `tests/design_references.rs` fails on a design reference in `src/`.
3. **Exactly one file crosses the boundary**, and it is data: `engineer-cli-ds/dist/tokens.toml`, mirrored to `design/tokens.toml`. `tests/tokens.rs` generates `src/ui/tokens.rs` from it — one `u8` per decision token, never an option — and fails the build when the two disagree. `theme.rs` maps the application's names onto those decisions, and no other source file spells a colour index.
4. **Design content lives in `engineer-cli-ds`**: the pages, the kit, the briefs and the palette. `/epic` reads a page from a sibling checkout, `${ENGINEER_CLI_DS:-../engineer-cli-ds}/pages/`, the same convention `engineer-cli-ds` uses to find `cli-ds`.

## Alternatives considered

- **Re-point each citation at the page's new location.** Rejected: it is the same defect with a different path, and a canvas regenerates.
- **A prose specification beside the code for every rule.** Rejected as the default: prose that describes behaviour drifts, and a test cannot. It stays available for a contract no test can hold.
- **`cargo xtask tokens`.** Rejected: an xtask makes the crate a workspace, and cargo-dist reads the workspace to build releases. A test runs in the gate CI already has.
- **A `build.rs` generating into `OUT_DIR`.** Rejected: the palette would never appear in a diff, so a colour change would reach a release without anyone reading it.
- **`/epic` fetching a page by URL, or through an index kept here.** A URL needs `gh` auth and a network read per page; an index is one more file to drift. The sibling checkout is what the design repositories already assume.

## Consequences

- The palette changed at the cutover, each change measured in `engineer-cli-ds`:
  - the selected-row fill moves from `61` to `103`, because `61` measured 3.34 against the 4.5 floor for ink on a fill;
  - the border grey moves from `240` to `244`, because `240` measured 2.63 on a dark ground, under the 3.0 a mark needs;
  - ink on a fill is `text-inverse` (`233`) rather than ANSI black, because indices 0–15 are the user's theme's to redefine.
- Refreshing the palette is two commands: `cp ../engineer-cli-ds/dist/tokens.toml design/tokens.toml`, then `UPDATE_TOKENS=1 cargo test --test tokens`. Hand-editing either generated file fails CI.
- The token set has no decisions for work states yet. `theme.rs` maps them onto the notice decisions (`SUCCESS`, `WARN`, `DANGER`), which are the right hues but are named for a different purpose. That gap belongs to the design system, not to this repository.
- A behaviour a canvas draws and nothing here tests is not a specification of this client. It becomes an issue, or it is corrected on the canvas.
