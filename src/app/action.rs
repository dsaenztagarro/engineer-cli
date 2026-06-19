use crate::api::{Activity, Book, BookChapter};
use crate::app::screens::ScreenKind;

/// Reducer-style messages dispatched by event handlers and async tasks.
#[derive(Debug)]
pub enum Action {
    Quit,
    Goto(ScreenKind),
    SetUser(String),
    FetchMe,
    Toast(String),

    // Auth
    Login,
    LoginSucceeded,
    LoginFailed(String),

    // Home
    HomeLoaded { today: Vec<Activity>, reading: Vec<Book> },
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
    BookDetailLoaded { book: Box<Book>, chapters: Vec<BookChapter> },
    ChapterMove(i32),
    ToggleChapterDone,
    BeginEditPage,
    EditPageInput(char),
    EditPageBackspace,
    SubmitPage,
    CancelEdit,
    BookStatusPicker,
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
    ActivityFailed { errors: Vec<crate::api::FieldError>, detail: String },

    // Command mode
    CommandBegin,
    CommandInput(char),
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
