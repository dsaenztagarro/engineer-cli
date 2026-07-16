//! TUI shell. Owns terminal state, the event loop, and screen routing.
//!
//! Architecture: a single `tokio::select!` loop drains crossterm events,
//! background HTTP results from a `mpsc` channel, and a tick timer. Screens
//! interpret keys into `Action`s; `App::handle` mutates state and may spawn
//! async work whose results come back as `Action`s.

use color_eyre::eyre::Result;
use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{stdout, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::api::{ApiClient, Timer};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::editor::EditorOutcome;
use crate::queue::{QueueStore, QueuedClient};
use crate::ui::notify::{Level, Notification};

mod action;
mod capture;
pub mod command;
mod event;
pub mod screens;

pub use action::Action;

use capture::QuickCapture;
use screens::{Screen, ScreenKind};

const TICK: Duration = Duration::from_millis(250);

/// How often the header timer cell re-polls the server. Between polls the
/// displayed elapsed is ticked locally from `elapsed_seconds` + a monotonic
/// baseline, so the cell advances smoothly without a request per second.
const TIMER_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// The most often a key in the TUI marks presence (the idle-guard heartbeat).
/// Matches the web pill's once-a-minute throttle — the server beat is
/// presence-only, so the rate limit is the client's job.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// A pending `$EDITOR` hand-off the run loop performs between frames: suspend the
/// TUI, spawn the editor seeded with `seed`, and route the saved buffer to
/// `target`.
pub struct PendingEditor {
    seed: String,
    target: EditorTarget,
}

/// Where a finished `$EDITOR` buffer lands, and how an abort/empty save is read.
pub enum EditorTarget {
    /// The quick-capture overlay's draft (#88) — a non-empty save updates it; an
    /// abort or an empty buffer keeps it (capture-is-sacred).
    Capture,
    /// The week's retro reflection (#117) — a save persists (empty clears); an
    /// abort keeps the stored note (abort ≠ empty-save).
    WeekNote { iso_week: String },
}

pub struct App {
    pub config: Config,
    pub api: ApiClient,
    pub user: Option<String>,
    pub current: Screen,
    pub notification: Option<Notification>,
    pub leader_pending: bool,
    /// `g`-goto prefix pending — the next key picks a destination (`g t`/`g p`/
    /// `g r`/…) or, on a `g`, the current list's top motion (`gg`).
    pub goto_pending: bool,
    pub command_buffer: Option<String>,
    /// The quick-capture overlay, when open — modal over the current screen and
    /// reachable from anywhere (`<Space>c`). `None` when closed.
    pub capture: Option<QuickCapture>,
    pub should_quit: bool,
    pub tx: mpsc::UnboundedSender<Action>,
    /// Latest timer snapshot shared by every screen's header cell (`None` until
    /// the first poll / when no timer is running).
    pub timer: Option<Timer>,
    /// Monotonic instant the current `timer` snapshot was received — the base
    /// for ticking the displayed elapsed between polls.
    pub timer_base: Option<Instant>,
    /// True while `timer` is the folded local clock (a transport-failed poll
    /// fell back to cache ⊕ queue) — the header cell wears the ` ~` staleness
    /// marker until a live poll lands again.
    pub timer_stale: bool,
    /// When the last header poll was dispatched, to honour `TIMER_POLL_INTERVAL`.
    pub timer_last_poll: Instant,
    /// The shared write queue, for the header's ` ↑N` unsynced-writes count.
    /// `None` when the state dir is unavailable (the count reads 0). Read-only
    /// here — writes go through `QueuedClient`.
    pub queue: Option<QueueStore>,
    /// Queued writes still in play (pending + diverged; parked intents are kept
    /// for review and excluded), refreshed each tick — the header cell's ` ↑N`
    /// complication (quiet accent, the shipped stale marker's family).
    pub queued_writes: usize,
    /// True while the queue holds a diverged intent — the header wears the one
    /// loud state in the vocabulary (a full ` diverged ` danger chip) until the
    /// reconcile panel or `engineer queue resolve` settles it.
    pub queue_diverged: bool,
    /// The per-user timer knobs, fetched once after sign-in — the header cell
    /// reads them to spot a finished focus phase (the offer pill).
    pub settings: Option<crate::api::TimerSettings>,
    /// Timer id already pinged for overrun — the ping fires once per timer,
    /// never again on later polls of the same clock.
    pub overrun_pinged: Option<i64>,
    /// When the last presence heartbeat was sent, to honour `HEARTBEAT_INTERVAL`.
    pub heartbeat_last: Instant,
    /// A pending `$EDITOR` hand-off — the run loop suspends the TUI, spawns the
    /// editor seeded with the current text, and routes the saved buffer to its
    /// target. Set by the capture overlay's `Ctrl-E` (the draft) or the week
    /// board's `i` (the retro reflection).
    pub pending_editor: Option<PendingEditor>,
    /// The verb words replayed so far in the current reconnect drain — the
    /// running one-line transcript (`back online · replaying the queue… start ·
    /// pause`). Grows on each `ReplayProgress` and is emptied when the drain
    /// finishes (`ReplayFinished`). Empty when no drain is in flight.
    pub reconnect_words: Vec<String>,
}

pub async fn run(config: Config) -> Result<()> {
    let provider = TokenProvider::new(config.clone()).await?;
    let api = ApiClient::new(config.api_url.clone(), provider);

    let mut terminal = init_terminal()?;
    let res = run_loop(config, api, &mut terminal).await;
    restore_terminal(&mut terminal).ok();
    res
}

async fn run_loop(
    config: Config,
    api: ApiClient,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();

    // Land on the Login screen when there is no stored refresh token; otherwise
    // boot straight into the authenticated UI.
    let logged_in = crate::auth::is_logged_in(&config);
    let start = if logged_in {
        ScreenKind::Home
    } else {
        ScreenKind::Login
    };

    let mut app = App {
        config,
        api: api.clone(),
        user: None,
        current: Screen::new(start),
        notification: None,
        leader_pending: false,
        goto_pending: false,
        command_buffer: None,
        capture: None,
        should_quit: false,
        tx: tx.clone(),
        timer: None,
        timer_base: None,
        timer_stale: false,
        timer_last_poll: Instant::now(),
        queue: QueueStore::open_default().ok(),
        queued_writes: 0,
        queue_diverged: false,
        settings: None,
        overrun_pinged: None,
        heartbeat_last: Instant::now(),
        pending_editor: None,
        reconnect_words: Vec::new(),
    };

    // Kick off initial loads (only meaningful once authenticated).
    if logged_in {
        app.dispatch(Action::FetchMe);
        app.dispatch(Action::RefreshTimer);
    }
    app.current.on_enter(&app.api, &app.tx);

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);

    while !app.should_quit {
        terminal.draw(|f| app.render(f))?;

        tokio::select! {
            biased;
            Some(action) = rx.recv() => {
                app.handle(action).await;
            }
            maybe_event = events.next() => {
                if let Some(Ok(ev)) = maybe_event {
                    // A key in the TUI is the CLI's honest "still working"
                    // signal — beat presence before the event is interpreted.
                    if matches!(ev, crossterm::event::Event::Key(_)) {
                        app.beat_presence_if_active();
                    }
                    if let Some(action) = event::translate(&mut app, ev) {
                        app.handle(action).await;
                    }
                }
            }
            _ = ticker.tick() => {
                app.expire_stale_notification();
                app.poll_timer_if_due();
                app.refresh_queued_writes();
            }
        }

        // A note asked for the full editor (Ctrl-E) — suspend the TUI, run
        // $EDITOR, restore. Blocking is fine: the terminal is ours meanwhile.
        if app.pending_editor.is_some() {
            if let Err(e) = run_editor(terminal, &mut app) {
                app.notify(Level::Error, format!("editor failed: {e}"));
            }
        }
    }

    Ok(())
}

