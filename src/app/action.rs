use crate::api::{Activity, Book, BookChapter, Progress, Timer, TimerCandidate, TimerStopped};
use crate::app::screens::ScreenKind;
use crate::ui::notify::Level;

/// Reducer-style messages dispatched by event handlers and async tasks.
#[derive(Debug)]
pub enum Action {
    Quit,
    Goto(ScreenKind),
    SetUser(String),
    FetchMe,
    Notify {
        level: Level,
        text: String,
    },
    DismissNotification,

    // Auth
    Login,
    LoginSucceeded,
    LoginFailed(String),

    // Home
    HomeLoaded {
        today: Vec<Activity>,
        reading: Vec<Book>,
    },
    RefreshHome,

    // Books
    BooksLoaded(Vec<Book>),
    BooksFilter(BooksFilter),
    BooksSearchInput(char),
    BooksSearchBackspace,
    BooksSearchSubmit,
    BooksSearchCancel,
    BooksMove(i32),
    BooksJumpStart,
    BooksJumpEnd,
    BooksOpen,

    // Book detail
    BookDetailLoaded {
        book: Box<Book>,
        chapters: Vec<BookChapter>,
    },
    ChapterMove(i32),
    ToggleChapterDone,
    BeginEditPage,
    EditPageInput(char),
    EditPageBackspace,
    SubmitPage,
    CancelEdit,
    BookStatusPicker,
    // Handled by BookDetail but not yet emitted (status picker is a TODO).
    #[allow(dead_code)]
    PickStatus(crate::api::BookStatus),
    BookUpdated(Box<Book>),

    // Activity new
    ActivityFieldNext,
    ActivityFieldPrev,
    ActivityEnterInsert,
    ActivityLeaveInsert,
    ActivityKey(crossterm::event::KeyEvent),
    ActivitySubmit,
    ActivityCreated,
    ActivityFailed {
        errors: Vec<crate::api::FieldError>,
        detail: String,
    },

    // Progress
    ProgressLoaded(Box<Progress>),
    ProgressLoadFailed(String),
    ProgressWeekStep(i32),
    ProgressWeekReset,
    RefreshProgress,

    // Timer — the header cell (app-owned snapshot) and the Timer screen.
    // `RefreshTimer` re-polls the header snapshot; `TimerLoaded` updates the
    // app snapshot and is forwarded to the Timer screen; `TimerCleared` wipes
    // the header snapshot without disturbing the screen (used after stop, so the
    // segment confirmation survives). The rest are Timer-screen intents.
    RefreshTimer,
    TimerLoaded(Box<Timer>),
    TimerCleared,
    TimerReload,
    TimerStartBlank,
    TimerPauseResume,
    TimerStop,
    TimerStopped(Box<TimerStopped>),
    TimerDismissStopped,
    TimerDiscard,
    TimerBindBegin,
    TimerBindCancel,
    TimerBindInput(char),
    TimerBindBackspace,
    TimerBindMove(i32),
    TimerBindSubmit,
    TimerCandidatesLoaded(Vec<TimerCandidate>),

    // Command mode. The buffer is mutated in the event layer; these are signals.
    CommandBegin,
    CommandInput,
    CommandBackspace,
    CommandSubmit,
    CommandCancel,
}

#[derive(Debug, Clone, Copy)]
pub enum BooksFilter {
    All,
    Reading,
    Completed,
}
