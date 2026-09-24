//! Translates raw crossterm events into reducer `Action`s — the neovim-flavoured keymap.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::app::action::{Action, BooksFilter};
use crate::app::screens::{Screen, ScreenKind, ScreenMode};
use crate::app::App;

pub fn translate(app: &mut App, ev: Event) -> Option<Action> {
    let Event::Key(key) = ev else { return None };
    if key.kind != KeyEventKind::Press {
        return None;
    }

    if app.command_buffer.is_some() {
        return command_mode(app, key);
    }

    if let Some(cap) = app.capture.as_ref() {
        return cap.translate(key);
    }

    if matches!(app.current.mode(), ScreenMode::Insert) {
        return match key.code {
            KeyCode::Esc => Some(Action::ActivityLeaveInsert),
            _ => Some(Action::ActivityKey(key)),
        };
    }

    if let Some(action) = app.current.intercept_key(key) {
        return Some(action);
    }

    if app.notification.is_some() && matches!(key.code, KeyCode::Esc) {
        return Some(Action::DismissNotification);
    }

    if app.leader_pending {
        app.leader_pending = false;
        return leader(key);
    }

    if app.goto_pending {
        app.goto_pending = false;
        return goto(app, key);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char(' '), _) => {
            app.leader_pending = true;
            None
        }
        (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.goto_pending = true;
            None
        }
        (KeyCode::Char(':'), _) => {
            app.command_buffer = Some(String::new());
            Some(Action::CommandBegin)
        }
        (KeyCode::Char('q'), KeyModifiers::NONE) => {
            if app.current.kind() == ScreenKind::Queue {
                Some(Action::Goto(ScreenKind::Home))
            } else {
                Some(Action::Quit)
            }
        }
        (KeyCode::Char('?'), _) => Some(Action::Notify {
            level: crate::ui::notify::Level::Info,
            text: "help: j/k move · gg/G top/bottom · g t/p/r goto · / search · : command · <Space> leader · q quit".into(),
        }),
        (KeyCode::Char('r'), KeyModifiers::NONE) => Some(refresh_for(app.current.kind())),
        _ => screen_key(app, key),
    }
}

fn leader(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('1') => Some(Action::Goto(ScreenKind::Home)),
        KeyCode::Char('2') => Some(Action::Goto(ScreenKind::Books)),
        KeyCode::Char('3') => Some(Action::Goto(ScreenKind::Progress)),
        KeyCode::Char('t') => Some(Action::Goto(ScreenKind::Timer)),
        KeyCode::Char('a') => Some(Action::Goto(ScreenKind::ActivityNew)),
        KeyCode::Char('A') => Some(Action::Goto(ScreenKind::Activities)),
        KeyCode::Char('R') => Some(Action::Goto(ScreenKind::Review)),
        KeyCode::Char('b') => Some(Action::Goto(ScreenKind::Books)),
        KeyCode::Char('p') => Some(Action::Goto(ScreenKind::Progress)),
        KeyCode::Char('h') | KeyCode::Char('H') => Some(Action::Goto(ScreenKind::Home)),
        KeyCode::Char('s') => Some(Action::ActivitySubmit),
        KeyCode::Char('r') => Some(refresh_for_default()),
        KeyCode::Char('n') => Some(Action::Goto(ScreenKind::Notes)),
        KeyCode::Char('c') => Some(Action::CaptureOpen),
        _ => None,
    }
}

fn goto(app: &App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use ScreenKind::*;
    match key.code {
        KeyCode::Char('t') => Some(Action::Goto(Timer)),
        KeyCode::Char('p') => Some(Action::Goto(Progress)),
        KeyCode::Char('w') => Some(Action::Goto(Week)),
        KeyCode::Char('r') => Some(Action::Goto(Review)),
        KeyCode::Char('i') => Some(Action::Goto(Inbox)),
        KeyCode::Char('q') => Some(Action::Goto(Queue)),
        KeyCode::Char('h') => Some(Action::Goto(Home)),
        KeyCode::Char('b') => Some(Action::Goto(Books)),
        KeyCode::Char('n') => Some(Action::Goto(Notes)),
        KeyCode::Char('a') => Some(Action::Goto(Activities)),
        KeyCode::Char('g') => jump_start_for(app),
        _ => None,
    }
}

fn jump_start_for(app: &App) -> Option<Action> {
    use crate::app::screens::review::Stage;
    match app.current.kind() {
        ScreenKind::Books => Some(Action::BooksJumpStart),
        ScreenKind::Activities => Some(Action::ActivitiesJumpStart),
        ScreenKind::Notes => Some(Action::NotesJumpStart),
        ScreenKind::Review => match &app.current {
            Screen::Review(s) if matches!(s.stage(), Stage::Browse) => {
                Some(Action::ReviewBrowseJumpStart)
            }
            _ => None,
        },
        _ => None,
    }
}

