//! Screen routing. Each screen is a self-contained reducer + renderer.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::ApiClient;
use crate::app::action::Action;
use crate::ui::notify::Level;

pub mod activity_new;
pub mod book_detail;
pub mod books;
pub mod home;
pub mod login;
pub mod notes;
pub mod progress;
pub mod timer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    Login,
    Home,
    Books,
    BookDetail,
    ActivityNew,
    Progress,
    Timer,
    Notes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    Normal,
    Insert,
}

pub enum Screen {
    Login(login::Login),
    Home(home::Home),
    Books(books::Books),
    BookDetail(book_detail::BookDetail),
    ActivityNew(activity_new::ActivityNew),
    Progress(progress::Progress),
    Timer(timer::Timer),
    Notes(notes::Notes),
}

impl Screen {
    pub fn new(kind: ScreenKind) -> Self {
        match kind {
            ScreenKind::Login => Self::Login(login::Login::default()),
            ScreenKind::Home => Self::Home(home::Home::default()),
            ScreenKind::Books => Self::Books(books::Books::default()),
            ScreenKind::BookDetail => Self::BookDetail(book_detail::BookDetail::default()),
            ScreenKind::ActivityNew => Self::ActivityNew(activity_new::ActivityNew::default()),
            ScreenKind::Progress => Self::Progress(progress::Progress::default()),
            ScreenKind::Timer => Self::Timer(timer::Timer::default()),
            ScreenKind::Notes => Self::Notes(notes::Notes::default()),
        }
    }

    pub fn kind(&self) -> ScreenKind {
        match self {
            Self::Login(_) => ScreenKind::Login,
            Self::Home(_) => ScreenKind::Home,
            Self::Books(_) => ScreenKind::Books,
            Self::BookDetail(_) => ScreenKind::BookDetail,
            Self::ActivityNew(_) => ScreenKind::ActivityNew,
            Self::Progress(_) => ScreenKind::Progress,
            Self::Timer(_) => ScreenKind::Timer,
            Self::Notes(_) => ScreenKind::Notes,
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Login(_) => "Sign in",
            Self::Home(_) => "Home",
            Self::Books(_) => "Books",
            Self::BookDetail(_) => "Book",
            Self::ActivityNew(_) => "New activity",
            Self::Progress(_) => "Progress",
            Self::Timer(_) => "Timer",
            Self::Notes(_) => "Notes",
        }
    }

    pub fn mode(&self) -> ScreenMode {
        match self {
            Self::ActivityNew(s) => s.mode(),
            _ => ScreenMode::Normal,
        }
    }

    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        match self {
            Self::Login(s) => s.on_enter(api, tx),
            Self::Home(s) => s.on_enter(api, tx),
            Self::Books(s) => s.on_enter(api, tx),
            Self::BookDetail(s) => s.on_enter(api, tx),
            Self::ActivityNew(s) => s.on_enter(api, tx),
            Self::Progress(s) => s.on_enter(api, tx),
            Self::Timer(s) => s.on_enter(api, tx),
            Self::Notes(s) => s.on_enter(api, tx),
        }
    }

    /// Screens may consume keys before the global keymap (used for inline edits).
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self {
            Self::Books(s) => s.intercept_key(key),
            Self::BookDetail(s) => s.intercept_key(key),
            Self::Timer(s) => s.intercept_key(key),
            Self::Notes(s) => s.intercept_key(key),
            _ => None,
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match self {
            Self::Login(s) => s.handle(action, api, tx).await,
            Self::Home(s) => s.handle(action, api, tx).await,
            Self::Books(s) => s.handle(action, api, tx).await,
            Self::BookDetail(s) => s.handle(action, api, tx).await,
            Self::ActivityNew(s) => s.handle(action, api, tx).await,
            Self::Progress(s) => s.handle(action, api, tx).await,
            Self::Timer(s) => s.handle(action, api, tx).await,
            Self::Notes(s) => s.handle(action, api, tx).await,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            Self::Login(s) => s.render(frame, area),
            Self::Home(s) => s.render(frame, area),
            Self::Books(s) => s.render(frame, area),
            Self::BookDetail(s) => s.render(frame, area),
            Self::ActivityNew(s) => s.render(frame, area),
            Self::Progress(s) => s.render(frame, area),
            Self::Timer(s) => s.render(frame, area),
            Self::Notes(s) => s.render(frame, area),
        }
    }

    pub fn hints(&self, leader: bool, command: Option<&str>) -> Line<'static> {
        if let Some(buf) = command {
            return Line::from(vec![
                ratatui::text::Span::styled(":", crate::ui::theme::focused()),
                ratatui::text::Span::raw(buf.to_string()),
                ratatui::text::Span::styled("█", crate::ui::theme::muted()),
            ]);
        }
        if leader {
            return crate::ui::widgets::footer_hints(&[
                ("1", "home"),
                ("2", "books"),
                ("3", "progress"),
                ("t", "timer"),
                ("a", "+activity"),
                ("n", "notes"),
                ("c", "+note"),
                ("s", "save"),
            ]);
        }
        match self {
            Self::Login(s) => s.hints(),
            Self::Home(_) => crate::ui::widgets::footer_hints(&[
                ("t", "timer"),
                ("a", "+activity"),
                ("b", "books"),
                ("n", "notes"),
                ("c", "+note"),
                ("p", "progress"),
                (":", "cmd"),
                ("q", "quit"),
            ]),
            Self::Books(s) => s.hints(),
            Self::BookDetail(s) => s.hints(),
            Self::ActivityNew(s) => s.hints(),
            Self::Progress(s) => s.hints(),
            Self::Timer(s) => s.hints(),
            Self::Notes(s) => s.hints(),
        }
    }
}
