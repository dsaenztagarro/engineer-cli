//! TUI shell. Owns terminal state, the event loop, and screen routing.

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

/// Between polls the header ticks elapsed locally from a monotonic baseline.
const TIMER_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// The server beat is presence-only, so throttling it is the client's job.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

pub struct PendingEditor {
    seed: String,
    target: EditorTarget,
}

pub enum EditorTarget {
    Capture,
    WeekNote { iso_week: String },
    QueueIntent { intent_id: u64 },
}

pub struct App {
    pub config: Config,
    pub api: ApiClient,
    pub user: Option<String>,
    pub current: Screen,
    pub notification: Option<Notification>,
    pub leader_pending: bool,
    pub goto_pending: bool,
    pub command_buffer: Option<String>,
    pub capture: Option<QuickCapture>,
    pub should_quit: bool,
    pub tx: mpsc::UnboundedSender<Action>,
    pub timer: Option<Timer>,
    pub timer_base: Option<Instant>,
    pub timer_stale: bool,
    pub timer_last_poll: Instant,
    /// Read-only here; writes go through `QueuedClient`.
    pub queue: Option<QueueStore>,
    pub queued_writes: usize,
    pub queue_diverged: bool,
    pub settings: Option<crate::api::TimerSettings>,
    pub overrun_pinged: Option<i64>,
    pub heartbeat_last: Instant,
    pub pending_editor: Option<PendingEditor>,
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
                    if let Some(action) = app.on_terminal_event(ev) {
                        app.handle(action).await;
                    }
                }
            }
            _ = ticker.tick() => app.on_tick(),
        }

        // Blocking is fine: the terminal is ours until the editor exits.
        if app.pending_editor.is_some() {
            if let Err(e) = run_editor(terminal, &mut app) {
                app.notify(Level::Error, format!("editor failed: {e}"));
            }
        }
    }

    Ok(())
}

fn run_editor(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let Some(pending) = app.pending_editor.take() else {
        return Ok(());
    };
    restore_terminal(terminal)?;
    let edited = crate::editor::edit(&pending.seed);
    resume_terminal(terminal)?;
    route_editor_outcome(app, pending.target, edited?);
    Ok(())
}

fn route_editor_outcome(app: &mut App, target: EditorTarget, outcome: EditorOutcome) {
    match target {
        EditorTarget::Capture => {
            if let EditorOutcome::Saved(text) = outcome {
                if !text.trim().is_empty() {
                    if let Some(cap) = app.capture.as_mut() {
                        cap.set_content(&text);
                    }
                }
            }
        }
        EditorTarget::WeekNote { iso_week } => match outcome {
            EditorOutcome::Saved(body) => app.dispatch(Action::WeekReflectSave { iso_week, body }),
            EditorOutcome::Aborted => app.dispatch(Action::WeekReflectAbort),
        },
        EditorTarget::QueueIntent { intent_id } => match outcome {
            EditorOutcome::Saved(buffer) => {
                app.dispatch(Action::TimerReconcileEditApply { intent_id, buffer })
            }
            EditorOutcome::Aborted => app.notify(
                Level::Info,
                "edit aborted — nothing changed, the queued write stays diverged".to_string(),
            ),
        },
    }
}

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

fn writes_reconciled(n: usize) -> String {
    if n == 1 {
        "1 queued write reconciled".to_string()
    } else {
        format!("{n} queued writes reconciled")
    }
}

