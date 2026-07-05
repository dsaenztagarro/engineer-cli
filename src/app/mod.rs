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
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::api::{ApiClient, Timer};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::ui::notify::{Level, Notification};

mod action;
mod event;
pub mod screens;

pub use action::Action;

use screens::{Screen, ScreenKind};

const TICK: Duration = Duration::from_millis(250);

/// How often the header timer cell re-polls the server. Between polls the
/// displayed elapsed is ticked locally from `elapsed_seconds` + a monotonic
/// baseline, so the cell advances smoothly without a request per second.
const TIMER_POLL_INTERVAL: Duration = Duration::from_secs(15);

pub struct App {
    pub config: Config,
    pub api: ApiClient,
    pub user: Option<String>,
    pub current: Screen,
    pub notification: Option<Notification>,
    pub leader_pending: bool,
    pub command_buffer: Option<String>,
    pub should_quit: bool,
    pub tx: mpsc::UnboundedSender<Action>,
    /// Latest timer snapshot shared by every screen's header cell (`None` until
    /// the first poll / when no timer is running).
    pub timer: Option<Timer>,
    /// Monotonic instant the current `timer` snapshot was received — the base
    /// for ticking the displayed elapsed between polls.
    pub timer_base: Option<Instant>,
    /// When the last header poll was dispatched, to honour `TIMER_POLL_INTERVAL`.
    pub timer_last_poll: Instant,
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
        command_buffer: None,
        should_quit: false,
        tx: tx.clone(),
        timer: None,
        timer_base: None,
        timer_last_poll: Instant::now(),
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
                    if let Some(action) = event::translate(&mut app, ev) {
                        app.handle(action).await;
                    }
                }
            }
            _ = ticker.tick() => {
                if app.notification.as_ref().is_some_and(Notification::is_expired) {
                    app.notification = None;
                }
                app.poll_timer_if_due();
            }
        }
    }

    Ok(())
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
            Action::SetUser(email) => self.user = Some(email),
            // Header timer cell. Plain polling: `GET /api/v1/timer` returns the
            // full snapshot on each request — the endpoint does not offer
            // conditional revalidation (If-None-Match / 304), so every poll
            // transfers the whole body.
            Action::RefreshTimer => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match api.timer().await {
                        Ok(t) => {
                            let _ = tx.send(Action::TimerLoaded(Box::new(t)));
                        }
                        Err(e) => {
                            tracing::warn!(target: "engineer_cli::api", error = %e, "timer poll failed");
                        }
                    }
                });
            }
            Action::TimerLoaded(t) => {
                self.timer = Some((*t).clone());
                self.timer_base = Some(Instant::now());
                // Forward to the Timer screen so its detailed view mirrors the
                // same snapshot; other screens ignore it.
                let _ = self
                    .current
                    .handle(Action::TimerLoaded(t), &self.api, &self.tx)
                    .await;
            }
            // Wipe the header cell without touching the current screen (used
            // after a stop, so the segment confirmation view is preserved).
            Action::TimerCleared => {
                self.timer = None;
                self.timer_base = None;
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
            Action::CommandBegin => { /* buffer already initialised by event layer */ }
            Action::CommandInput | Action::CommandBackspace => { /* buffer mutated in event layer */
            }
            Action::CommandCancel => {
                self.command_buffer = None;
            }
            Action::CommandSubmit => {
                let buf = self.command_buffer.take().unwrap_or_default();
                match buf.trim() {
                    "q" | "quit" => self.should_quit = true,
                    "home" => self.dispatch(Action::Goto(ScreenKind::Home)),
                    "books" => self.dispatch(Action::Goto(ScreenKind::Books)),
                    "timer" => self.dispatch(Action::Goto(ScreenKind::Timer)),
                    "activity" | "a" => self.dispatch(Action::Goto(ScreenKind::ActivityNew)),
                    "logout" => self.notify(Level::Info, "run `engineer logout` from the shell"),
                    "logs" => match Config::log_dir() {
                        Ok(dir) => self.notify(Level::Info, format!("logs: {}", dir.display())),
                        Err(e) => self.notify(Level::Error, format!("log dir error: {e}")),
                    },
                    "w" => self.dispatch(Action::ActivitySubmit),
                    other => self.notify(Level::Warning, format!("unknown command: :{other}")),
                }
            }
            other => {
                let next = self.current.handle(other, &self.api, &self.tx).await;
                if let Some((level, text)) = next {
                    self.notify(level, text);
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame) {
        use crate::ui::layout::{render_chrome, Chrome};

        let host = self.config.identity_url.host_str().unwrap_or("identity");
        let chrome = Chrome {
            user: self.user.as_deref(),
            identity_host: host,
            screen_title: self.current.title(),
            timer: self.timer_cell_spans(),
            notification: self.notification.as_ref(),
            hints: self
                .current
                .hints(self.leader_pending, self.command_buffer.as_deref()),
        };
        let body = render_chrome(frame, frame.area(), chrome);
        self.current.render(frame, body);
    }

    /// Re-poll the header timer snapshot when a poll is due and we're signed in.
    fn poll_timer_if_due(&mut self) {
        if self.user.is_some() && self.timer_last_poll.elapsed() >= TIMER_POLL_INTERVAL {
            self.timer_last_poll = Instant::now();
            self.dispatch(Action::RefreshTimer);
        }
    }

    /// The header timer cell spans, with the displayed elapsed ticked locally
    /// from the last snapshot. `None` when no timer is running.
    fn timer_cell_spans(&self) -> Option<Vec<ratatui::text::Span<'static>>> {
        let t = self.timer.as_ref()?;
        let elapsed = screens::timer::live_elapsed(t, self.timer_base);
        crate::ui::widgets::timer_cell(t.running, t.paused, elapsed)
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
            command_buffer: None,
            should_quit: false,
            tx,
            timer: None,
            timer_base: None,
            timer_last_poll: Instant::now(),
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
    async fn header_shows_running_timer_pill() {
        let (mut app, _rx) = test_app(Some("alice@example.com".into()));
        app.timer = Some(running_timer(272));
        app.timer_base = Some(Instant::now());
        let text = rendered_text(&mut app);
        // ● + mm:ss in the header, never the activity title/label.
        assert!(text.contains("● 04:32"), "{text}");
        assert!(!text.contains("consensus"), "{text}");
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
}
