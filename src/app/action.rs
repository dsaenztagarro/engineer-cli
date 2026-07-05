use crate::api::{
    Activity, Book, BookChapter, Note, Progress, Timer, TimerCandidate, TimerStopped,
};
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

    // Activities table (`src/app/screens/activities.rs`). The first surface to
    // expose `meta.page`: `ActivitiesLoaded` carries the page's rows plus the
    // pagination meta; mutations (complete/archive/duplicate) refetch the page
    // via `RefreshActivities` rather than patching a row in place.
    ActivitiesLoaded {
        items: Vec<Activity>,
        page: u32,
        per_page: u32,
        total: u32,
    },
    ActivitiesLoadFailed(String),
    ActivitiesMove(i32),
    ActivitiesJumpStart,
    ActivitiesJumpEnd,
    ActivitiesPageNext,
    ActivitiesPagePrev,
    ActivitiesCycleFilter,
    ActivitiesSearchInput(char),
    ActivitiesSearchBackspace,
    ActivitiesSearchSubmit,
    ActivitiesSearchCancel,
    ActivitiesOpenDetail,
    ActivitiesDetailLoaded(Box<Activity>),
    ActivitiesCloseDetail,
    ActivitiesComplete,
    ActivitiesArchive,
    ActivitiesDuplicate,
    ActivitiesStartTimer,
    RefreshActivities,

    // Notes browser (`src/app/screens/notes.rs`).
    NotesLoaded(Vec<Note>),
    NotesMove(i32),
    NotesJumpStart,
    NotesJumpEnd,
    NotesSearchInput(char),
    NotesSearchBackspace,
    NotesSearchSubmit,
    NotesSearchCancel,
    NotesOpenDetail,
    NotesDetailLoaded(Box<Note>),
    NotesCloseDetail,
    NotesToggleArchived,
    NotesArchiveSelected,
    NotesEditSelected,
    RefreshNotes,

    // Quick-capture overlay (`src/app/capture.rs`). Reachable from any screen
    // via the `<Space>` leader; also opened pre-filled to edit an existing note
    // from the browser. `CaptureOpen*`/`CaptureClose`/`CaptureSaved` are handled
    // by `App` (they create or drop `App::capture`); the rest are routed to the
    // live overlay reducer.
    CaptureOpen,
    CaptureOpenEdit(Box<Note>),
    CaptureClose,
    CaptureSaved,
    CaptureKey(crossterm::event::KeyEvent),
    CaptureSave,
    CaptureSaveFailed,
    CaptureCancel,
    CaptureFieldNext,
    CaptureFieldPrev,
    CaptureBookInput(char),
    CaptureBookBackspace,
    CaptureBookMove(i32),
    CaptureBookPickerSubmit,
    CaptureBookPickerClose,
    CaptureBookResults(Vec<Book>),

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