/// Suspend the TUI, open the seed in `$EDITOR`, and route the saved buffer to
/// its target — the capture draft or the week reflection (the `git commit`
/// pattern). The alt screen is handed to the child and re-entered after.
fn run_editor(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let Some(pending) = app.pending_editor.take() else {
        return Ok(());
    };
    restore_terminal(terminal)?; // leave the alt screen + raw mode to the child
    let edited = crate::editor::edit(&pending.seed);
    resume_terminal(terminal)?;
    let outcome = edited?;
    match pending.target {
        // The capture draft: a non-empty save updates it; an abort or an empty
        // buffer keeps the original (capture-is-sacred, empty-buffer-cancels).
        EditorTarget::Capture => {
            if let EditorOutcome::Saved(text) = outcome {
                if !text.trim().is_empty() {
                    if let Some(cap) = app.capture.as_mut() {
                        cap.set_content(&text);
                    }
                }
            }
        }
        // The reflection: a save persists (an empty buffer clears the note
        // deliberately — abort ≠ empty-save); an abort keeps the stored note.
        // Both route back to the week screen's queue seam, which owns the write.
        EditorTarget::WeekNote { iso_week } => match outcome {
            EditorOutcome::Saved(body) => app.dispatch(Action::WeekReflectSave { iso_week, body }),
            EditorOutcome::Aborted => app.dispatch(Action::WeekReflectAbort),
        },
    }
    Ok(())
}

/// Re-enter the alt screen + raw mode after the child editor exits.
fn resume_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

