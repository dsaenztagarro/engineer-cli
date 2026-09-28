---
name: read-designs
description: Read the palette the design repository published. It vendors dist/tokens.toml, regenerates src/ui/tokens.rs, reports what changed by decision, and then hands any satisfied brief to brief-closer. Use after engineer-cli-ds has a new dist/tokens.toml, or when asked to refresh the palette.
disable-model-invocation: true
allowed-tools: Bash, Read, Edit, Glob, Grep, Agent
---

# Read designs (engineer-cli)

**Read** takes what the design repository published. **Adopt** makes this application use it.
They are separate steps. This command does the first, and reports what the second has to do.

Exactly one artefact crosses the boundary, and it is data:

```
engineer-cli-ds/dist/tokens.toml  ->  design/tokens.toml
```

Pages and briefs never come this way. A person building a surface reads a page; a page is never a build input.

## 1. Read: vendor the token set

The design checkout is a sibling, `DS="${ENGINEER_CLI_DS:-../engineer-cli-ds}"`, the same convention `/epic` uses. If `$ARGUMENTS` names another checkout, use that.
Pull it first, so you read what was published rather than a stale clone:

```sh
git -C "$DS" pull --ff-only
```

Refuse to vendor from a design checkout whose own suite is red. A `dist/` that disagrees with its palette is not a published token set:

```sh
(cd "$DS" && rake)
```

Then vendor and regenerate:

```sh
cp "$DS/dist/tokens.toml" design/tokens.toml
UPDATE_TOKENS=1 cargo test --test tokens
cargo test --test tokens
```

The second run is the check. It fails if the committed `src/ui/tokens.rs` differs from what the token set generates, or if a decision lands on one of the terminal's 16 theme slots.

## 2. Report: what changed, by decision

A file-level diff only says "tokens.toml changed", which decides nothing. Report by decision:

```sh
git diff -- design/tokens.toml | grep -E '^[-+](name|rgb|indexed|references)'
```

Say which decisions **appeared**, which **changed** (a new hue, a new index, a new option behind them), and which **disappeared**.
A disappearance is the dangerous one. A constant `src/ui/theme.rs` still names but the token set no longer defines fails the build.

## 3. Adopt: only what the report requires

When a decision disappears or is renamed, the fix is to map `theme.rs` onto the decision the design repository now defines. Never re-add the old constant by hand.
A decision that only changed hue needs nothing here. The screens name the decision, and the new colour arrives with it.

If the palette cannot express what a surface needs, that is a finding for the design repository: a palette change there, or a brief. It is never a local colour.

## 4. Close out: hand any satisfied brief to `brief-closer`

An export usually lands because a brief asked for it, and a brief ends when its surface ships.
Check `"$DS/briefs/proposed/"` against what just arrived and what this application now builds.
If a brief's ask looks landed, spawn the [`brief-closer`](../../agents/brief-closer.md) agent with the brief's name.

## 5. Do not commit

Leave the working tree for review.
The two halves can be reviewed separately. `git diff -- design/ src/ui/tokens.rs` is what arrived; everything else is what this application changed to use it.
