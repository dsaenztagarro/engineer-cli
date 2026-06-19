use crate::api::{Activity, Book, BookChapter};
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
