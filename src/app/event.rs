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

    // Goto (`g`) pending — the second key picks a destination (`g t`/`g p`/…) or,
    // on a second `g`, the current list's top motion (`gg`). Mirrors the leader.
    if app.goto_pending {
        app.goto_pending = false;
        return goto(app, key);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char(' '), _) => {
            app.leader_pending = true;
            None
        }
        // `g` is a prefix (vim `gg`/`gt`/…), so it never acts on its own — the
        // next key resolves it. This means single-`g` list motions became `gg`.
        (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.goto_pending = true;
            None
        }
        (KeyCode::Char(':'), _) => {
            app.command_buffer = Some(String::new());
            Some(Action::CommandBegin)
        }
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(Action::Quit),
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
        // `A` (capital) opens the activities table; `a` stays "+activity" (the
        // new-activity form) so muscle memory is preserved.
        KeyCode::Char('A') => Some(Action::Goto(ScreenKind::Activities)),
        // `R` opens the review screen; `r` stays the global refresh key.
        KeyCode::Char('R') => Some(Action::Goto(ScreenKind::Review)),
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

/// The `g`-goto prefix's second key: a destination (`g t`/`g p`/…), or `gg` for
/// the current list's top motion. Kept beside `leader` so the two prefixes read
/// alike. One goto grammar on every screen; single letters stay free for
/// screen-local actions.
fn goto(app: &App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use ScreenKind::*;
    match key.code {
        KeyCode::Char('t') => Some(Action::Goto(Timer)),
        KeyCode::Char('p') => Some(Action::Goto(Progress)),
        KeyCode::Char('w') => Some(Action::Goto(Week)),
        KeyCode::Char('r') => Some(Action::Goto(Review)),
        KeyCode::Char('h') => Some(Action::Goto(Home)),
        KeyCode::Char('b') => Some(Action::Goto(Books)),
        KeyCode::Char('n') => Some(Action::Goto(Notes)),
        KeyCode::Char('a') => Some(Action::Goto(Activities)),
        // `gg` — the vim top motion the single-`g` handlers served before `g`
        // became the goto prefix.
        KeyCode::Char('g') => jump_start_for(app),
        _ => None,
    }
}

/// The current screen's "jump to top" motion, dispatched by `gg`. Screens with
/// no scrollable list return `None`.
fn jump_start_for(app: &App) -> Option<Action> {
    use crate::app::screens::review::Stage;
    match app.current.kind() {
        ScreenKind::Books => Some(Action::BooksJumpStart),
        ScreenKind::Activities => Some(Action::ActivitiesJumpStart),
        ScreenKind::Notes => Some(Action::NotesJumpStart),
        // Review's list lives in the Browse stage only.
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
        Settings => match key.code {
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(Home)),
            _ => None,
        },
        Audit => audit_key(key),
    }
}

/// Segment-audit keys (§Segment audit): the row actions, plus `h`/Esc back to
/// Progress (the tab it belongs to).
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

/// Review-screen keys for the two non-modal base stages (dashboard + browse
/// list). The rating contexts — the sitting and the browse detail read — and
/// the browse search prompt own their keys via the screen's `intercept_key`,
/// which runs before this; those keys never reach here.
fn review_key(app: &App, key: crossterm::event::KeyEvent) -> Option<Action> {
    use crate::app::screens::review::Stage;
    let Screen::Review(s) = &app.current else {
        return None;
    };
    match s.stage() {
        Stage::Dashboard => match key.code {
            // Enter or `s` starts the sitting at the queue head.
            KeyCode::Enter | KeyCode::Char('s') => Some(Action::ReviewStartSitting),
            KeyCode::Char('b') => Some(Action::ReviewOpenBrowse),
            KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
            _ => None,
        },
        Stage::Browse => match (key.code, key.modifiers) {
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ReviewBrowseMove(1)),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ReviewBrowseMove(-1)),
            // `gg` jumps to the top (handled by the global goto prefix); `G` to the end.
            (KeyCode::Char('G'), _) => Some(Action::ReviewBrowseJumpEnd),
            (KeyCode::Enter, _) | (KeyCode::Char('l'), _) => Some(Action::ReviewBrowseOpenDetail),
            // `s` cycles the sort ring (the #11 `f`-ring precedent).
            (KeyCode::Char('s'), _) => Some(Action::ReviewBrowseCycleSort),
            (KeyCode::Char(']'), _) => Some(Action::ReviewBrowsePageNext),
            (KeyCode::Char('['), _) => Some(Action::ReviewBrowsePagePrev),
            // `h`/Esc steps back to the dashboard, not out of the screen.
            (KeyCode::Char('h'), _) | (KeyCode::Esc, _) => Some(Action::ReviewOpenDashboard),
            _ => None,
        },
        // The sitting's keys (f/z/s/i rate, Esc exit) are handled in intercept_key.
        Stage::Sitting => None,
    }
}

