//! Screen routing. Each screen is a self-contained reducer + renderer.

use std::path::PathBuf;

use crossterm::event::KeyEvent;
use jiff::{ToSpan, Zoned};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::ApiClient;
use crate::app::action::Action;
use crate::queue::{QueueStore, QueuedClient};
use crate::ui::notify::Level;

pub mod activities;
pub mod activity_new;
pub mod audit;
pub mod book_detail;
pub mod books;
pub mod home;
pub mod inbox;
pub mod login;
pub mod notes;
pub mod progress;
pub mod review;
pub mod settings;
pub mod timer;
pub mod week;

/// The ISO week id (`YYYY-Www`) `offset` weeks from the current study week
/// (0 = this week, -1 = last week). The one derivation the week-dialect screens
/// (Progress, Week) step with, so `[`/`]`/`t` agree across them.
pub(crate) fn iso_week_for_offset(offset: i32) -> String {
    let today = Zoned::now().date();
    let target = today
        .checked_add((offset as i64 * 7).days())
        .unwrap_or(today);
    let iso = target.iso_week_date();
    format!("{:04}-W{:02}", iso.year(), iso.week())
}

/// The `week` query parameter for the Progress endpoint: `None` for the current
/// week (the server picks its own default), else the explicit ISO week id.
pub(crate) fn week_param(offset: i32) -> Option<String> {
    (offset != 0).then(|| iso_week_for_offset(offset))
}

/// Left-align `s` into `width` columns, truncating with an ellipsis when it
/// overruns — the shared row-label fitter for the week-dialect tables.
pub(crate) fn pad_or_truncate(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len > width {
        let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        format!("{s:<width$}")
    }
}

/// Queue + read-cache locations for the offline write seam. `None` (production)
/// uses the shared XDG paths (`QueuedClient::new`); tests inject a scratch dir so
/// a spawned write never touches the real queue. Shared by every screen that
/// routes writes through `QueuedClient` (Timer, Week).
pub(crate) type QueuePaths = Option<(PathBuf, PathBuf)>;

/// Build the write seam a spawned task enqueues through — the shared XDG queue,
/// or the test scratch paths when the screen was handed some.
pub(crate) fn open_queued(
    api: &ApiClient,
    paths: &QueuePaths,
) -> Result<QueuedClient, crate::queue::QueueError> {
    match paths {
        Some((queue, cache)) => Ok(QueuedClient::with_paths(
            api,
            QueueStore::at(queue.clone()),
            cache.clone(),
        )),
        None => QueuedClient::new(api),
    }
}

