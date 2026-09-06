# Architecture decision records

This folder holds the terminal client's architecture decision records (ADRs) — the reasoning behind non-trivial, cross-cutting choices, so a future maintainer can answer *"why is it built this way?"* without excavating closed pull requests.

Each record is `NNNN-<slug>.md`, numbered in the order decided, and is **immutable once accepted**: supersede a decision with a new record rather than editing an old one in place. A record states its Context, the Decision, the Alternatives considered, and the Consequences. Diagrams are ASCII only (this is a terminal project).

## Index

| Record | Decision |
|---|---|
| [0001](0001-terminal-error-notification-model.md) | The terminal error, notification & search-state model — three tiers, and one spelling per outcome |
| [0002](0002-sterling-not-a-replica.md) | Sterling, not a replica: the glance-or-gesture test a surface must pass to live in the terminal |
| [0003](0003-tui-headless-contract.md) | The TUI ↔ headless contract: every read is also a one-shot, and what a verb owes a script |
| [0004](0004-derived-never-stored-and-the-write-queue.md) | Derived, never stored — and the offline write queue as its one scoped exception |
| [0005](0005-editor-for-prose.md) | `$EDITOR` for prose; the in-app `i`/`Esc` grammar for a line |
