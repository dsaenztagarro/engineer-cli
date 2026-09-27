# Decision records

**A few broad themes, not a record per decision.**
A new decision amends the record that owns its theme; it does not open a sibling.
Each record answers the question a future maintainer actually asks — *why is it built this way?* — which is the one thing a closed pull request throws away.

| Record | Owns | Decision log |
|---|---|---|
| [Scope](scope.md) | what earns a place in the terminal versus the web; the design boundary and where design content lives | [#209](https://github.com/dsaenztagarro/engineer-cli/issues/209) |
| [Terminal surface](terminal-surface.md) | the TUI and headless contract; the three message tiers; one spelling per outcome; `$EDITOR` for prose; the palette pipeline | [#210](https://github.com/dsaenztagarro/engineer-cli/issues/210) |
| [Client state](client-state.md) | derived, never stored; the offline write queue; the read cache | [#211](https://github.com/dsaenztagarro/engineer-cli/issues/211) |

A record is named for its theme, not numbered: it is amended in place, so an order of creation says nothing, and a citation reads as the theme it points at — code cites "the client-state record", never a number.
A theme is drawn wide enough that the next decision in its area lands in it; a decision that fits none is a new theme, and its record argues, in its own context section, which themes were considered and why the decision fits none of them.
An `ADR NNNN` met in an old commit, pull request or CHANGELOG entry names a numbered record since folded into one of these themes; each theme's decision-log issue lists the numbers it absorbed.

## The bar

A record holds reasoning that is not recoverable from the code: a real fork with at least two defensible options, a consequence beyond the change, and nothing else able to hold it.
Behaviour belongs in tests, a mechanism in its topic's guide under `docs/`; a record is for the reasoning.

## Format

Each record states what is **currently believed**, not how the project got here.

- **Status:** Accepted · **Decision log:** the theme's issue, then one or two sentences on what the theme decides.
- **Context · Decisions · Rejected · Left open · References.** A decision is a `###` heading stating the position, and a few sentences on what it rules out.
- **A rejected option earns its line only if someone would reach for it tomorrow.** One line: the option, and why it lost. Its job is stopping the rebuild.
- **A fork deliberately left open always stays**, with the condition that would reopen it.
- Diagrams are ASCII; prose is one line per paragraph or semantic line breaks.

## On immutability

Immutability exists to stop a decision being silently rewritten so no reader can tell it changed.
It is a means, not the end.

A themed record keeps that property by a different mechanism: an amendment changes the record in place and adds one dated line to the theme's `decision-log` issue — what changed, and why, linking the commit — and git holds the prior wording. `git log --follow -p docs/adr/<theme>.md` is the history a chain of superseded files was only approximating.

The log lives on the issue rather than at the foot of the record because a log inside the record grows for as long as the theme is alive, and every line of it is loaded by every reader — and every agent — who opens the record for its current position.

What stays forbidden is changing a position without logging it.
