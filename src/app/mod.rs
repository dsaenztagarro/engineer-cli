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
use std::time::Duration;
use tokio::sync::mpsc;

use crate::api::ApiClient;
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::ui::notify::{Level, Notification};

mod action;
mod event;
pub mod screens;

pub use action::Action;

use screens::{Screen, ScreenKind};

const TICK: Duration = Duration::from_millis(250);

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
    let start = if logged_in { ScreenKind::Home } else { ScreenKind::Login };

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
    };

    // Kick off initial loads (only meaningful once authenticated).
    if logged_in {
        app.dispatch(Action::FetchMe);
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
            Action::CommandInput(_) | Action::CommandBackspace => { /* buffer mutated in event layer */ }
            Action::CommandCancel => {
                self.command_buffer = None;
            }
            Action::CommandSubmit => {
                let buf = self.command_buffer.take().unwrap_or_default();
                match buf.trim() {
                    "q" | "quit" => self.should_quit = true,
                    "home" => self.dispatch(Action::Goto(ScreenKind::Home)),
                    "books" => self.dispatch(Action::Goto(ScreenKind::Books)),
                    "activity" | "a" => self.dispatch(Action::Goto(ScreenKind::ActivityNew)),
                    "logout" => self.notify(Level::Info, "run `engineer logout` from the shell"),
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
            notification: self.notification.as_ref(),
            hints: self.current.hints(self.leader_pending, self.command_buffer.as_deref()),
        };
        let body = render_chrome(frame, frame.area(), chrome);
        self.current.render(frame, body);
    }
}
