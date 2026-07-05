//! Translates raw crossterm events into reducer `Action`s using a
//! minimalistic neovim keymap (j/k/gg/G, /, n/N, :cmd, <Space> leader,
//! i/Esc for insert/normal in forms).

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::app::action::{Action, BooksFilter};
use crate::app::screens::{Screen, ScreenKind, ScreenMode};
use crate::app::App;

pub fn translate(app: &mut App, ev: Event) -> Option<Action> {
    let Event::Key(key) = ev else { return None };
    if key.kind != KeyEventKind::Press {
        return None;
    }

    // Command mode (after `:`) captures everything until Enter/Esc.
    if app.command_buffer.is_some() {
        return command_mode(app, key);
    }

    // The quick-capture overlay is modal: while open it owns every key (from
    // any screen), so its draft is never disturbed by the global keymap.
    if let Some(cap) = app.capture.as_ref() {
        return cap.translate(key);
    }

    // Insert mode in forms: pass through to the screen.
    if matches!(app.current.mode(), ScreenMode::Insert) {
        return match key.code {
            KeyCode::Esc => Some(Action::ActivityLeaveInsert),
            _ => Some(Action::ActivityKey(key)),
        };
    }

    // Inline edits (search prompt, page edit) live in their own state on the screen.
    if let Some(action) = app.current.intercept_key(key) {
        return Some(action);
    }

    // Esc dismisses an active notification (command/insert/inline-edit modes
    // above already consumed Esc in their own contexts).
    if app.notification.is_some() && matches!(key.code, KeyCode::Esc) {
        return Some(Action::DismissNotification);
    }

    // Leader (Space) pending — second key picks the action.
    if app.leader_pending {
        app.leader_pending = false;
        return leader(key);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char(' '), _) => {
            app.leader_pending = true;
            None
        }
        (KeyCode::Char(':'), _) => {
            app.command_buffer = Some(String::new());
            Some(Action::CommandBegin)
        }
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(Action::Quit),
        (KeyCode::Char('?'), _) => Some(Action::Notify {
            level: crate::ui::notify::Level::Info,
            text: "help: j/k move · / search · : command · <Space> leader · q quit".into(),
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
        KeyCode::Char('b') => Some(Action::Goto(ScreenKind::Books)),
        KeyCode::Char('p') => Some(Action::Goto(ScreenKind::Progress)),
        KeyCode::Char('h') | KeyCode::Char('H') => Some(Action::Goto(ScreenKind::Home)),
        KeyCode::Char('s') => Some(Action::ActivitySubmit),
        KeyCode::Char('r') => Some(refresh_for_default()),
        // `n` browses notes; `c` captures one from anywhere (the sacred path).
        KeyCode::Char('n') => Some(Action::Goto(ScreenKind::Notes)),
        KeyCode::Char('c') => Some(Action::CaptureOpen),
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
            KeyCode::Char('b') => Some(Action::Goto(Books)),
            KeyCode::Char('p') => Some(Action::Goto(Progress)),
            KeyCode::Char('n') => Some(Action::Goto(Notes)),
            KeyCode::Char('c') => Some(Action::CaptureOpen),
            _ => None,
        },
        Books => books_key(key),
        BookDetail => book_detail_key(key),
        ActivityNew => activity_normal_key(key),
        Progress => progress_key(key),
        Timer => timer_key(key),
        Notes => notes_key(key),
    }
}

/// Notes-browser keys (search and the detail read own their keys via the
/// screen's `intercept_key`, which runs before this).
fn notes_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::NotesMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::NotesMove(-1)),
        (KeyCode::Char('g'), _) => Some(Action::NotesJumpStart),
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

/// Timer-screen keys (bind-panel keys are handled by the screen's
/// `intercept_key`, which runs before this). Intents are validated by the
/// screen's reducer against the current stage, so the map is stage-agnostic.
fn timer_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('s') => Some(Action::TimerStartBlank),
        KeyCode::Char('p') => Some(Action::TimerPauseResume),
        KeyCode::Char('x') => Some(Action::TimerStop),
        KeyCode::Char('d') => Some(Action::TimerDiscard),
        KeyCode::Char('/') | KeyCode::Char('b') => Some(Action::TimerBindBegin),
        KeyCode::Enter => Some(Action::TimerDismissStopped),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn progress_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match key.code {
        // `[` / `]` step weeks (a vim-ish prev/next idiom that avoids the
        // `h`-means-back convention the other screens use); `t` jumps to today.
        KeyCode::Char('[') => Some(Action::ProgressWeekStep(-1)),
        KeyCode::Char(']') => Some(Action::ProgressWeekStep(1)),
        KeyCode::Char('t') => Some(Action::ProgressWeekReset),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
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
        (KeyCode::Char('g'), _) => Some(Action::BooksJumpStart),
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
        ScreenKind::Timer => Action::TimerReload,
        ScreenKind::Notes => Action::RefreshNotes,
        _ => Action::RefreshHome,
    }
}

fn refresh_for_default() -> Action {
    Action::RefreshHome
}

#[allow(dead_code)]
fn _unused(_: &Screen) {}