fn screen_key(app: &mut App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use ScreenKind::*;
    match app.current.kind() {
        Login => login_key(key),
        Home => match key.code {
            KeyCode::Char('t') => Some(Action::Goto(Timer)),
            KeyCode::Char('a') => Some(Action::Goto(ActivityNew)),
            KeyCode::Char('A') => Some(Action::Goto(Activities)),
            KeyCode::Char('R') => Some(Action::Goto(Review)),
            KeyCode::Char('b') => Some(Action::Goto(Books)),
            KeyCode::Char('p') => Some(Action::Goto(Progress)),
            KeyCode::Char('n') => Some(Action::Goto(Notes)),
            KeyCode::Char('i') => Some(Action::Goto(Inbox)),
            KeyCode::Char('c') => Some(Action::CaptureOpen),
            _ => None,
        },
        Books => books_key(key),
        BookDetail => book_detail_key(key),
        ActivityNew => activity_normal_key(key),
        Activities => activities_key(key),
        Progress => progress_key(key),
        Week => week_key(key),
        Timer => timer_key(key),
        Notes => notes_key(key),
        Review => review_key(app, key),
        Inbox => inbox_key(app, key),
        Connect => connect_key(key),
        Queue => queue_key(key),
        Settings => match key.code {
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(Home)),
            _ => None,
        },
        Audit => audit_key(key),
    }
}

fn inbox_key(app: &App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use crate::app::screens::inbox::Stage;
    let Screen::Inbox(s) = &app.current else {
        return None;
    };
    match s.stage() {
        Stage::List => match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(Action::InboxMove(1)),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::InboxMove(-1)),
            KeyCode::Enter | KeyCode::Char('l') => Some(Action::InboxOpen),
            KeyCode::Char('x') => Some(Action::InboxRejectBegin),
            KeyCode::Char('a') => Some(Action::InboxAck),
            KeyCode::Char('c') => Some(Action::Goto(ScreenKind::Connect)),
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
            _ => None,
        },
        Stage::Draft => match key.code {
            KeyCode::Enter => Some(Action::InboxAccept),
            KeyCode::Char('J') | KeyCode::Char('j') | KeyCode::Down => {
                Some(Action::InboxDraftStep(1))
            }
            KeyCode::Char('K') | KeyCode::Char('k') | KeyCode::Up => {
                Some(Action::InboxDraftStep(-1))
            }
            KeyCode::Char('x') => Some(Action::InboxRejectBegin),
            KeyCode::Char('a') => Some(Action::InboxAck),
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::InboxCloseDetail),
            _ => None,
        },
    }
}

fn connect_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::ConnectMove(1)),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::ConnectMove(-1)),
        KeyCode::Char('c') => Some(Action::ConnectBegin),
        KeyCode::Char('d') => Some(Action::ConnectDisconnectBegin),
        KeyCode::Char('s') => Some(Action::ConnectSync),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Inbox)),
        _ => None,
    }
}

fn queue_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::QueueSelectMove(1)),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::QueueSelectMove(-1)),
        KeyCode::Char('x') => Some(Action::QueueDropSelected),
        KeyCode::Enter | KeyCode::Char('l') => Some(Action::QueueOpenReconcile),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn audit_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::AuditMove(1)),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::AuditMove(-1)),
        KeyCode::Char('a') => Some(Action::AuditAcknowledge),
        KeyCode::Char('t') => Some(Action::AuditTrim),
        KeyCode::Char('d') => Some(Action::AuditDelete),
        KeyCode::Char('f') => Some(Action::AuditFix),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Progress)),
        _ => None,
    }
}

fn review_key(app: &App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use crate::app::screens::review::Stage;
    let Screen::Review(s) = &app.current else {
        return None;
    };
    match s.stage() {
        Stage::Dashboard => match key.code {
            KeyCode::Enter | KeyCode::Char('s') => Some(Action::ReviewStartSitting),
            KeyCode::Char('b') => Some(Action::ReviewOpenBrowse),
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
            _ => None,
        },
        Stage::Browse => match (key.code, key.modifiers) {
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ReviewBrowseMove(1)),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ReviewBrowseMove(-1)),
            (KeyCode::Char('G'), _) => Some(Action::ReviewBrowseJumpEnd),
            (KeyCode::Enter, _) | (KeyCode::Char('l'), _) => Some(Action::ReviewBrowseOpenDetail),
            (KeyCode::Char('s'), _) => Some(Action::ReviewBrowseCycleSort),
            (KeyCode::Char(']'), _) => Some(Action::ReviewBrowsePageNext),
            (KeyCode::Char('['), _) => Some(Action::ReviewBrowsePagePrev),
            (KeyCode::Char('h'), _) | (KeyCode::Esc, _) => Some(Action::ReviewOpenDashboard),
            _ => None,
        },
        // The sitting's keys are the screen's `intercept_key`.
        Stage::Sitting => None,
    }
}

