# Design briefs — engineer-cli

Handoff briefs for the **terminal** client. A brief is a problem-first **input** to design; the rendered terminal mockups (`../*.dc.html`, ratatui-faithful) and the shipped Rust screens are the **outputs**.

Read [`../README.md`](../README.md) first — it is the terminal **design kit** (palette mapping, chrome conventions, translate/don't-translate rules, and the `design-system.dc.html` style anchor). A brief says *what to build and why*; the kit says *how it must look and feel in a character grid*. The two are read together.

Every brief is measured against the governing principle — **sterling, not a replica** — before it is designed: the terminal owns the study loop's high-frequency core as *glances and gestures*, not a port of the `engineer` web UI. Depth stays on the web. That law, the six-point glance-or-gesture test, and the standing non-goals live in [ADR 0002](../../architecture/decisions/0002-sterling-not-a-replica.md).

## Lifecycle — a brief is an input, and it ends at ship

```
proposed/  ->  (Claude Design produces the screens, the CLI implements them)  ->  deleted
```

**An artefact written to *produce* something is an input, and its lifecycle ends when the thing it produced ships.**
A design brief is deleted once the surface it asked for is live.

Everything in a brief is written in the future tense — *design this, draw that, here is what it must do* — so the moment the surface exists, every sentence is a claim about the past in the wrong tense, and the document starts telling its reader to do work already done. An archive of shipped briefs is not a reference; it is a set of instructions to rebuild what you already have.

**Deleting is not losing.** Before a brief goes, its durable half must already have a home:

| What the brief carried | Where it lives afterwards |
|---|---|
| A decision — a trade-off weighed, a path chosen, a thing deliberately *not* built | An ADR in [`../../architecture/decisions/`](../../architecture/decisions/) |
| What a surface looks like | The area's `.dc.html` board, and the kit in [`../README.md`](../README.md) |
| What a surface does | A test |
| Work that is still open — a residual gap, a deferral, a server ask | A GitHub issue |

Only then delete it. The prose itself stays recoverable — `git log --diff-filter=D -- docs/designs/briefs/` finds the commit, `git show <sha>:<path>` prints the file.

**Status belongs on the issue tracker, not in a folder name.** A brief's directory says one thing: it is still an open input. Nothing here records what shipped — the CHANGELOG, the issues, and the tags do that.

**Code cites a decision or a test — never a brief.** A brief is an input, so a source comment pointing at one is pointing at something scheduled for deletion. Cite the ADR for *why* and the test for *what*.

## Index

*Nothing proposed. New work starts here: write the brief, drop it in `proposed/`, and delete it at close-out once its durable half has landed in an ADR, a board, a test, or an issue.*

## Writing a brief

Match the house format: a For / Produces / Status header; a first-person workflow; jobs-as-outcomes; the principles that genuinely bind; an orientation section that states the *shipped reality*; the API the module consumes with **verified** routes; a hard visual-language constraint; out-of-scope; and phasing. Bind the visual language to **this repo's kit** ([`../README.md`](../README.md), [`../design-system.dc.html`](../design-system.dc.html)), not the web design system.

Keep it problem-first and non-prescriptive: name existing screens, widgets, and the pre-built API client as *reuse context*, never as the prescribed answer.

Two things a brief must do so it can be deleted cleanly at close-out:

- **Name the ADR carrying any decision its surface depends on** — write the record as the decision is made, not at delete time. This is the backstop that keeps the reasoning when the brief goes.
- **Record open work as issues, not as prose notes.** A "residual gap" or "deferred for now" line inside a brief is work status hiding in a document nobody re-reads; file it.