/// `queued` is `None` when the state dir is unavailable; `cache_path` `None` is
/// the shared XDG cache, and tests pass a scratch one.
async fn run_timer_poll(
    api: ApiClient,
    tx: mpsc::UnboundedSender<Action>,
    queued: Option<QueuedClient>,
    cache_path: Option<PathBuf>,
) {
    // Drain before the read so the poll reflects what just synced.
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
            // Without a cached snapshot a TUI-only session would have nothing to
            // synthesize an offline pause/resume/stop from, and would refuse it.
            match &cache_path {
                Some(path) => crate::timer_cache::store_at(path, &t),
                None => crate::timer_cache::store(&t),
            }
            let _ = tx.send(Action::TimerLoaded(Box::new(t)));
        }
        // Fold cache ⊕ queue rather than let the header extrapolate a clock
        // the queue may have paused or stopped.
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
            // `GET /api/v1/timer` offers no conditional revalidation
            // (If-None-Match / 304), so every poll transfers the whole body.
            Action::RefreshTimer => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let queued = QueuedClient::new(&api).ok();
                    run_timer_poll(api, tx, queued, None).await;
                });
            }
            Action::TimerLoaded(t) => {
                // The server gates `over` on the user's knob.
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
                let _ = self
                    .current
                    .handle(Action::TimerLoaded(t), &self.api, &self.tx)
                    .await;
            }
            // Header-only: the screens keep their last live snapshot.
            Action::TimerStale(t) => {
                self.timer = Some(*t);
                self.timer_base = Some(Instant::now());
                self.timer_stale = true;
            }
            Action::TimerProvisional(t) => {
                self.timer = Some((*t).clone());
                self.timer_base = Some(Instant::now());
                self.timer_stale = false;
                let _ = self
                    .current
                    .handle(Action::TimerProvisional(t), &self.api, &self.tx)
                    .await;
            }
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
            Action::SettingsLoaded(s) => {
                self.settings = Some((*s).clone());
                let _ = self
                    .current
                    .handle(Action::SettingsLoaded(s), &self.api, &self.tx)
                    .await;
            }
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
                    // Failing to discover means the flow can't start (Tier 3,
                    // retry); a failure after it is recoverable in place.
                    let discovery = match crate::auth::discover(&cfg).await {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = tx.send(Action::LoginServerError(e.to_string()));
                            return;
                        }
                    };
                    let result = async {
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
            Action::LoginServerError(e) => {
                // A re-auth may have triggered the retry from elsewhere, so land
                // on Login first.
                if !matches!(self.current, Screen::Login(_)) {
                    self.current = Screen::new(ScreenKind::Login);
                }
                if let Screen::Login(s) = &mut self.current {
                    s.set_server_error(e);
                }
            }
            Action::SessionExpired => {
                self.capture = None;
                if !matches!(self.current, Screen::Login(_)) {
                    self.current = Screen::new(ScreenKind::Login);
                }
                if let Screen::Login(s) = &mut self.current {
                    s.set_expired();
                }
            }
            Action::Goto(kind) => {
                self.current = Screen::new(kind);
                self.current.on_enter(&self.api, &self.tx);
            }
            Action::CaptureOpen => self.capture = Some(QuickCapture::new()),
            Action::CaptureOpenText(text) => self.capture = Some(QuickCapture::with_text(&text)),
            Action::CaptureOpenEdit(note) => self.capture = Some(QuickCapture::for_edit(*note)),
            Action::CaptureClose => self.capture = None,
            Action::CaptureSaved => {
                self.capture = None;
                self.notify(Level::Success, "note saved");
                if self.current.kind() == ScreenKind::Notes {
                    self.dispatch(Action::RefreshNotes);
                }
            }
            Action::CaptureEditExternal => {
                if let Some(cap) = &self.capture {
                    self.pending_editor = Some(PendingEditor {
                        seed: cap.body(),
                        target: EditorTarget::Capture,
                    });
                }
            }
            Action::WeekReflectEdit { iso_week, seed } => {
                self.pending_editor = Some(PendingEditor {
                    seed,
                    target: EditorTarget::WeekNote { iso_week },
                });
            }
            Action::QueueIntentEdit { intent_id, seed } => {
                self.pending_editor = Some(PendingEditor {
                    seed,
                    target: EditorTarget::QueueIntent { intent_id },
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
            | Action::CaptureBookResults(_)
            | Action::CaptureAnchorPickerOpen
            | Action::CaptureAnchorPickerClose
            | Action::CaptureAnchorPickerSubmit
            | Action::CaptureAnchorMove(_)
            | Action::CaptureAnchorInput(_)
            | Action::CaptureAnchorBackspace
            | Action::CaptureAnchorDataLoaded(_)
            | Action::CaptureAnchorDataFailed(_)) => {
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
            Command::Log => self.dispatch(Action::Goto(ScreenKind::ActivityNew)),
            Command::Target => {
                self.dispatch(Action::Goto(ScreenKind::Progress));
                self.dispatch(Action::ProgressDeclareBegin);
            }
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
            timer: self.timer_cell_spans(frame.area().width < 70),
            notification: self.notification.as_ref(),
            hints,
        };
        let body = render_chrome(frame, frame.area(), chrome);
        self.current.render(frame, body);
        if let Some(cap) = self.capture.as_mut() {
            cap.render(frame, body);
        }
    }

    fn on_terminal_event(&mut self, ev: crossterm::event::Event) -> Option<Action> {
        if matches!(ev, crossterm::event::Event::Key(_)) {
            self.beat_presence_if_active();
        }
        event::translate(self, ev)
    }

    fn on_tick(&mut self) {
        self.expire_stale_notification();
        self.poll_timer_if_due();
        self.refresh_queued_writes();
    }

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

    fn expire_stale_notification(&mut self) {
        if self
            .notification
            .as_ref()
            .is_some_and(Notification::is_expired)
        {
            self.notification = None;
        }
    }

    fn poll_timer_if_due(&mut self) {
        if self.user.is_some() && self.timer_last_poll.elapsed() >= TIMER_POLL_INTERVAL {
            self.timer_last_poll = Instant::now();
            self.dispatch(Action::RefreshTimer);
        }
    }

    /// Read every tick (a tiny file) so another process enqueuing or resolving
    /// shows up too.
    fn refresh_queued_writes(&mut self) {
        let summary = self.queue.as_ref().and_then(|q| q.summary().ok());
        self.queued_writes = summary.map_or(0, |s| s.in_play());
        self.queue_diverged = summary.is_some_and(|s| s.diverged > 0);
    }

    fn timer_cell_spans(&self, narrow: bool) -> Option<Vec<ratatui::text::Span<'static>>> {
        let mut spans = self.timer_clock_spans(narrow).unwrap_or_default();
        if self.queue_diverged {
            if !spans.is_empty() {
                spans.push(ratatui::text::Span::raw(" "));
            }
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

    fn timer_clock_spans(&self, narrow: bool) -> Option<Vec<ratatui::text::Span<'static>>> {
        let t = self.timer.as_ref()?;
        let elapsed = screens::timer::live_elapsed(t, self.timer_base);
        let offer = self
            .settings
            .as_ref()
            .and_then(|s| screens::timer::offer_for(t, s, jiff::Timestamp::now()))
            .is_some();
        let mut spans = crate::ui::widgets::timer_cell(t, elapsed, narrow, offer)?;
        if self.timer_stale {
            spans.push(ratatui::text::Span::styled(" ~", crate::ui::theme::muted()));
        }
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
    async fn a_401_from_any_screen_routes_to_reauth() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        assert!(matches!(app.current, Screen::Home(_)));
        app.handle(Action::SessionExpired).await;
        assert!(matches!(app.current, Screen::Login(_)), "routed to Login");
        let text = rendered_text(&mut app);
        assert!(text.contains("session expired"), "re-auth prompt: {text}");
    }

    #[tokio::test]
    async fn a_burst_of_401s_does_not_stack_login_screens() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::SessionExpired).await;
        app.handle(Action::SessionExpired).await;
        assert!(matches!(app.current, Screen::Login(_)));
    }

    #[tokio::test]
    async fn an_unreachable_identity_server_raises_the_blocking_screen() {
        let (mut app, _rx) = test_app(None);
        app.handle(Action::LoginServerError("identity.dev → HTTP 500".into()))
            .await;
        assert!(matches!(app.current, Screen::Login(_)));
        let text = rendered_text(&mut app);
        assert!(text.contains("can't reach the identity server"), "{text}");
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
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(!beat_fired(&app), "nothing running → no presence beat");

        let (mut app, _rx) = test_app(None);
        app.timer = Some(running_timer(300));
        app.heartbeat_last = Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1);
        app.beat_presence_if_active();
        assert!(!beat_fired(&app), "signed out → no presence beat");
    }

    fn stale_heartbeat() -> Instant {
        Instant::now() - HEARTBEAT_INTERVAL - Duration::from_secs(1)
    }

    #[tokio::test]
    async fn a_keystroke_marks_presence_once_the_interval_has_passed() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(300));
        app.heartbeat_last = stale_heartbeat();
        app.on_terminal_event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('j')),
        ));
        assert!(beat_fired(&app));
    }

    #[tokio::test]
    async fn a_non_key_terminal_event_is_not_presence() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(300));
        let last = stale_heartbeat();
        app.heartbeat_last = last;
        app.on_terminal_event(crossterm::event::Event::Resize(80, 24));
        app.on_terminal_event(crossterm::event::Event::FocusGained);
        assert_eq!(app.heartbeat_last, last);
    }

    #[tokio::test]
    async fn without_input_the_tick_never_marks_presence() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(300));
        let last = stale_heartbeat();
        app.heartbeat_last = last;
        app.on_tick();
        app.handle(Action::TimerLoaded(Box::new(running_timer(310))))
            .await;
        assert_eq!(app.heartbeat_last, last);
    }

    fn capture_with_draft(app: &mut App, draft: &str) {
        app.capture = Some(QuickCapture::with_text(draft));
    }

    #[tokio::test]
    async fn quitting_the_editor_without_writing_keeps_the_capture_draft() {
        let (mut app, _rx) = test_app(Some("a@b.c".into()));
        capture_with_draft(&mut app, "a half-formed thought");
        route_editor_outcome(&mut app, EditorTarget::Capture, EditorOutcome::Aborted);
        assert_eq!(app.capture.unwrap().body(), "a half-formed thought");
    }

    #[tokio::test]
    async fn an_empty_editor_save_keeps_the_capture_draft() {
        let (mut app, _rx) = test_app(Some("a@b.c".into()));
        capture_with_draft(&mut app, "a half-formed thought");
        route_editor_outcome(
            &mut app,
            EditorTarget::Capture,
            EditorOutcome::Saved("  \n".into()),
        );
        assert_eq!(app.capture.unwrap().body(), "a half-formed thought");
    }

    #[tokio::test]
    async fn a_written_editor_buffer_replaces_the_capture_draft() {
        let (mut app, _rx) = test_app(Some("a@b.c".into()));
        capture_with_draft(&mut app, "a half-formed thought");
        route_editor_outcome(
            &mut app,
            EditorTarget::Capture,
            EditorOutcome::Saved("the finished thought".into()),
        );
        assert_eq!(app.capture.unwrap().body(), "the finished thought");
    }

    #[tokio::test]
    async fn header_shows_running_timer_pill() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let text = rendered_text(&mut app);
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

        app.handle(Action::TimerStale(Box::new(running_timer(272))))
            .await;
        assert!(app.timer_stale);
        let text = rendered_text(&mut app);
        assert!(text.contains("● 04:32 consensus ~"), "{text}");

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
        app.queued_writes = 2;
        let text = rendered_text(&mut app);
        assert!(text.contains("↑2"), "{text}");
        app.queued_writes = 0;
        assert!(!rendered_text(&mut app).contains('↑'));
    }

    #[tokio::test]
    async fn header_wears_the_diverged_chip_even_without_a_clock() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.queue_diverged = true;
        let text = rendered_text(&mut app);
        assert!(text.contains("diverged"), "{text}");

        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let text = rendered_text(&mut app);
        assert!(text.contains("● 04:32"), "{text}");
        assert!(text.contains("diverged"), "{text}");

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

    // --- Reconnect: the replay transcript and the synced tile ---

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

        app.expire_stale_notification();
        assert!(app.notification.is_some(), "not yet past its TTL");

        tokio::time::advance(Level::Success.ttl() + Duration::from_secs(1)).await;
        app.expire_stale_notification();
        assert!(app.notification.is_none(), "auto-dismissed after ~4s");
    }

    #[tokio::test]
    async fn a_diverged_drain_shows_no_synced_tile() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::ReplayProgress {
            word: "start".into(),
        })
        .await;
        assert!(app.notification.is_some());

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
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "paused": true
            })))
            .expect(1)
            .mount(&server)
            .await;
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
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ReplayProgress { word } if word == "pause")),
            "{actions:?}"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ReplayFinished(r) if r.replayed == 1 && !r.diverged)),
            "{actions:?}"
        );
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

        let opened = drain(&mut rx)
            .into_iter()
            .find(|a| matches!(a, Action::CaptureOpenText(t) if t == "closures are objects"));
        assert!(opened.is_some(), "expected a prefilled CaptureOpenText");
        app.handle(opened.unwrap()).await;
        assert!(app.capture.is_some());
        assert!(rendered_text(&mut app).contains("closures are objects"));
    }

    #[tokio::test]
    async fn command_log_opens_the_activity_capture_form() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "log").await;
        let goto = drain(&mut rx)
            .into_iter()
            .find(|a| matches!(a, Action::Goto(ScreenKind::ActivityNew)));
        assert!(goto.is_some(), "expected a Goto(ActivityNew)");
        app.handle(goto.unwrap()).await;
        assert_eq!(app.current.kind(), ScreenKind::ActivityNew);
    }

    #[tokio::test]
    async fn command_target_lands_on_progress_and_opens_declare() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "target").await;
        let actions = drain(&mut rx);
        let goto = actions
            .iter()
            .position(|a| matches!(a, Action::Goto(ScreenKind::Progress)));
        let begin = actions
            .iter()
            .position(|a| matches!(a, Action::ProgressDeclareBegin));
        assert!(goto.is_some(), "expected a Goto(Progress)");
        assert!(begin.is_some(), "expected a ProgressDeclareBegin");
        assert!(goto < begin, "Goto must land before the declare begins");
        // The declare overlay renders only over loaded meters, so seed a week.
        app.handle(Action::Goto(ScreenKind::Progress)).await;
        let progress = serde_json::from_value(serde_json::json!({
            "week": {
                "id": "2026-W29", "monday": "2026-07-13", "sunday": "2026-07-19",
                "elapsed_days": 4, "now_fraction": 0.5714
            },
            "totals": { "actual_minutes": 0, "activity_count": 0, "thin": false }
        }))
        .unwrap();
        app.handle(Action::ProgressLoaded(Box::new(progress))).await;
        app.handle(Action::ProgressDeclareBegin).await;
        assert_eq!(app.current.kind(), ScreenKind::Progress);
        assert!(
            rendered_text(&mut app).contains("declare a target"),
            "the declare overlay should be open"
        );
    }

    #[tokio::test]
    async fn command_t_prefix_still_resolves_to_the_timer() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        submit_command(&mut app, "t").await;
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::Goto(ScreenKind::Timer))));
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

        assert!(press(&mut app, 'g').is_none());
        assert!(app.goto_pending);

        assert!(matches!(
            press(&mut app, 't'),
            Some(Action::Goto(ScreenKind::Timer))
        ));
        assert!(!app.goto_pending);

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

        press(&mut app, 'g');
        assert!(matches!(
            press(&mut app, 'w'),
            Some(Action::Goto(ScreenKind::Week))
        ));
    }

    #[tokio::test]
    async fn gg_tops_a_list_and_is_inert_where_there_is_none() {
        let (mut app, _rx) = test_app(None);

        app.current = Screen::new(ScreenKind::Books);
        press(&mut app, 'g');
        assert!(matches!(press(&mut app, 'g'), Some(Action::BooksJumpStart)));

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
        assert!(!app.goto_pending);
    }

    fn overrun_snapshot(id: i64) -> Timer {
        serde_json::from_value(serde_json::json!({
            "id": id, "running": true, "bound": true, "paused": false,
            "elapsed_seconds": 7300, "planned_minutes": 120, "over": true,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn the_overrun_ping_fires_once_per_timer() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::TimerLoaded(Box::new(overrun_snapshot(7))))
            .await;
        let ping = app
            .notification
            .take()
            .expect("the first overrun read pings");
        assert_eq!(ping.level, Level::Warning);
        assert!(ping.text.contains("planned 2h 00m"), "{}", ping.text);

        app.handle(Action::TimerLoaded(Box::new(overrun_snapshot(7))))
            .await;
        assert!(app.notification.is_none(), "a later poll of the same clock");

        app.handle(Action::TimerLoaded(Box::new(overrun_snapshot(8))))
            .await;
        assert!(app.notification.is_some(), "a new timer pings again");
    }

    #[tokio::test]
    async fn a_session_expiry_dismisses_the_capture_overlay() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::CaptureOpen).await;
        app.handle(Action::SessionExpired).await;
        assert!(app.capture.is_none());
    }

    #[tokio::test]
    async fn a_saved_note_refreshes_the_notes_browser_only_when_it_is_showing() {
        let (mut app, mut rx) = test_app(Some("alice@example.com".into()));
        app.handle(Action::CaptureSaved).await;
        assert!(!drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::RefreshNotes)));

        app.current = Screen::new(ScreenKind::Notes);
        app.handle(Action::CaptureSaved).await;
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::RefreshNotes)));
    }

    #[tokio::test]
    async fn the_header_poll_waits_for_a_signed_in_user() {
        let (mut app, mut rx) = test_app(None);
        app.timer_last_poll = Instant::now() - TIMER_POLL_INTERVAL;
        app.on_tick();
        assert!(!drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::RefreshTimer)));

        app.user = Some("alice@example.com".into());
        app.on_tick();
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::RefreshTimer)));
    }

    #[tokio::test]
    async fn below_seventy_columns_the_header_cell_drops_its_label() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(69, 12)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("● 04:32"), "{text}");
        assert!(!text.contains("consensus"), "{text}");
    }

    fn week_note() -> EditorTarget {
        EditorTarget::WeekNote {
            iso_week: "2026-W29".into(),
        }
    }

    #[tokio::test]
    async fn an_emptied_reflection_is_saved_as_a_clear_not_read_as_an_abort() {
        let (mut app, mut rx) = test_app(Some("a@b.c".into()));
        route_editor_outcome(&mut app, week_note(), EditorOutcome::Saved(String::new()));
        assert!(drain(&mut rx)
            .iter()
            .any(|a| matches!(a, Action::WeekReflectSave { body, .. } if body.is_empty())));
    }

    #[tokio::test]
    async fn an_aborted_reflection_keeps_the_stored_note() {
        let (mut app, mut rx) = test_app(Some("a@b.c".into()));
        route_editor_outcome(&mut app, week_note(), EditorOutcome::Aborted);
        let sent = drain(&mut rx);
        assert!(sent.iter().any(|a| matches!(a, Action::WeekReflectAbort)));
        assert!(!sent
            .iter()
            .any(|a| matches!(a, Action::WeekReflectSave { .. })));
    }

    #[tokio::test]
    async fn an_aborted_queue_intent_edit_leaves_the_intent_diverged() {
        let (mut app, mut rx) = test_app(Some("a@b.c".into()));
        route_editor_outcome(
            &mut app,
            EditorTarget::QueueIntent { intent_id: 3 },
            EditorOutcome::Aborted,
        );
        assert!(drain(&mut rx).is_empty(), "no retry is dispatched");
        assert_eq!(app.notification.unwrap().level, Level::Info);
    }
}