fn activities_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ActivitiesMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ActivitiesMove(-1)),
        (KeyCode::Char('G'), _) => Some(Action::ActivitiesJumpEnd),
        (KeyCode::Enter, _) | (KeyCode::Char('l'), _) => Some(Action::ActivitiesOpenDetail),
        (KeyCode::Char('c'), _) => Some(Action::ActivitiesComplete),
        (KeyCode::Char('a'), _) => Some(Action::ActivitiesArchive),
        (KeyCode::Char('d'), _) => Some(Action::ActivitiesDuplicate),
        (KeyCode::Char('t'), _) => Some(Action::ActivitiesStartTimer),
        (KeyCode::Char('f'), _) => Some(Action::ActivitiesCycleFilter),
        (KeyCode::Char(']'), _) => Some(Action::ActivitiesPageNext),
        (KeyCode::Char('['), _) => Some(Action::ActivitiesPagePrev),
        (KeyCode::Char('h'), _) => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn notes_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::NotesMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::NotesMove(-1)),
        (KeyCode::Char('G'), _) => Some(Action::NotesJumpEnd),
        (KeyCode::Enter, _) | (KeyCode::Char('l'), _) => Some(Action::NotesOpenDetail),
        (KeyCode::Char('a'), _) => Some(Action::NotesArchiveSelected),
        (KeyCode::Char('e'), _) => Some(Action::NotesEditSelected),
        (KeyCode::Char('t'), _) => Some(Action::NotesToggleArchived),
        (KeyCode::Char('c'), _) => Some(Action::CaptureOpen),
        (KeyCode::Char('h'), _) => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

/// Stage-agnostic: the screen's reducer validates each intent against its stage.
fn timer_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('s') => Some(Action::TimerSave),
        KeyCode::Char('p') => Some(Action::TimerPauseResume),
        // A legacy alias of `s`, kept for muscle memory.
        KeyCode::Char('x') => Some(Action::TimerStop),
        KeyCode::Char('d') => Some(Action::TimerDiscard),
        KeyCode::Char('i') => Some(Action::TimerToggleRail),
        KeyCode::Char('m') => Some(Action::TimerModeSwitch),
        KeyCode::Char('n') => Some(Action::TimerSkipInterval),
        KeyCode::Char('/') => Some(Action::TimerBindBegin),
        KeyCode::Char('b') => Some(Action::TimerBreak),
        KeyCode::Char('u') => Some(Action::TimerUndo),
        KeyCode::Enter => Some(Action::TimerDismissStopped),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn progress_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('a'), _) => Some(Action::Goto(ScreenKind::Audit)),
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ProgressSelectMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ProgressSelectMove(-1)),
        (KeyCode::Char('e'), _) => Some(Action::ProgressAdjustBegin),
        (KeyCode::Char('x'), _) => Some(Action::ProgressRetire),
        (KeyCode::Char('n'), _) => Some(Action::ProgressDeclareBegin),
        (KeyCode::Char('['), _) => Some(Action::ProgressWeekStep(-1)),
        (KeyCode::Char(']'), _) => Some(Action::ProgressWeekStep(1)),
        (KeyCode::Char('t'), _) => Some(Action::ProgressWeekReset),
        // Not `g`: that is the global goto prefix.
        (KeyCode::Tab, _) => Some(Action::ProgressFoldCycle),
        (KeyCode::Char('h'), _) | (KeyCode::Esc, _) => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn week_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::WeekSelectMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::WeekSelectMove(-1)),
        (KeyCode::Char('s'), _) => Some(Action::WeekStartTimer),
        (KeyCode::Char('a'), _) => Some(Action::WeekAddBegin),
        (KeyCode::Char('e'), _) => Some(Action::WeekAdjustBegin),
        (KeyCode::Char('d'), _) => Some(Action::WeekDrop),
        // `r` is the global refresh, so reflect is `i`.
        (KeyCode::Char('i'), _) => Some(Action::WeekReflect),
        (KeyCode::Char('['), _) => Some(Action::WeekStep(-1)),
        (KeyCode::Char(']'), _) => Some(Action::WeekStep(1)),
        (KeyCode::Char('t'), _) => Some(Action::WeekReset),
        (KeyCode::Char('h'), _) | (KeyCode::Esc, _) => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn login_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter | KeyCode::Char('l') => Some(Action::Login),
        _ => None,
    }
}