/// Loud failure when the queue seam itself can't open — the write can't even be
/// deferred, so say so rather than dropping the gesture.
pub(crate) fn notify_seam_error(
    tx: &UnboundedSender<Action>,
    context: &str,
    e: impl std::fmt::Display,
) {
    let _ = tx.send(Action::Notify {
        level: Level::Error,
        text: format!("{context}: {e}"),
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    Login,
    Home,
    Books,
    BookDetail,
    ActivityNew,
    Activities,
    Progress,
    Timer,
    Notes,
    Review,
    Settings,
    Audit,
    Week,
    Inbox,
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
    Activities(activities::Activities),
    Progress(progress::Progress),
    Timer(timer::Timer),
    Notes(notes::Notes),
    // Boxed: the review screen holds three stages' worth of state (dashboard,
    // sitting, browse), making it much larger than the other variants.
    Review(Box<review::Review>),
    Settings(settings::Settings),
    Audit(audit::Audit),
    Week(week::Week),
    Inbox(inbox::Inbox),
}

impl Screen {
    pub fn new(kind: ScreenKind) -> Self {
        match kind {
            ScreenKind::Login => Self::Login(login::Login::default()),
            ScreenKind::Home => Self::Home(home::Home::default()),
            ScreenKind::Books => Self::Books(books::Books::default()),
            ScreenKind::BookDetail => Self::BookDetail(book_detail::BookDetail::default()),
            ScreenKind::ActivityNew => Self::ActivityNew(activity_new::ActivityNew::default()),
            ScreenKind::Activities => Self::Activities(activities::Activities::default()),
            ScreenKind::Progress => Self::Progress(progress::Progress::default()),
            ScreenKind::Timer => Self::Timer(timer::Timer::default()),
            ScreenKind::Notes => Self::Notes(notes::Notes::default()),
            ScreenKind::Review => Self::Review(Box::default()),
            ScreenKind::Settings => Self::Settings(settings::Settings::default()),
            ScreenKind::Audit => Self::Audit(audit::Audit::default()),
            ScreenKind::Week => Self::Week(week::Week::default()),
            ScreenKind::Inbox => Self::Inbox(inbox::Inbox::default()),
        }
    }

    pub fn kind(&self) -> ScreenKind {
        match self {
            Self::Login(_) => ScreenKind::Login,
            Self::Home(_) => ScreenKind::Home,
            Self::Books(_) => ScreenKind::Books,
            Self::BookDetail(_) => ScreenKind::BookDetail,
            Self::ActivityNew(_) => ScreenKind::ActivityNew,
            Self::Activities(_) => ScreenKind::Activities,
            Self::Progress(_) => ScreenKind::Progress,
            Self::Timer(_) => ScreenKind::Timer,
            Self::Notes(_) => ScreenKind::Notes,
            Self::Review(_) => ScreenKind::Review,
            Self::Settings(_) => ScreenKind::Settings,
            Self::Audit(_) => ScreenKind::Audit,
            Self::Week(_) => ScreenKind::Week,
            Self::Inbox(_) => ScreenKind::Inbox,
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Login(_) => "Sign in",
            Self::Home(_) => "Home",
            Self::Books(_) => "Books",
            Self::BookDetail(_) => "Book",
            Self::ActivityNew(_) => "New activity",
            Self::Activities(_) => "Activities",
            Self::Progress(_) => "Progress",
            Self::Timer(_) => "Timer",
            Self::Notes(_) => "Notes",
            Self::Review(_) => "Review",
            Self::Settings(_) => "Settings · Timer",
            Self::Audit(_) => "Progress · Segment audit",
            Self::Week(_) => "Week",
            Self::Inbox(_) => "Inbox",
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
            Self::Activities(s) => s.on_enter(api, tx),
            Self::Progress(s) => s.on_enter(api, tx),
            Self::Timer(s) => s.on_enter(api, tx),
            Self::Notes(s) => s.on_enter(api, tx),
            Self::Review(s) => s.on_enter(api, tx),
            Self::Settings(s) => s.on_enter(api, tx),
            Self::Audit(s) => s.on_enter(api, tx),
            Self::Week(s) => s.on_enter(api, tx),
            Self::Inbox(s) => s.on_enter(api, tx),
        }
    }

    /// Screens may consume keys before the global keymap (used for inline edits).
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self {
            Self::Books(s) => s.intercept_key(key),
            Self::BookDetail(s) => s.intercept_key(key),
            Self::Activities(s) => s.intercept_key(key),
            Self::Progress(s) => s.intercept_key(key),
            Self::Timer(s) => s.intercept_key(key),
            Self::Notes(s) => s.intercept_key(key),
            Self::Review(s) => s.intercept_key(key),
            Self::Week(s) => s.intercept_key(key),
            Self::Inbox(s) => s.intercept_key(key),
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
            Self::Activities(s) => s.handle(action, api, tx).await,
            Self::Progress(s) => s.handle(action, api, tx).await,
            Self::Timer(s) => s.handle(action, api, tx).await,
            Self::Notes(s) => s.handle(action, api, tx).await,
            Self::Review(s) => s.handle(action, api, tx).await,
            Self::Settings(s) => s.handle(action, api, tx).await,
            Self::Audit(s) => s.handle(action, api, tx).await,
            Self::Week(s) => s.handle(action, api, tx).await,
            Self::Inbox(s) => s.handle(action, api, tx).await,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            Self::Login(s) => s.render(frame, area),
            Self::Home(s) => s.render(frame, area),
            Self::Books(s) => s.render(frame, area),
            Self::BookDetail(s) => s.render(frame, area),
            Self::ActivityNew(s) => s.render(frame, area),
            Self::Activities(s) => s.render(frame, area),
            Self::Progress(s) => s.render(frame, area),
            Self::Timer(s) => s.render(frame, area),
            Self::Notes(s) => s.render(frame, area),
            Self::Review(s) => s.render(frame, area),
            Self::Settings(s) => s.render(frame, area),
            Self::Audit(s) => s.render(frame, area),
            Self::Week(s) => s.render(frame, area),
            Self::Inbox(s) => s.render(frame, area),
        }
    }

    pub fn hints(&self, leader: bool, goto: bool, command: Option<&str>) -> Line<'static> {
        if let Some(buf) = command {
            // The command line renders its own four states (empty / partial /
            // unknown / executing) from the grammar table.
            return crate::app::command::render_line(buf);
        }
        if leader {
            return crate::ui::widgets::footer_hints(&[
                ("1", "home"),
                ("2", "books"),
                ("3", "progress"),
                ("t", "timer"),
                ("a", "+activity"),
                ("A", "activities"),
                ("R", "review"),
                ("n", "notes"),
                ("c", "+note"),
                ("s", "save"),
            ]);
        }
        if goto {
            // The `g`-goto menu: destinations plus `gg` = top of the list.
            return crate::ui::widgets::footer_hints(&[
                ("t", "timer"),
                ("p", "progress"),
                ("w", "week"),
                ("r", "review"),
                ("i", "inbox"),
                ("h", "home"),
                ("b", "books"),
                ("n", "notes"),
                ("a", "activities"),
                ("g", "top"),
            ]);
        }
        match self {
            Self::Login(s) => s.hints(),
            Self::Home(_) => crate::ui::widgets::footer_hints(&[
                ("r", "refresh"),
                ("a", "+activity"),
                ("g t", "timer"),
                ("g p", "progress"),
                ("g r", "review"),
                ("i", "inbox"),
                ("c", "+note"),
                (":", "cmd"),
                ("q", "quit"),
            ]),
            Self::Books(s) => s.hints(),
            Self::BookDetail(s) => s.hints(),
            Self::ActivityNew(s) => s.hints(),
            Self::Activities(s) => s.hints(),
            Self::Progress(s) => s.hints(),
            Self::Timer(s) => s.hints(),
            Self::Notes(s) => s.hints(),
            Self::Review(s) => s.hints(),
            Self::Settings(s) => s.hints(),
            Self::Audit(s) => s.hints(),
            Self::Week(s) => s.hints(),
            Self::Inbox(s) => s.hints(),
        }
    }
}
