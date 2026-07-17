# engineer-cli — terminal design references

Design references for the **terminal** client, organized by app area (mirroring `engineer/docs/designs/`).
Mockups are Claude Design **canvas docs** (`*.dc.html`, rendered by the bundled `support.js` runtime): each is a board of [ratatui](https://ratatui.rs)-faithful screens for one app area.
`design-system.dc.html` is the shared palette and component legend they all follow;
`briefs/shipped/timer-gaps.brief.md` is the timer gap analysis (mockups vs the timer engineer has shipped) that drove `timer.dc.html`'s design passes.

This directory is also the **reference kit** to seed a *new* Claude Design project for the terminal app. The terminal is a sibling surface to the web app — it shares the brand and the domain, but **not** the visual medium. A TUI is a character grid: one monospace font, no shadows/radii/web-fonts, an ANSI/256-colour palette, keyboard-driven, dark-first. So we design it in its own project, fed the *transferable* slice of the web design system (information architecture, colour semantics, brand) rather than its pixel/CSS chrome.

## How to use this kit in Claude Design

The **`engineer-cli`** Claude Design project is live (seeded from this kit's original `books.html` anchor + `terminal-tokens.css`, both since retired);
its outputs land here as `*.dc.html` canvas docs — `design-system.dc.html` (the style anchor) and one doc per app area (`timer.dc.html`, …).
To iterate on an area: give the project the area's current `.dc.html`, `design-system.dc.html`, this `README.md`, and the relevant brief or gap analysis from `briefs/`, and ask for the gaps to be closed within the palette mapping, chrome conventions, and translate/don't-translate rules below.

> Why a new project, not the web one: the web project carries ~50 files of web-CSS iteration. For a TUI, most of that is the wrong medium and biases output toward shadows/rounded/pixel idioms a grid can't render. Seed only what transfers; design in a clean, terminal-shaped frame.

## Palette mapping

Web tokens (`engineer/docs/designs/tokens.css`) mapped to terminal values, cross-checked against the shipped Rust palette (`../../src/ui/theme.rs`). The terminal **inverts** the web's light-on-white to light-on-dark, and **lightens** the accent for legibility.

| Role | Web token | Shipped `theme.rs` (256 / hex) | Recommended terminal (256 / hex) | Note |
|---|---|---|---|---|
| background | white `#FFFFFF` | terminal default | **`#05080F`** | inverted; dark-first |
| foreground | `neutral-900 #0F172A` | terminal default | **`#E6EBF2`** | inverted |
| muted fg | `neutral-600 #475569` | `244 / #808080` | `244 / #808080` | matches — keep |
| border | `neutral-200 #E2E8F0` | `240 / #585858` | `240 / #585858` | matches — keep |
| **accent** | `accent-600 #3B40CC` (indigo) | `75 / #5FAFFF` (**sky blue**) | **`105 / #8787FF`** (indigo-light) | shipped value drifts off the indigo *hue*; see below |
| selection bg | (web uses `accent-200`) | `67 / #5F87AF` (steel) | **`61 / #5F5FAF`** (indigo dim) | match accent hue |
| success | `#10B981` | `108 / #87AF87` | `108 / #87AF87` | matches — keep |
| warning | `#F59E0B` | `179 / #D7AF5F` | `179 / #D7AF5F` | matches — keep |
| danger | `#EF4444` | `167 / #D75F5F` | `167 / #D75F5F` | matches — keep |

**The accent decision (the one real divergence).** The web brand is indigo (`#3B40CC`, a blue-violet). `#3B40CC` is too dark to read on a dark terminal, so it must be lightened — but the shipped `theme.rs` lightens it all the way to a *sky blue* (`256 #75 = #5FAFFF`), which is a different **hue** (~210°/cyan vs indigo's ~237°/violet). It reads as a different brand colour than the web. The recommendation is to lighten *along the indigo hue* instead — `256 #105 = #8787FF` (periwinkle) keeps the brand identity while staying bright on dark. The mockups use the recommended value. Adopting it is a one-line change in `theme.rs` (`ACCENT`/`ACCENT_DIM`), deliberately **not** applied here — it's a design decision to ratify, not a silent code edit.

The semantic colours (success/warn/danger), border, and muted already track the web hues well — no change.

## Web → terminal screen inventory

What transfers from the web app is the **information architecture** (which screens, what data, what hierarchy), not the layout. Built screens are faithful today; "next" are the obvious growth seeded by the web designs.

| Terminal screen | Seeds from (web) | Status |
|---|---|---|
| Home / dashboard | dashboard + quick stats | built (`screens/home.rs`) |
| Books list | `books.html` | built (`screens/books.rs`) |
| Book detail + chapters | `books.html` / `roadmap.html` | built (`screens/book_detail.rs`) |
| Log activity (form) | `Activities.html` + `Forms v2.html` | built (`screens/activity_new.rs`) |
| Sign in | identity / auth | built (`screens/login.rs`) |
| Activities table | `Activities.html` | built (`screens/activities.rs`) |
| Timer + header cell | `navigation-bar.html` §M + `timer-hygiene.html` | built v1 (`screens/timer.rs`) — redesign specified in `timer.dc.html`, gaps in `briefs/shipped/timer-gaps.brief.md` |
| Notes capture + browser + `$EDITOR`/headless/faces | `notes.dc.html` (from `notes.html`) | built — capture (`app/capture.rs`), browser + delete/unlink faces (`screens/notes.rs`), `$EDITOR` (`editor.rs`), headless twin (`note_cli.rs`); epic #120 |
| Review (dashboard / browse / sitting) | `review.html` | built (`screens/review.rs`) |
| Progress (pace meters) | `progress.html` | built (`screens/progress.rs`) |
| Week planning + retro | `week-planning.dc.html` | built (`screens/week.rs`) |
| Inbox (draft triage) + Connect (sources) | `assisted-capture.dc.html` | built (`screens/inbox.rs`, `screens/connect.rs`) |
| Command palette (`:`) | `Command Palette v2.html` | built — `:` grammar (`src/app/command.rs`) |
| Roadmaps + book progress | `Roadmaps and Book Progress v2.html` | next |
| Shard / environment indicator | `Tenancy, Shard & Environment Indicators v3.html` | next — belongs in the header chrome |

## Translate / don't-translate

**Governing principle — sterling, not a replica.** A full terminal replica of the `engineer` web UI is a non-goal. The terminal is an *Apple Watch for the study loop*: it owns the high-frequency, high-value core, distilled into **glances (complications) and gestures (one-keystroke verbs)** — quiet, honest, and composable (every read pipes, every action is a headless verb). Depth — rich filtering, bulk edit, dashboards, planning canvases, settings forms — stays on the web. The shipped **timer** is the exemplar of the bar; the full **glance-or-gesture test**, and where each module sits against it, is the governing section of [`briefs/proposed/cross-cutting.brief.md`](briefs/proposed/cross-cutting.brief.md). Every brief is measured against it before it is designed.

- **Drop entirely** (no terminal equivalent): shadows, border-radius, Inter/web-fonts, gradients, pixel spacing, hover states, responsive breakpoints.
- **Replace with a terminal idiom:**
  - elevation / cards -> box-drawing panels (`bordered()`) + dim-vs-bright contrast
  - badges / pills -> inverse mono labels, black ink on a semantic fill (` reading `, ` done `)
  - type hierarchy -> weight (`BOLD`) + colour, **one font size**
  - selection / focus -> full-row inverse highlight + a `▌` marker
  - icons / SVG -> sparse unicode glyphs or ASCII; status dots `●`/`○`
  - progress -> the block-bar in `widgets::progress_bar` (`███▍·····  42%`)
- **Keep:** colour *semantics*, information density, the domain vocabulary.
- **Interaction:** keyboard-only, neovim-flavoured — `j`/`k`, `gg`/`G`, `/`, `n`/`N`, `:cmd`, `<Space>` leader, `i`/`Esc` for insert/normal in forms. No mouse. The footer always shows the active screen's keys.

## Chrome conventions (already shipped — match these)

Layout is three stacked rows (`../../src/ui/layout.rs::render_chrome`):

```
  engineer  >  <Screen Title>      <user> @ <identity_host>      <- header (1 row, accent app name)
  +-- <Panel Title> ------------------------------------+
  |  ... body: bordered panel(s), the screen's content  |        <- body (fills remaining rows)
  +-----------------------------------------------------+
  [ j/k ] move  ·  [ ↵ ] open  ·  ...                            <- footer: hints OR a notification tile
```

- **Header:** `engineer` (accent, bold) + ` › ` (muted) + screen title + user `@` identity host (muted).
- **Body panels:** `bordered(title)` — full box-drawing border in the border colour, title in ` accent bold ` at the top-left.
- **Footer:** either keybinding hints (`footer_hints` — each key a black-on-accent cap) or a level-styled notification tile (`notify.rs` — info/success as coloured text, warning/error filling the row). Tiles auto-expire.
- **Status pills** (`widgets::status_pill`), black ink on a semantic fill: ` reading ` (accent) · ` done ` (success) · ` unread ` (muted) · ` hold ` (warn) · ` stop ` (danger).
- **Selection:** whole row highlighted (`bg` accent-dim, black ink, bold) with a `▌ ` marker.

## Files

| File | What |
|---|---|
| `README.md` | This brief — the kit's entry point |
| `design-system.dc.html` | Palette & component legend — the canvas kit's style anchor |
| `timer.dc.html` | Timer screens — hero, status-line, start picker, idle reclaim, focus, audit, headless |
| `week-planning.dc.html` | Week board — plan ⇄ retro, the planned-vs-done readout, the `$EDITOR` reflection, headless twins |
| `assisted-capture.dc.html` | Draft-triage inbox (pending / draft / reject / zero) + the ambient count + the git-source connect flow (sources list, trust gate, requirement pointer) + headless twins |
| `support.js` | Claude Design canvas runtime the `.dc.html` docs load |
| `briefs/` | Problem-first design briefs and gap analyses (`proposed/` → `shipped/` lifecycle) |