fn books_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::BooksMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::BooksMove(-1)),
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => Some(Action::BooksMove(10)),
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(Action::BooksMove(-10)),
        (KeyCode::Char('G'), _) => Some(Action::BooksJumpEnd),
        (KeyCode::Enter, _) | (KeyCode::Char('l'), _) => Some(Action::BooksOpen),
        (KeyCode::Char('h'), _) => Some(Action::Goto(ScreenKind::Home)),
        (KeyCode::Char('1'), _) => Some(Action::BooksFilter(BooksFilter::All)),
        (KeyCode::Char('2'), _) => Some(Action::BooksFilter(BooksFilter::Reading)),
        (KeyCode::Char('3'), _) => Some(Action::BooksFilter(BooksFilter::Completed)),
        _ => None,
    }
}

fn book_detail_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::ChapterMove(1)),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::ChapterMove(-1)),
        KeyCode::Char(' ') => Some(Action::ToggleChapterDone),
        KeyCode::Char('p') => Some(Action::BeginEditPage),
        KeyCode::Char('s') => Some(Action::BookStatusPicker),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Books)),
        _ => None,
    }
}

fn activity_normal_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => Some(Action::ActivityFieldNext),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => Some(Action::ActivityFieldPrev),
        KeyCode::Char('i') | KeyCode::Enter => Some(Action::ActivityEnterInsert),
        KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn command_mode(app: &mut App, key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => {
            app.command_buffer = None;
            Some(Action::CommandCancel)
        }
        KeyCode::Enter => Some(Action::CommandSubmit),
        KeyCode::Tab => {
            if let Some(b) = app.command_buffer.as_mut() {
                *b = crate::app::command::complete(b);
            }
            Some(Action::CommandInput)
        }
        KeyCode::Backspace => {
            if let Some(b) = app.command_buffer.as_mut() {
                b.pop();
            }
            Some(Action::CommandBackspace)
        }
        KeyCode::Char(c) => {
            if let Some(b) = app.command_buffer.as_mut() {
                b.push(c);
            }
            Some(Action::CommandInput)
        }
        _ => None,
    }
}

fn refresh_for(kind: ScreenKind) -> Action {
    match kind {
        ScreenKind::Progress => Action::RefreshProgress,
        ScreenKind::Week => Action::RefreshWeek,
        ScreenKind::Timer => Action::TimerReload,
        ScreenKind::Notes => Action::RefreshNotes,
        ScreenKind::Activities => Action::RefreshActivities,
        ScreenKind::Review => Action::RefreshReview,
        ScreenKind::Inbox => Action::RefreshInbox,
        ScreenKind::Connect => Action::RefreshConnect,
        ScreenKind::Settings => Action::SettingsReload,
        ScreenKind::Audit => Action::AuditReload,
        ScreenKind::Queue => Action::QueueRetry,
        _ => Action::RefreshHome,
    }
}

fn refresh_for_default() -> Action {
    Action::RefreshHome
}

#[allow(dead_code)]
fn _unused(_: &Screen) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiClient;
    use crate::config::{Config, Environment};
    use crate::ui::notify::{Level, Notification};
    use crossterm::event::KeyEvent;
    use std::time::Instant;

    fn app_on(kind: ScreenKind) -> App {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        App {
            config,
            api,
            user: None,
            current: Screen::new(kind),
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
            queue: None,
            queued_writes: 0,
            queue_diverged: false,
            settings: None,
            overrun_pinged: None,
            heartbeat_last: Instant::now(),
            pending_editor: None,
            reconnect_words: Vec::new(),
        }
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        translate(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn q_quits_everywhere_but_the_queue_inspector_where_it_steps_home() {
        let mut home = app_on(ScreenKind::Home);
        assert!(matches!(
            press(&mut home, KeyCode::Char('q')),
            Some(Action::Quit)
        ));
        let mut queue = app_on(ScreenKind::Queue);
        assert!(matches!(
            press(&mut queue, KeyCode::Char('q')),
            Some(Action::Goto(ScreenKind::Home))
        ));
    }

    #[test]
    fn esc_dismisses_a_notification_before_the_screen_sees_it() {
        let mut app = app_on(ScreenKind::Books);
        app.notification = Some(Notification::new(Level::Info, "saved"));
        assert!(matches!(
            press(&mut app, KeyCode::Esc),
            Some(Action::DismissNotification)
        ));
    }

    #[test]
    fn the_capture_overlay_owns_every_key_while_open() {
        let mut app = app_on(ScreenKind::Home);
        app.capture = Some(crate::app::capture::QuickCapture::new());
        assert!(!matches!(
            press(&mut app, KeyCode::Char('q')),
            Some(Action::Quit)
        ));
        press(&mut app, KeyCode::Char('g'));
        assert!(
            !app.goto_pending,
            "the global prefix never arms under the overlay"
        );
    }

    #[test]
    fn r_on_the_queue_inspector_retries_now_instead_of_rereading() {
        assert!(matches!(refresh_for(ScreenKind::Queue), Action::QueueRetry));
        assert!(matches!(refresh_for(ScreenKind::Home), Action::RefreshHome));
    }
}
