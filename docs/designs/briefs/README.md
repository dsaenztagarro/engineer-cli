# Design briefs — engineer-cli

Handoff briefs for the **terminal** client, mirroring the structure of [`engineer/docs/designs/briefs/`](../../../engineer/docs/designs/briefs/). A brief is a problem-first **input** to design; the rendered terminal mockups (`docs/designs/*.html`, ratatui-faithful) and the shipped Rust screens are the **outputs**.

Read [`../README.md`](../README.md) first — it is the terminal **design kit** (palette mapping, chrome conventions, translate/don't-translate rules, the `books.html` style anchor). A brief here says *what to build and why*; the kit says *how it must look and feel in a character grid*. The two are read together.

## Lifecycle — where a brief lives tells you its status

```
proposed/  ->  (Claude Design produces the .html screens, the CLI implements them)  ->  shipped/
```

- **`proposed/`** — written, awaiting design and/or implementation.
- **`shipped/`** — designed and live in the CLI. Kept, not deleted, so the reasoning stays next to the code.

When a proposed brief ships, `git mv` it `proposed/ -> shipped/` and flip its row below. The move is the status signal.

## Index

### Proposed

| Brief | What it covers | Depends on |
|---|---|---|
| [`terminal-client.brief.md`](proposed/terminal-client.brief.md) | The full terminal study loop for a neovim/zellij power user — timer + ambient status presence, pace meters, activities/segments, review, the command-palette verb line, and a TUI↔headless/`--json` duality. Bundles the transferable slice of every `engineer` surface into one terminal-native client. | Engineer APIs that already exist (timer, activities, segments, progress/targets, review). Its *advanced* surfaces trail three `engineer` briefs — see §Phasing in the brief. |

### Shipped

*(none yet — the CLI's built screens predate the briefs workflow; see the kit README's screen inventory.)*

## Writing a brief

Match the house format the `engineer` briefs set (For / Produces / Do-not-edit / Status header; a first-person workflow; jobs-as-outcomes; binding principles; an orientation section; a hard visual-language constraint; out-of-scope; where the model lives) — but bind the visual language to **this repo's kit** (`../README.md`, `../terminal-tokens.css`, `../books.html`), not the web design system. Keep it problem-first and non-prescriptive: name existing screens, widgets, and the pre-built API client as *reuse context*, never as the prescribed answer.
