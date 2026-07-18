# UI rendering & data-layer sync

This guide explains how the ratatui view stays in step with the backend data
layer: the event loop, the message/reducer model, and why rendering is
"pull-based".

## The Elm Architecture (TEA)

The app is a single-threaded state machine. The four TEA roles map onto concrete
types:

| TEA role | In this codebase |
|----------|------------------|
| **Model** | `App` (`src/app/mod.rs`) plus each screen's own state (e.g. `Books` in `src/app/screens/books.rs`) |
| **Message** | `Action` (`src/app/action.rs`) |
| **Update** | `App::handle` (top-level) which delegates screen-specific actions to `Screen::handle` |
| **View** | `App::render` → `ui::layout::render_chrome` + `Screen::render` |

There are no observers, data bindings, or change subscriptions. State is mutated
only inside the reducer, and the view is recomputed from scratch each frame.

## The event loop

`run_loop` (`src/app/mod.rs`) owns the terminal and drives everything with a
single `tokio::select!` over three sources:

1. the **Action channel** (`mpsc::UnboundedReceiver<Action>`) — results of async
   work and self-dispatched actions;
2. the **crossterm event stream** (`EventStream`) — keypresses, translated to an
   `Action` by `event::translate` (`src/app/event.rs`);
3. a **tick** (`interval(TICK)`, 250 ms) — used to expire notifications.

```rust
while !app.should_quit {
    terminal.draw(|f| app.render(f))?;   // VIEW: pull current state

    tokio::select! {
        biased;
        Some(action) = rx.recv() => app.handle(action).await,           // async results
        maybe_event = events.next() => { /* translate key → Action */ } // input
        _ = ticker.tick() => { /* expire notification */ }
    }
}
```

`biased` makes queued actions drain before new input is read, keeping state
consistent before the next draw.

## Pull-based rendering

ratatui is an immediate-mode renderer: there is no retained widget tree to keep
in sync. `App::render` builds a fresh `Chrome` from the *current* state every
frame —

```rust
let chrome = Chrome {
    user: self.user.as_deref(),                 // header reads live state
    notification: self.notification.as_ref(),   // footer tile or hints
    screen_title: self.current.title(),
    hints: self.current.hints(...),
    ...
};
let body = render_chrome(frame, frame.area(), chrome);
self.current.render(frame, body);               // active screen fills the body
```

Because the header and footer simply read `self.user` and `self.notification`
each frame, **any state change is reflected on the very next draw** — no manual
"refresh the header" call is ever needed. `render_chrome` (`src/ui/layout.rs`)
splits the area into a 1-line header, the body, and a 1-line footer (notification
tile when present, otherwise the screen's keybinding hints).

## Keeping the UI in sync with the data layer

The data layer is asynchronous, but the UI never blocks on it. A key insight:
**the reducer spawns I/O and the I/O reports back as more actions.** A loading
flag bridges the gap so the view can show "loading…" until results arrive.

Worked example — filtering the books list by pressing `2` (Reading):

```mermaid
sequenceDiagram
    participant U as User
    participant EV as event::translate
    participant RD as App/Books::handle
    participant TK as tokio task
    participant API as ApiClient
    participant ST as Books state
    participant V as render

    U->>EV: key '2'
    EV->>RD: Action::BooksFilter(Reading)
    RD->>ST: filter = Reading; loading = true
    RD->>TK: spawn Books::fetch
    Note over V: next frames show "loading…"
    TK->>API: list_books(Reading, q)
    API-->>TK: Ok(List<Book>) / Err
    TK->>RD: Action::BooksLoaded(books)  (via tx)
    RD->>ST: items = books; loading = false
    ST->>V: next frame shows the list
```

The same pattern drives the home screen (`HomeLoaded`), the current user (`FetchMe` → `SetUser`), and book detail (`BookDetailLoaded`).
Failures are not swallowed, and — this is the rule the design system's error model makes explicit (`docs/designs/design-system.dc.html` §ERROR & NOTIFICATION MODEL) — a read that *failed* is never re-encoded as an *empty* result.
The spawned task dispatches a typed `*LoadFailed(reason)` action; the screen records a `PanelFailure` and, when the region has no rows, renders the **Tier-2 inline panel state** (`ui::panel::render_panel_state`) inside its bordered block: a loud red reason line plus a retry key, visibly distinct from the calm muted *empty* state.
Books (`src/app/screens/books.rs`) is the reference adopter.

## Screen routing

`Screen` (`src/app/screens/mod.rs`) is an enum over the concrete screens, keyed
by `ScreenKind`. The shell forwards lifecycle and events to the active variant:

- `on_enter` — kicks off the screen's initial load;
- `intercept_key` — lets a screen consume keys *before* the global keymap (used
  for inline edits like the books search prompt and the page editor);
- `mode()` → `ScreenMode::{Normal, Insert}` — switches the keymap for form input;
- `handle` — the screen-local reducer; returns `Option<(Level, String)>` which
  the shell turns into a notification.

`event::translate` resolves keys in priority order: command mode (`:`), insert
mode, screen `intercept_key`, notification dismiss (`Esc`), leader (`Space`),
then the global/!screen keymap.

## Notifications

`ui::notify` (`src/ui/notify.rs`) is the typed, self-expiring notification subsystem — **Tier 1** of the design's three-tier error/notification model.
`Level` (`Info`/`Success`/`Warning`/`Error`) carries an icon, a style, and a TTL; `Notification` is rendered as a one-line footer tile by `render_notification`.
The reducer sets `App.notification` via `App::notify`; the tick loop drops it once `is_expired()`; `Esc` dismisses it early via `Action::DismissNotification`.
Tier 1 is for the transient outcome of a keystroke ("signed in", "progress saved").
The persistent forms are **Tier 2** — the inline panel state for a read that failed (`ui::panel`, scoped to one region) — and **Tier 3** — a whole-screen blocking state for auth down / re-auth.
The wording for a given outcome is spelled once in `crate::messages`, so the tile, the panel line, and the headless `stderr` a script greps all read the same words (design §C).