fn format_minutes(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// The synced-tile count, pluralized: `1 queued write reconciled` vs. `N queued
/// writes reconciled`.
fn writes_reconciled(n: usize) -> String {
    if n == 1 {
        "1 queued write reconciled".to_string()
    } else {
        format!("{n} queued writes reconciled")
    }
}

/// The header poll's task body. Reconnect-drains the queue first — streaming
/// each landed intent's verb word into the ambient transcript, then the report
/// the `✓ synced` tile reads — then reads the live snapshot (warming the read
/// cache) or folds the cached clock on a transport failure. This is what makes
/// the TUI reconcile on its own, not only on the next write.
///
/// Extracted from the `RefreshTimer` handler so the wiremock tests can drive the
/// drain → transcript path with a scratch queue + cache. `queued` is the write
/// seam (`None` when the state dir is unavailable); `cache_path` warms a scratch
/// cache in tests (`None` → the shared XDG cache).
async fn run_timer_poll(
    api: ApiClient,
    tx: mpsc::UnboundedSender<Action>,
    queued: Option<QueuedClient>,
    cache_path: Option<PathBuf>,
) {
    // Reconnect drain: replay any pending intents before the read so the poll
    // reflects what just synced. `drain_reporting` streams a `ReplayProgress`
    // per acknowledged intent (the transcript) and returns the report; it skips
    // instantly — streaming nothing — on an empty queue or a held replay lock,
    // so a false transcript can never appear.
    if let Some(q) = &queued {
        let tx2 = tx.clone();
        let report = q
            .drain_reporting(|intent| {
                let _ = tx2.send(Action::ReplayProgress {
                    word: intent.kind.word().to_string(),
                });
            })
            .await;
        if let Some(report) = report {
            let _ = tx.send(Action::ReplayFinished(report));
        }
    }

    match api.timer().await {
        Ok(t) => {
            // Warm the read cache with server truth (the headless read caches
            // the same way at `timer_cli::fetch_timer`). Without a snapshot a
            // TUI-only session would have nothing to synthesize an offline
            // pause/resume/stop/bind/discard from, and would refuse the keystroke.
            match &cache_path {
                Some(path) => crate::timer_cache::store_at(path, &t),
                None => crate::timer_cache::store(&t),
            }
            let _ = tx.send(Action::TimerLoaded(Box::new(t)));
        }
        // Offline (the same seam the headless read falls back on): render the
        // effective local timer — cached snapshot ⊕ pending queue — instead of
        // letting the header quietly extrapolate a clock the queue may have
        // paused or stopped.
        Err(crate::api::ApiError::Transport(e)) => {
            tracing::warn!(target: "engineer_cli::api", error = %e, "timer poll failed; folding cache + queue");
            if let Some(q) = &queued {
                if let Some((t, _)) = q.effective_timer(jiff::Timestamp::now()) {
                    let _ = tx.send(Action::TimerStale(Box::new(t)));
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "engineer_cli::api", error = %e, "timer poll failed");
        }
    }
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

impl App {
    pub fn dispatch(&self, action: Action) {
        let _ = self.tx.send(action);
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        self.notification = Some(Notification::new(level, text));
    }

    pub async fn handle(&mut self, action: Action) {
        // Top-level actions handled here; everything else delegates to the screen.
        match action {
            Action::Quit => self.should_quit = true,
            Action::Notify { level, text } => self.notify(level, text),
            Action::DismissNotification => self.notification = None,
            Action::FetchMe => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match api.me().await {
                        Ok(me) => {
                            let _ = tx.send(Action::SetUser(me.email));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::Notify {
                                level: Level::Error,
                                text: format!("login required: {e}"),
                            });
                        }
                    }
                });
            }
            Action::SetUser(email) => {
                self.user = Some(email);
                // The knobs are needed screen-agnostically (the header cell's
                // offer pill); fetch once per session.
                if self.settings.is_none() {
                    let api = self.api.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        if let Ok(s) = api.timer_settings().await {
                            let _ = tx.send(Action::SettingsLoaded(Box::new(s)));
                        }
                    });
                }
            }
            // Header timer cell. Plain polling: `GET /api/v1/timer` returns the
            // full snapshot on each request — the endpoint does not offer
            // conditional revalidation (If-None-Match / 304), so every poll
            // transfers the whole body.
            Action::RefreshTimer => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let queued = QueuedClient::new(&api).ok();
                    run_timer_poll(api, tx, queued, None).await;
                });
            }
            Action::TimerLoaded(t) => {
                // The overrun ping: once per timer, when a read first crosses
                // the plan (the server gates `over` on the user's knob).
                if t.over && t.id.is_some() && self.overrun_pinged != t.id {
                    self.overrun_pinged = t.id;
                    let planned = t.planned_minutes.unwrap_or(0);
                    self.notify(
                        Level::Warning,
                        format!(
                            "past the plan — planned {}, all-in over it now · s wraps up & saves",
                            format_minutes(planned)
                        ),
                    );
                }
                self.timer = Some((*t).clone());
                self.timer_base = Some(Instant::now());
                self.timer_stale = false;
                // Forward to the Timer screen so its detailed view mirrors the
                // same snapshot; other screens ignore it.
                let _ = self
                    .current
                    .handle(Action::TimerLoaded(t), &self.api, &self.tx)
                    .await;
            }
            // The offline twin of `TimerLoaded`: the folded local timer, worn
            // with the stale marker. Header-only by design — the screens keep
            // their last live snapshot (their own clocks already advance via
            // `live_elapsed`'s arithmetic).
            Action::TimerStale(t) => {
                self.timer = Some(*t);
                self.timer_base = Some(Instant::now());
                self.timer_stale = true;
            }
            // A queued write's provisional clock. Updates the header snapshot
            // (so the cell advances) and is forwarded to the Timer screen, which
            // flips its `◔` marker on. Not stale — it is the freshest local
            // truth; the ` ↑N` count (from the queue) says it is unsynced.
            Action::TimerProvisional(t) => {
                self.timer = Some((*t).clone());
                self.timer_base = Some(Instant::now());
                self.timer_stale = false;
                let _ = self
                    .current
                    .handle(Action::TimerProvisional(t), &self.api, &self.tx)
                    .await;
            }
            // The reconnect drain streamed a landed intent's verb word: append
            // it to the running one-line transcript and render it in the shipped
            // notify surface (`back online · replaying the queue… start · pause`)
            // — quiet and ambient, never a modal takeover.
            Action::ReplayProgress { word } => {
                self.reconnect_words.push(word);
                self.notify(
                    Level::Info,
                    format!(
                        "back online · replaying the queue… {}",
                        self.reconnect_words.join(" · ")
                    ),
                );
            }
            // The drain finished. A clean pass that reconciled ≥1 write lands one
            // calm `✓ synced` tile (auto-dismissing on the notify TTL, ~4s). An
            // empty pass shows nothing; a pass halted by divergence retires the
            // transcript and lets the existing diverged markers stand — the loud
            // reconcile panel is #106, not this ticket.
            Action::ReplayFinished(report) => {
                let showed_transcript = !std::mem::take(&mut self.reconnect_words).is_empty();
                if report.replayed >= 1 && !report.diverged {
                    self.notify(
                        Level::Success,
                        format!("synced — {}", writes_reconciled(report.replayed)),
                    );
                } else if showed_transcript {
                    self.notification = None;
                }
            }
            // Store-and-forward like TimerLoaded: the app keeps the knobs for
            // the header cell, the current screen gets its own copy.
            Action::SettingsLoaded(s) => {
                self.settings = Some((*s).clone());
                let _ = self
                    .current
                    .handle(Action::SettingsLoaded(s), &self.api, &self.tx)
                    .await;
            }
            // Wipe the header cell without touching the current screen (used
            // after a stop, so the segment confirmation view is preserved).
            Action::TimerCleared => {
                self.timer = None;
                self.timer_base = None;
                self.timer_stale = false;
            }
            Action::Login => {
                if let Screen::Login(s) = &mut self.current {
                    s.set_pending();
                }
                let cfg = self.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let discovery = crate::auth::discover(&cfg).await?;
                        let issued = crate::auth::login(&cfg, &discovery, false).await?;
                        if let Some(refresh) = &issued.refresh {
                            crate::auth::store_refresh(&cfg, refresh)?;
                        }
                        Ok::<(), color_eyre::eyre::Report>(())
                    }
                    .await;
                    let _ = match result {
                        Ok(()) => tx.send(Action::LoginSucceeded),
                        Err(e) => tx.send(Action::LoginFailed(e.to_string())),
                    };
                });
            }
            Action::LoginSucceeded => {
                self.notify(Level::Success, "signed in");
                self.dispatch(Action::Goto(ScreenKind::Home));
                self.dispatch(Action::FetchMe);
            }
            Action::LoginFailed(e) => {
                if let Screen::Login(s) = &mut self.current {
                    s.set_idle();
                }
                self.notify(Level::Error, format!("login failed: {e}"));
            }
            Action::Goto(kind) => {
                self.current = Screen::new(kind);
                self.current.on_enter(&self.api, &self.tx);
            }
            // Quick-capture overlay lifecycle. Open/close/saved are app-owned
            // (they create or drop `self.capture`); the live-edit actions below
            // route to the overlay's own reducer.
            Action::CaptureOpen => self.capture = Some(QuickCapture::new()),
            Action::CaptureOpenText(text) => self.capture = Some(QuickCapture::with_text(&text)),
            Action::CaptureOpenEdit(note) => self.capture = Some(QuickCapture::for_edit(*note)),
            Action::CaptureClose => self.capture = None,
            Action::CaptureSaved => {
                self.capture = None;
                self.notify(Level::Success, "note saved");
                // If the browser is showing, reflect the new/edited note.
                if self.current.kind() == ScreenKind::Notes {
                    self.dispatch(Action::RefreshNotes);
                }
            }
            // Stash the body for the run loop to open in $EDITOR (it owns the
            // terminal, so the suspend/spawn/restore happens there, not here).
            Action::CaptureEditExternal => {
                if let Some(cap) = &self.capture {
                    self.pending_editor = Some(PendingEditor {
                        seed: cap.body(),
                        target: EditorTarget::Capture,
                    });
                }
            }
            // The week board's `i` reflect: stash the current note body for the
            // run loop to open in $EDITOR, tagged to persist back to the week's
            // note. Same terminal-owned suspend/spawn as the capture path — only
            // the completion target differs.
            Action::WeekReflectEdit { iso_week, seed } => {
                self.pending_editor = Some(PendingEditor {
                    seed,
                    target: EditorTarget::WeekNote { iso_week },
                });
            }
            capture_action @ (Action::CaptureKey(_)
            | Action::CaptureSave
            | Action::CaptureSaveFailed
            | Action::CaptureCancel
            | Action::CaptureFieldNext
            | Action::CaptureFieldPrev
            | Action::CaptureBookInput(_)
            | Action::CaptureBookBackspace
            | Action::CaptureBookMove(_)
            | Action::CaptureBookPickerSubmit
            | Action::CaptureBookPickerClose
            | Action::CaptureBookResults(_)) => {
                if let Some(cap) = self.capture.as_mut() {
                    if let Some((level, text)) =
                        cap.handle(capture_action, &self.api, &self.tx).await
                    {
                        self.notify(level, text);
                    }
                }
            }
            Action::CommandBegin => { /* buffer already initialised by event layer */ }
            Action::CommandInput | Action::CommandBackspace => { /* buffer mutated in event layer */
            }
            Action::CommandCancel => {
                self.command_buffer = None;
            }
            Action::CommandSubmit => {
                let buf = self.command_buffer.take().unwrap_or_default();
                self.run_command(&buf);
            }
            other => {
                let next = self.current.handle(other, &self.api, &self.tx).await;
                if let Some((level, text)) = next {
                    self.notify(level, text);
                }
            }
        }
    }

    /// Parse a submitted `:` line against the grammar table and act on it. The
    /// table is the single source of truth: this dispatches, and the completion
    /// / inline hints (`command::complete`, `command::render_line`) read the same
    /// `ENTRIES`, so what runs and what the UI advertises can't drift.
    fn run_command(&mut self, buf: &str) {
        use command::Parse;
        match command::parse(buf) {
            Parse::Empty => {}
            Parse::Run(cmd) => self.execute_command(cmd),
            Parse::Unknown(verb) => {
                self.notify(Level::Warning, format!("unknown :{verb} — try :help"));
            }
            Parse::Ambiguous(matches) => {
                self.notify(
                    Level::Warning,
                    format!("ambiguous — {}", matches.join(" · ")),
                );
            }
            Parse::BadArg {
                verb,
                expected,
                got,
            } => {
                self.notify(
                    Level::Warning,
                    format!(":{verb} {got}? — try {}", expected.join("|")),
                );
            }
            Parse::AmbiguousArg { verb, matches } => {
                self.notify(Level::Warning, format!(":{verb} {}?", matches.join(" or ")));
            }
        }
    }

    fn execute_command(&mut self, cmd: command::Command) {
        use command::Command;
        match cmd {
            Command::Nav(kind) => self.dispatch(Action::Goto(kind)),
            // Timer actions run against the app-owned snapshot from any screen;
            // the header cell shows the result, and an invalid transition surfaces
            // the same warning the Timer screen would.
            Command::Timer(verb) => {
                if let Some((level, text)) = screens::timer::palette_dispatch(
                    verb,
                    self.timer.as_ref(),
                    &self.api,
                    &self.tx,
                    None,
                ) {
                    self.notify(level, text);
                }
            }
            Command::Note(None) => self.dispatch(Action::CaptureOpen),
            Command::Note(Some(text)) => self.dispatch(Action::CaptureOpenText(text)),
            Command::Quit => self.should_quit = true,
            Command::Write => self.dispatch(Action::ActivitySubmit),
            Command::Logs => match Config::log_dir() {
                Ok(dir) => self.notify(Level::Info, format!("logs: {}", dir.display())),
                Err(e) => self.notify(Level::Error, format!("log dir error: {e}")),
            },
            Command::Logout => self.notify(Level::Info, "run `engineer logout` from the shell"),
            Command::Help => self.notify(Level::Info, command::help_summary()),
        }
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame) {
        use crate::ui::layout::{render_chrome, Chrome};

        let host = self.config.identity_url.host_str().unwrap_or("identity");
        // The open overlay owns the footer hints so its keymap is legible.
        let hints = match self.capture.as_ref() {
            Some(cap) => cap.hints(),
            None => self.current.hints(
                self.leader_pending,
                self.goto_pending,
                self.command_buffer.as_deref(),
            ),
        };
        let chrome = Chrome {
            user: self.user.as_deref(),
            identity_host: host,
            screen_title: self.current.title(),
            // Narrow rail: below ~70 columns the cell drops its label down to
            // glyph + clock so the breadcrumb keeps its room.
            timer: self.timer_cell_spans(frame.area().width < 70),
            notification: self.notification.as_ref(),
            hints,
        };
        let body = render_chrome(frame, frame.area(), chrome);
        self.current.render(frame, body);
        // The quick-capture overlay renders last, as a modal over the body.
        if let Some(cap) = self.capture.as_mut() {
            cap.render(frame, body);
        }
    }

    /// A key in the TUI marks presence: while a timer is running, unpaused,
    /// and not already idle, POST a heartbeat so the idle guard reflects real
    /// in-TUI work. Throttled to `HEARTBEAT_INTERVAL` (the server beat is
    /// presence-only). Deliberately silent once the timer has gone idle — the
    /// reclaim screen owns that decision (its `keep` verb is the explicit "I
    /// was present"), so navigating the reclaim list never auto-resolves it.
    fn beat_presence_if_active(&mut self) {
        let active = self.user.is_some()
            && self
                .timer
                .as_ref()
                .is_some_and(|t| t.running && !t.paused && t.idle != Some(true));
        if !active || self.heartbeat_last.elapsed() < HEARTBEAT_INTERVAL {
            return;
        }
        self.heartbeat_last = Instant::now();
        let api = self.api.clone();
        tokio::spawn(async move {
            if let Err(e) = api.heartbeat_timer().await {
                tracing::debug!(target: "engineer_cli::api", error = %e, "heartbeat failed");
            }
        });
    }

    /// Drop the active notification once it outlives its level's TTL — run each
    /// tick. The `✓ synced` reconnect tile rides this to auto-dismiss (~4s,
    /// `Level::Success`'s TTL), the same self-expiry every notification uses.
    fn expire_stale_notification(&mut self) {
        if self
            .notification
            .as_ref()
            .is_some_and(Notification::is_expired)
        {
            self.notification = None;
        }
    }

    /// Re-poll the header timer snapshot when a poll is due and we're signed in.
    fn poll_timer_if_due(&mut self) {
        if self.user.is_some() && self.timer_last_poll.elapsed() >= TIMER_POLL_INTERVAL {
            self.timer_last_poll = Instant::now();
            self.dispatch(Action::RefreshTimer);
        }
    }

    /// Refresh the unsynced-writes count and the diverged flag from the shared
    /// queue for the header's ` ↑N` and ` diverged ` chip. Read on the tick
    /// (≤4×/s of a tiny, lock-free file), so it also reflects a drain, a
    /// divergence, or another process enqueuing or resolving — the queue file
    /// is the single source of truth, exactly as `engineer timer` reads it.
    fn refresh_queued_writes(&mut self) {
        let summary = self.queue.as_ref().and_then(|q| q.summary().ok());
        self.queued_writes = summary.map_or(0, |s| s.in_play());
        self.queue_diverged = summary.is_some_and(|s| s.diverged > 0);
    }

    /// The header timer cell spans, with the displayed elapsed ticked locally
    /// from the last snapshot, plus the ` diverged ` chip — the one loud state,
    /// worn even when no clock is running. `None` when there is nothing to show.
    fn timer_cell_spans(&self, narrow: bool) -> Option<Vec<ratatui::text::Span<'static>>> {
        let mut spans = self.timer_clock_spans(narrow).unwrap_or_default();
        if self.queue_diverged {
            if !spans.is_empty() {
                spans.push(ratatui::text::Span::raw(" "));
            }
            // Full-row danger treatment, the notify Error tile's idiom: the
            // queue holds a choice nothing will make for you.
            spans.push(ratatui::text::Span::styled(
                " diverged ",
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(crate::ui::theme::DANGER)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if spans.is_empty() {
            None
        } else {
            Some(spans)
        }
    }

    /// The clock half of the header cell. `None` when no timer is running.
    fn timer_clock_spans(&self, narrow: bool) -> Option<Vec<ratatui::text::Span<'static>>> {
        let t = self.timer.as_ref()?;
        let elapsed = screens::timer::live_elapsed(t, self.timer_base);
        // A finished focus phase shows as the offer pill on every screen.
        let offer = self
            .settings
            .as_ref()
            .and_then(|s| screens::timer::offer_for(t, s, jiff::Timestamp::now()))
            .is_some();
        let mut spans = crate::ui::widgets::timer_cell(t, elapsed, narrow, offer)?;
        // The shipped staleness idiom (the `--short` string's ` ~`): the cell
        // is showing the folded local clock, not a live server read.
        if self.timer_stale {
            spans.push(ratatui::text::Span::styled(" ~", crate::ui::theme::muted()));
        }
        // ` ↑N` — unsynced local writes, the quiet queued complication (accent,
        // the same idiom `engineer timer --short` prints).
        if self.queued_writes > 0 {
            spans.push(ratatui::text::Span::styled(
                format!(" ↑{}", self.queued_writes),
                ratatui::style::Style::default().fg(crate::ui::theme::ACCENT),
            ));
        }
        Some(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app(user: Option<String>) -> (App, mpsc::UnboundedReceiver<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App {
            config,
            api,
            user,
            current: Screen::new(ScreenKind::Home),
            notification: None,
            leader_pending: false,
            goto_pending: false,
            command_buffer: None,
            capture: None,
            should_quit: false,
            tx,
            timer: None,
            timer_base: None,
            timer_stale: false,
            timer_last_poll: Instant::now(),
            // Tests never touch the shared queue — the header count stays 0.
            queue: None,
            queued_writes: 0,
            queue_diverged: false,
            settings: None,
            overrun_pinged: None,
            heartbeat_last: Instant::now(),
            pending_editor: None,
            reconnect_words: Vec::new(),
        };
        (app, rx)
    }

    fn running_timer(elapsed_seconds: i64) -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "paused": false,
            "label": "consensus", "elapsed_seconds": elapsed_seconds,
        }))
        .unwrap()
    }

    fn rendered_text(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[tokio::test]
    async fn header_shows_signed_in_user() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        assert!(rendered_text(&mut app).contains("alice@example.com"));
    }

    #[tokio::test]
    async fn header_shows_not_signed_in_when_anonymous() {
        let (mut app, _rx) = test_app(None);
        assert!(rendered_text(&mut app).contains("not signed in"));
    }

    #[tokio::test]
    async fn set_user_updates_state() {
        let (mut app, _rx) = test_app(None);
        app.handle(Action::SetUser("bob@example.com".into())).await;
        assert_eq!(app.user.as_deref(), Some("bob@example.com"));
    }

    #[tokio::test]
    async fn week_reflect_edit_stashes_the_seed_and_target_for_the_run_loop() {
        // The week board's `i` hands the note seed to the app; the run loop reads
        // `pending_editor` between frames to suspend the TUI and open $EDITOR.
        let (mut app, _rx) = test_app(Some("a@b.c".into()));
        app.handle(Action::WeekReflectEdit {
            iso_week: "2026-W29".into(),
            seed: "the current note".into(),
        })
        .await;
        let pending = app.pending_editor.expect("the hand-off is stashed");
        assert_eq!(pending.seed, "the current note");
        assert!(matches!(
            pending.target,
            EditorTarget::WeekNote { iso_week } if iso_week == "2026-W29"
        ));
    }

    fn idle_snapshot() -> Timer {
        serde_json::from_value(serde_json::json!({
            "running": true, "bound": true, "paused": false, "idle": true,
            "elapsed_seconds": 9660,
        }))
        .unwrap()
    }

    /// A beat resets `heartbeat_last` to ~now; skipping it leaves the old
    /// instant, so `elapsed()` distinguishes the two.
    fn beat_fired(app: &App) -> bool {
        app.heartbeat_last.elapsed() < HEARTBEAT_INTERVAL
    }

    #[tokio::test]
    async fn presence_beats_for_a_running_timer_past_the_throttle() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(300));
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(beat_fired(&app), "a key past the throttle marks presence");
    }

    #[tokio::test]
    async fn presence_is_throttled_within_the_window() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(300));
        let recent = Instant::now() - Duration::from_secs(5);
        app.heartbeat_last = recent;
        app.beat_presence_if_active();
        assert_eq!(app.heartbeat_last, recent, "within the window, no beat");
    }

    #[tokio::test]
    async fn presence_never_beats_once_idle_so_reclaim_owns_the_decision() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(idle_snapshot());
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(
            !beat_fired(&app),
            "an idle timer's reclaim decision is not auto-resolved by a keypress"
        );
    }

    #[tokio::test]
    async fn presence_never_beats_without_a_running_timer_or_a_user() {
        // No timer.
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(!beat_fired(&app), "nothing running → no presence beat");

        // Running timer but signed out.
        let (mut app, _rx) = test_app(None);
        app.timer = Some(running_timer(300));
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(!beat_fired(&app), "signed out → no presence beat");
    }

    #[tokio::test]
    async fn header_shows_running_timer_pill() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let text = rendered_text(&mut app);
        // ● + mm:ss + the muted title (the v2 status-line grammar shows the
        // label at full width — superseding the v1 "never a title" pill).
        assert!(text.contains("● 04:32"), "{text}");
        assert!(text.contains("consensus"), "{text}");
    }

    #[tokio::test]
    async fn header_has_no_pill_without_a_timer() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        assert!(!rendered_text(&mut app).contains('●'));
    }

    #[tokio::test]
    async fn timer_loaded_updates_shared_snapshot() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::TimerLoaded(Box::new(running_timer(60))))
            .await;
        assert!(app.timer.as_ref().is_some_and(|t| t.running));
        assert!(app.timer_base.is_some());
    }

    #[tokio::test]
    async fn timer_cleared_wipes_the_header_snapshot() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(60));
        app.timer_base = Some(Instant::now());
        app.handle(Action::TimerCleared).await;
        assert!(app.timer.is_none());
    }

    #[tokio::test]
    async fn a_stale_fold_wears_the_marker_until_a_live_poll_lands() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));

        // The folded local clock renders, wearing the shipped ` ~` idiom.
        app.handle(Action::TimerStale(Box::new(running_timer(272))))
            .await;
        assert!(app.timer_stale);
        let text = rendered_text(&mut app);
        assert!(text.contains("● 04:32 consensus ~"), "{text}");

        // A live poll clears it — the header speaks server truth again.
        app.handle(Action::TimerLoaded(Box::new(running_timer(272))))
            .await;
        assert!(!app.timer_stale);
        let text = rendered_text(&mut app);
        assert!(text.contains("● 04:32"), "{text}");
        assert!(!text.contains("04:32 consensus ~"), "{text}");
    }

    #[tokio::test]
    async fn header_shows_the_queued_up_count_when_writes_are_pending() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        // Two unsynced offline writes — the quiet ` ↑N` complication.
        app.queued_writes = 2;
        let text = rendered_text(&mut app);
        assert!(text.contains("↑2"), "{text}");
        // It clears once the queue drains.
        app.queued_writes = 0;
        assert!(!rendered_text(&mut app).contains('↑'));
    }

    #[tokio::test]
    async fn header_wears_the_diverged_chip_even_without_a_clock() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        // No timer at all: the loud chip still shows — the choice outranks
        // every other header state.
        app.queue_diverged = true;
        let text = rendered_text(&mut app);
        assert!(text.contains("diverged"), "{text}");

        // With a clock, the chip rides next to the cell.
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let text = rendered_text(&mut app);
        assert!(text.contains("● 04:32"), "{text}");
        assert!(text.contains("diverged"), "{text}");

        // Resolved: the chip clears with the flag.
        app.queue_diverged = false;
        assert!(!rendered_text(&mut app).contains("diverged"));
    }

    #[tokio::test]
    async fn timer_provisional_updates_the_header_snapshot_unstale() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::TimerProvisional(Box::new(running_timer(60))))
            .await;
        assert!(app.timer.as_ref().is_some_and(|t| t.running));
        assert!(!app.timer_stale, "a fresh local write is not a stale read");
    }

    // --- Reconnect UX: the replay transcript + the synced tile (#105) ---

    fn replay_report(
        replayed: usize,
        remaining: usize,
        diverged: bool,
    ) -> crate::queue::ReplayReport {
        crate::queue::ReplayReport {
            replayed,
            deduped: 0,
            remaining,
            diverged,
        }
    }

    #[tokio::test]
    async fn reconnect_transcript_streams_then_lands_the_synced_tile() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));

        // Each landed intent appends its verb word to the one-line transcript.
        app.handle(Action::ReplayProgress {
            word: "start".into(),
        })
        .await;
        let n = app.notification.as_ref().expect("the transcript shows");
        assert_eq!(
            n.level,
            Level::Info,
            "the transcript is quiet, not a success"
        );
        assert!(n.text.contains("back online"), "{}", n.text);
        assert!(n.text.contains("start"), "{}", n.text);

        app.handle(Action::ReplayProgress {
            word: "pause".into(),
        })
        .await;
        let n = app.notification.as_ref().unwrap();
        assert!(
            n.text.contains("start") && n.text.contains("pause"),
            "each word accumulates: {}",
            n.text
        );

        // A clean drain lands one calm success tile and empties the transcript.
        app.handle(Action::ReplayFinished(replay_report(2, 0, false)))
            .await;
        let n = app.notification.as_ref().expect("the synced tile lands");
        assert_eq!(n.level, Level::Success);
        assert_eq!(n.text, "synced — 2 queued writes reconciled");
        assert!(
            app.reconnect_words.is_empty(),
            "transcript reset for next drain"
        );
    }

    #[tokio::test]
    async fn synced_tile_uses_singular_copy_for_one_write() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::ReplayFinished(replay_report(1, 0, false)))
            .await;
        assert_eq!(
            app.notification.as_ref().unwrap().text,
            "synced — 1 queued write reconciled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn synced_tile_auto_dismisses_after_its_ttl() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::ReplayFinished(replay_report(2, 0, false)))
            .await;
        assert_eq!(app.notification.as_ref().unwrap().level, Level::Success);

        // Still fresh — the tick's expiry leaves it be.
        app.expire_stale_notification();
        assert!(app.notification.is_some(), "not yet past its TTL");

        // Past the Success TTL, the same self-expiry every notification uses
        // clears it — the glance, not a report.
        tokio::time::advance(Level::Success.ttl() + Duration::from_secs(1)).await;
        app.expire_stale_notification();
        assert!(app.notification.is_none(), "auto-dismissed after ~4s");
    }

    #[tokio::test]
    async fn a_diverged_drain_shows_no_synced_tile() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        // A transcript was showing when the server diverged mid-drain.
        app.handle(Action::ReplayProgress {
            word: "start".into(),
        })
        .await;
        assert!(app.notification.is_some());

        // Divergence retires the transcript and shows no "synced" — the existing
        // diverged markers stand (the reconcile panel is #106).
        app.handle(Action::ReplayFinished(replay_report(1, 1, true)))
            .await;
        assert!(app.notification.is_none(), "no synced tile on divergence");
        assert!(app.reconnect_words.is_empty());
    }

    #[tokio::test]
    async fn an_empty_drain_shows_nothing() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::ReplayFinished(replay_report(0, 0, false)))
            .await;
        assert!(
            app.notification.is_none(),
            "a drain that replayed nothing is silent"
        );
    }

    #[tokio::test]
    async fn poll_with_a_queued_write_drains_and_streams_the_transcript() {
        use crate::queue::{IntentKind, QueueStore, QueuedClient};
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The queued pause replays on reconnect...
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        // ...then the live read lands.
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "elapsed_seconds": 300
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!("engineer-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = QueueStore::at(dir.join("queue.json"));
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:30:00Z".parse().unwrap(),
            })
            .unwrap();

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let cache = dir.join("timer-cache.json");
        let queued = QueuedClient::with_paths(&api, store, cache.clone());

        let (tx, mut rx) = mpsc::unbounded_channel();
        super::run_timer_poll(api, tx, Some(queued), Some(cache)).await;

        let actions = drain(&mut rx);
        // The transcript: one ReplayProgress per replayed intent, its verb word.
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ReplayProgress { word } if word == "pause")),
            "{actions:?}"
        );
        // The report the synced tile reads.
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ReplayFinished(r) if r.replayed == 1 && !r.diverged)),
            "{actions:?}"
        );
        // The read landed after the drain.
        assert!(actions.iter().any(|a| matches!(a, Action::TimerLoaded(_))));
    }

    #[tokio::test]
    async fn login_succeeded_enqueues_goto_home_and_fetch_me() {
        let (mut app, mut rx) = test_app(None);
        app.handle(Action::LoginSucceeded).await;

        let mut actions = Vec::new();
        while let Ok(a) = rx.try_recv() {
            actions.push(a);
        }
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Goto(ScreenKind::Home))));
        assert!(actions.iter().any(|a| matches!(a, Action::FetchMe)));
    }

    #[tokio::test]
    async fn books_load_failure_notifies_error() {
        let (mut app, _rx) = test_app(None);
        app.handle(Action::Notify {
            level: Level::Error,
            text: "books load failed".into(),
        })
        .await;
        let n = app.notification.expect("notification set");
        assert_eq!(n.level, Level::Error);
        assert_eq!(n.text, "books load failed");
    }

    #[tokio::test]
    async fn goto_notes_titles_the_screen() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::Goto(ScreenKind::Notes)).await;
        assert_eq!(app.current.kind(), ScreenKind::Notes);
        assert!(rendered_text(&mut app).contains("Notes"));
    }

    #[tokio::test]
    async fn capture_overlay_opens_and_renders_over_any_screen() {
        // Home is showing; opening capture must draw the modal on top of it.
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::CaptureOpen).await;
        assert!(app.capture.is_some());
        let text = rendered_text(&mut app);
        assert!(text.contains("Quick capture"), "{text}");
    }

    #[tokio::test]
    async fn capture_saved_closes_the_overlay_and_confirms() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::CaptureOpen).await;
        app.handle(Action::CaptureSaved).await;
        assert!(app.capture.is_none());
        let n = app.notification.expect("a confirmation is shown");
        assert_eq!(n.level, Level::Success);
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<Action>) -> Vec<Action> {
        let mut out = Vec::new();
        while let Ok(a) = rx.try_recv() {
            out.push(a);
        }
        out
    }

    async fn submit_command(app: &mut App, buf: &str) {
        app.command_buffer = Some(buf.to_string());
        app.handle(Action::CommandSubmit).await;
    }

    #[tokio::test]
    async fn command_nav_verb_dispatches_goto() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "books").await;
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::Goto(ScreenKind::Books))));
        assert!(app.command_buffer.is_none(), "buffer is consumed on submit");
    }

    #[tokio::test]
    async fn command_week_verb_lands_on_the_week_board() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        // `:week` routes through Nav → Goto and the screen becomes current.
        submit_command(&mut app, "week").await;
        // Apply the enqueued Goto so the screen actually switches.
        app.handle(Action::Goto(ScreenKind::Week)).await;
        assert_eq!(app.current.kind(), ScreenKind::Week);
        assert!(rendered_text(&mut app).contains("Week"));
    }

    #[tokio::test]
    async fn command_prefix_resolves_to_activities() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "act").await;
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::Goto(ScreenKind::Activities))));
    }

    #[tokio::test]
    async fn command_note_prefills_the_capture_overlay() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "note closures are objects").await;

        // The submit enqueues the prefilled open; apply it, then it must render.
        let opened = drain(&mut rx)
            .into_iter()
            .find(|a| matches!(a, Action::CaptureOpenText(t) if t == "closures are objects"));
        assert!(opened.is_some(), "expected a prefilled CaptureOpenText");
        app.handle(opened.unwrap()).await;
        assert!(app.capture.is_some());
        assert!(rendered_text(&mut app).contains("closures are objects"));
    }

    #[tokio::test]
    async fn command_unknown_verb_notifies_helpfully() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "wobble").await;
        let n = app.notification.expect("a warning is shown");
        assert_eq!(n.level, Level::Warning);
        assert!(n.text.contains("unknown"), "{}", n.text);
        assert!(n.text.contains(":help"), "{}", n.text);
    }

    #[tokio::test]
    async fn command_timer_stop_on_unbound_timer_warns() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        // A running-but-unbound timer: `:timer stop` must refuse with the same
        // guidance the Timer screen gives.
        app.timer = Some(
            serde_json::from_value(serde_json::json!({ "running": true, "bound": false })).unwrap(),
        );
        submit_command(&mut app, "timer stop").await;
        let n = app.notification.expect("a warning is shown");
        assert_eq!(n.level, Level::Warning);
        assert!(n.text.contains("bind"), "{}", n.text);
    }

    #[tokio::test]
    async fn command_help_lists_the_table() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "help").await;
        let n = app.notification.expect("help is shown");
        assert_eq!(n.level, Level::Info);
        assert!(n.text.contains("home"), "{}", n.text);
        assert!(n.text.contains("timer"), "{}", n.text);
    }

    #[tokio::test]
    async fn capture_edit_prefills_the_overlay_from_a_note() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        let note = serde_json::from_value(serde_json::json!({
            "id": 9, "title": "closures", "content": "closures are objects"
        }))
        .unwrap();
        app.handle(Action::CaptureOpenEdit(Box::new(note))).await;
        assert!(app.capture.is_some());
        assert!(rendered_text(&mut app).contains("Edit note"));
    }

    /// Feed a plain `Char` key through the real translate pipeline.
    fn press(app: &mut App, c: char) -> Option<Action> {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let ev = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        crate::app::event::translate(app, ev)
    }

    #[tokio::test]
    async fn g_prefix_navigates_to_the_ambient_surfaces() {
        let (mut app, _rx) = test_app(None);

        // `g` alone pends — no action yet.
        assert!(press(&mut app, 'g').is_none());
        assert!(app.goto_pending);

        // `g t` / `g p` / `g r` — the footer's goto grammar.
        assert!(matches!(
            press(&mut app, 't'),
            Some(Action::Goto(ScreenKind::Timer))
        ));
        assert!(!app.goto_pending); // prefix cleared after the destination

        press(&mut app, 'g');
        assert!(matches!(
            press(&mut app, 'p'),
            Some(Action::Goto(ScreenKind::Progress))
        ));
        press(&mut app, 'g');
        assert!(matches!(
            press(&mut app, 'r'),
            Some(Action::Goto(ScreenKind::Review))
        ));

        // `g w` reaches the week board.
        press(&mut app, 'g');
        assert!(matches!(
            press(&mut app, 'w'),
            Some(Action::Goto(ScreenKind::Week))
        ));
    }

    #[tokio::test]
    async fn gg_tops_a_list_and_is_inert_where_there_is_none() {
        let (mut app, _rx) = test_app(None);

        // On a list screen, `gg` is the top motion (single-`g` became `gg`).
        app.current = Screen::new(ScreenKind::Books);
        press(&mut app, 'g');
        assert!(matches!(press(&mut app, 'g'), Some(Action::BooksJumpStart)));

        // Home has no list — `gg` is a clean no-op.
        app.current = Screen::new(ScreenKind::Home);
        press(&mut app, 'g');
        assert!(press(&mut app, 'g').is_none());
    }

    #[tokio::test]
    async fn g_prefix_then_unmapped_key_clears_without_acting() {
        let (mut app, _rx) = test_app(None);
        press(&mut app, 'g');
        assert!(app.goto_pending);
        assert!(press(&mut app, 'z').is_none());
        assert!(!app.goto_pending); // consumed and cleared, no lingering prefix
    }
}