/// Activities-table keys (search and the detail read own their keys via the
/// screen's `intercept_key`, which runs before this). `[`/`]` step pages —
/// consistent with the Progress screen's week nav — and `t` binds the live
/// timer to the selected activity.
fn activities_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ActivitiesMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ActivitiesMove(-1)),
        // `gg` jumps to the top (handled by the global goto prefix); `G` to the end.
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

/// Notes-browser keys (search and the detail read own their keys via the
/// screen's `intercept_key`, which runs before this).
fn notes_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::NotesMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::NotesMove(-1)),
        // `gg` jumps to the top (handled by the global goto prefix); `G` to the end.
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
        KeyCode::Char('s') => Some(Action::TimerSave),
        KeyCode::Char('p') => Some(Action::TimerPauseResume),
        // Legacy alias of `s` end & save, kept for muscle memory.
        KeyCode::Char('x') => Some(Action::TimerStop),
        KeyCode::Char('d') => Some(Action::TimerDiscard),
        KeyCode::Char('i') => Some(Action::TimerToggleRail),
        KeyCode::Char('m') => Some(Action::TimerModeSwitch),
        KeyCode::Char('n') => Some(Action::TimerSkipInterval),
        KeyCode::Char('/') => Some(Action::TimerBindBegin),
        // Focus: the phase toggle. Stopwatch: the bind/picker alias.
        KeyCode::Char('b') => Some(Action::TimerBreak),
        KeyCode::Char('u') => Some(Action::TimerUndo),
        KeyCode::Enter => Some(Action::TimerDismissStopped),
        KeyCode::Char('h') | KeyCode::Esc => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

fn progress_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        // The audit subtab (§Segment audit).
        (KeyCode::Char('a'), _) => Some(Action::Goto(ScreenKind::Audit)),
        // j/k move the target-row cursor; `e` adjusts the selected target's
        // weekly hours in place; `x` retires it (confirmed on a second press).
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::ProgressSelectMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::ProgressSelectMove(-1)),
        (KeyCode::Char('e'), _) => Some(Action::ProgressAdjustBegin),
        (KeyCode::Char('x'), _) => Some(Action::ProgressRetire),
        // `n` declares a new target — a fuzzy scope pick, then hours.
        (KeyCode::Char('n'), _) => Some(Action::ProgressDeclareBegin),
        // `[` / `]` step weeks (a vim-ish prev/next idiom that avoids the
        // `h`-means-back convention the other screens use); `t` jumps to today.
        (KeyCode::Char('['), _) => Some(Action::ProgressWeekStep(-1)),
        (KeyCode::Char(']'), _) => Some(Action::ProgressWeekStep(1)),
        (KeyCode::Char('t'), _) => Some(Action::ProgressWeekReset),
        (KeyCode::Char('h'), _) | (KeyCode::Esc, _) => Some(Action::Goto(ScreenKind::Home)),
        _ => None,
    }
}

/// Week-board keys (§Week · board / §Week · add an intent). `j`/`k` move the
/// full-row cursor over the plan rows; `a` declares a new intent, `e` adjusts
/// the selected one, `d` drops it (confirmed on a second press) — the plan-write
/// gestures (#115); `s` starts the timer bound to the selected item's activity
/// (the seam, #116). The one-line input, while open, owns keys via the screen's
/// `intercept_key`, so those never reach here. `[`/`]`/`t` step the week in the
/// same dialect as Progress; `h`/Esc steps home.
fn week_key(key: crossterm::event::KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Some(Action::WeekSelectMove(1)),
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Some(Action::WeekSelectMove(-1)),
        (KeyCode::Char('s'), _) => Some(Action::WeekStartTimer),
        (KeyCode::Char('a'), _) => Some(Action::WeekAddBegin),
        (KeyCode::Char('e'), _) => Some(Action::WeekAdjustBegin),
        (KeyCode::Char('d'), _) => Some(Action::WeekDrop),
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
        // `gg` jumps to the top (handled by the global goto prefix); `G` to the end.
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
        // Tab completes the current verb (or timer sub-verb) toward the longest
        // common prefix of the matches, per the grammar table.
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
        ScreenKind::Settings => Action::SettingsReload,
        ScreenKind::Audit => Action::AuditReload,
        _ => Action::RefreshHome,
    }
}

fn refresh_for_default() -> Action {
    Action::RefreshHome
}

#[allow(dead_code)]
fn _unused(_: &Screen) {}
