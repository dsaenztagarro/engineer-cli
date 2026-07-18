use crate::api::{
    Activity, AnchorData, AuditAcknowledged, AuditRead, Book, BookChapter, CaptureSource,
    Dashboard, DayMinutes, Domain, Note, Progress, RateResult, Task, Timer, TimerCandidate,
    TimerSettings, TimerStopped, Today, Topic, Week,
};
use crate::app::screens::review::Rating;
use crate::app::screens::ScreenKind;
use crate::queue::{Intent, ReplayReport};
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
    /// The pre-flight `discover()` couldn't reach the identity server (auth
    /// unreachable / 5xx) — the sign-in flow can't even start. Renders the
    /// Tier-3 blocking screen (§SIGN IN · SERVER ERROR), `⏎` to retry.
    LoginServerError(String),
    /// A 401 from any authenticated read/write — the stored session is no longer
    /// valid. Routes to Login in re-auth mode (Tier 3, `⏎` to sign in). The one
    /// cross-cutting behavioural addition of the error-model epic.
    SessionExpired,

    // Home
    /// The composed daily-loop aggregate (`GET /api/v1/today`) — the whole Home
    /// screen from one read. Boxed: `Today` is large next to the other variants.
    TodayLoaded(Box<Today>),
    /// The `today()` load failed; clears Home's spinner (the error is surfaced
    /// as a notification tile from the load task).
    HomeLoadFailed,
    RefreshHome,
    /// The ambient pending-drafts count for Home's inbox chip (§Inbox · the
    /// ambient count). Loaded by a light `list_pending_tasks()` fetch on Home
    /// load — the `/today` aggregate doesn't carry a drafts count, and the CLI
    /// invents no server endpoint. `expiring` is set when a draft is near its
    /// expiry (the chip's amber escalation). A failed fetch stays silent.
    HomeInboxLoaded {
        pending: usize,
        expiring: bool,
    },

    // Books
    BooksLoaded(Vec<Book>),
    /// The books read failed — carries the Tier-2 reason line (built from
    /// `messages::fail_reason`). Replaces the old `BooksLoaded(vec![])`-on-error
    /// that made a failure indistinguishable from an empty shelf.
    BooksLoadFailed(String),
    BooksFilter(BooksFilter),
    BooksSearchInput(char),
    BooksSearchBackspace,
    BooksSearchSubmit,
    BooksSearchCancel,
    BooksMove(i32),
    /// `n`/`N` — step to the next/previous loaded row matching the live query
    /// (client-side; `/` still owns the server re-query). Inert with no query.
    BooksMatchStep(i32),
    BooksJumpStart,
    BooksJumpEnd,
    BooksOpen,
    /// `f` — open a fuzzy picker over the loaded books; while open it owns keys,
    /// routed through `BooksPickerKey`.
    BooksPickerOpen,
    BooksPickerKey(crossterm::event::KeyEvent),

    // Book detail
    BookDetailLoaded {
        book: Box<Book>,
        chapters: Vec<BookChapter>,
    },
    /// The book detail read (its chapters) failed — carries the Tier-2 reason
    /// line (built from `messages::fail_reason`). Replaces the old
    /// `BookDetailLoaded { chapters: vec![] }`-on-error that made a failed
    /// chapters fetch indistinguishable from a book with no chapters. Dispatched
    /// from `Books::BooksOpen`'s spawned task; the detail renders the whole-body
    /// failed panel since without the payload there's nothing else to show.
    BookDetailLoadFailed(String),
    ChapterMove(i32),
    ToggleChapterDone,
    BeginEditPage,
    EditPageInput(char),
    EditPageBackspace,
    SubmitPage,
    CancelEdit,
    // Status picker modal (opened with `s` on BookDetail). `BookStatusSelect`
    // moves the highlight to a status (the `r/c/u/h/a` mnemonics); `j/k` step it
    // via `BookStatusMove`; `BookStatusConfirm` PATCHes the book; `Esc` cancels.
    BookStatusPicker,
    BookStatusMove(i32),
    BookStatusSelect(crate::api::BookStatus),
    BookStatusConfirm,
    BookStatusCancel,
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
    // `Tab` cycles the "where it went" fold through its facets (by kind → by
    // domain → by intent) — a muted glance, not a pivot grid (#122).
    ProgressFoldCycle,
    // Target editing on the Progress screen. `e` edits the selected target's
    // weekly hours inline; `x` retires it (armed, confirmed on a second press).
    // Every target write (declare/adjust/retire) routes through `QueuedClient`,
    // so an offline gesture queues and replays like the timer/week writes (#121).
    ProgressSelectMove(i32),
    ProgressAdjustBegin,
    ProgressAdjustInput(char),
    ProgressAdjustBackspace,
    ProgressAdjustSubmit,
    ProgressAdjustCancel,
    ProgressRetire,
    // Declare a new target on the Progress screen (`n`) — riding the fuzzy picker.
    // `Begin` fetches domains; `Ready` opens the scope picker over domains + the
    // kind/intent enums; `Key` routes every key while the flow is open; `Queued`
    // records an offline declare so the screen renders it `◔ … queued` until it
    // syncs (the Week board's `WeekPlanQueued` twin).
    ProgressDeclareBegin,
    ProgressDeclareReady(Vec<Domain>),
    ProgressDeclareKey(crossterm::event::KeyEvent),
    ProgressDeclareQueued(String),

    // Timer — the header cell (app-owned snapshot) and the Timer screen.
    // `RefreshTimer` re-polls the header snapshot; `TimerLoaded` updates the
    // app snapshot and is forwarded to the Timer screen; `TimerCleared` wipes
    // the header snapshot without disturbing the screen (used after stop, so the
    // segment confirmation survives). The rest are Timer-screen intents.
    RefreshTimer,
    TimerLoaded(Box<Timer>),
    /// The header poll hit a transport failure and fell back offline: the
    /// *effective* local timer (cached snapshot ⊕ pending queue, via
    /// `queue::fold_timer`). Header-only — unlike `TimerLoaded` it is not
    /// forwarded to the Timer screen, which keeps its own last live snapshot.
    TimerStale(Box<Timer>),
    /// An offline write landed in the queue: the synthesized local clock a verb
    /// returns provisionally (`◔`). Both the app snapshot and the Timer screen
    /// take it — the screen flips its provisional marker on, unlike `TimerStale`.
    TimerProvisional(Box<Timer>),
    /// The reconnect drain acknowledged one queued intent — its verb word,
    /// streamed into the ambient replay transcript as it lands (`back online ·
    /// replaying the queue…`). One per acknowledged intent; the reducer appends
    /// it to the one-line status. Reuses the `TimerProvisional`-style plumbing:
    /// the spawned drain task streams these back into the reducer.
    ReplayProgress {
        word: String,
    },
    /// The reconnect drain finished — the [`ReplayReport`] the synced tile reads.
    /// A clean pass that replayed ≥1 lands `✓ synced — N queued writes
    /// reconciled` (auto-dismissing on the notify TTL); an empty pass or one
    /// halted by divergence shows nothing here (the diverged markers stand; the
    /// reconcile panel is #106).
    ReplayFinished(ReplayReport),
    /// The queue's first diverged intent, payload and all — `Some` opens (or
    /// refreshes) the Timer screen's reconcile panel, `None` closes a stale
    /// one (the divergence was resolved elsewhere, e.g. headlessly). Loaded by
    /// a spawned queue read after each snapshot lands, so the panel follows
    /// the queue file — the single source of truth — like every other surface.
    TimerDivergedLoaded(Option<Box<Intent>>),
    /// `b` on the reconcile panel — keep both: the local session is written
    /// via `create_segment` and the server session stands.
    TimerReconcileBoth,
    /// `e` on the reconcile panel's rejected-write face (#109) — open the
    /// intent's payload in `$EDITOR`. The screen builds the seed and hands it
    /// up via [`Action::QueueIntentEdit`]; the run loop owns the terminal.
    TimerReconcileEdit,
    /// The `$EDITOR` hand-off finished with a saved buffer — parse it back,
    /// re-pend the intent, and retry the drain (`queue::apply_edit`).
    TimerReconcileEditApply {
        intent_id: u64,
        buffer: String,
    },
    /// `x` on the reconcile panel's rejected-write face — drop the intent.
    /// First press arms the confirm; only the very next `x` goes through
    /// (the explicit, confirmed delete — never silent).
    TimerReconcileDrop,
    /// `s` on the reconcile panel's rejected-write face — skip: park the
    /// intent (reason `skipped`), kept in the queue, out of the replay line.
    TimerReconcileSkip,
    /// Stash a queue intent's editable payload for the run loop's `$EDITOR`
    /// hand-off (the `git commit` pattern the capture overlay and the week
    /// retro already ride). The saved buffer comes back as
    /// [`Action::TimerReconcileEditApply`].
    QueueIntentEdit {
        intent_id: u64,
        seed: String,
    },
    TimerCleared,
    TimerReload,
    /// The `s` key — stage-dependent primary: starts the clock when absent,
    /// ends & saves when live and bound, warns when live and unbound.
    TimerSave,
    TimerPauseResume,
    TimerStop,
    /// `i` — fold/unfold the instrument rail on the watch face.
    TimerToggleRail,
    /// `m` — stopwatch ⇄ focus on the running timer; warns until the focus
    /// API ships.
    TimerModeSwitch,
    /// `n` — bank the interval and arm the next work phase.
    TimerSkipInterval,
    /// `b` — the focus phase toggle (break now / back to work); keeps its
    /// bind meaning in stopwatch mode.
    TimerBreak,
    /// Today's logged minutes for the rail (summed from today's activities).
    TimerTodayLoaded(u32),
    /// The week's per-day minutes (mon→sun) for the rail's sparkline.
    TimerWeekLoaded(Vec<DayMinutes>),
    TimerStopped(Box<TimerStopped>),
    TimerDismissStopped,
    TimerDiscard,
    TimerBindBegin,
    TimerBindCancel,
    /// `Tab` in the start picker — stopwatch ⇄ focus for the fresh start.
    TimerPickerToggleMode,
    /// `u` on the stop confirmation — delete the just-written segment.
    TimerUndo,
    /// The undo landed: the segment is gone, reload into the empty face.
    TimerUndone,

    // Timer settings — the view-only knobs screen (`:settings`).
    SettingsLoaded(Box<TimerSettings>),
    SettingsReload,

    // Week board (`src/app/screens/week.rs`, `:week`, `g w`) — the planned-vs-
    // done readout for one ISO week. Stepping refetches; the cursor moves over
    // the plan rows the plan-write gestures act on.
    WeekLoaded(Box<Week>),
    WeekLoadFailed(String),
    WeekStep(i32),
    WeekReset,
    RefreshWeek,
    WeekSelectMove(i32),
    // Plan writes from the board (#115). `a` opens the one-line intent input,
    // `e` adjusts the selected row's title (both route through `WeekInput*`);
    // `d` drops the selected row (armed, confirmed on a second press). Each
    // write goes through `QueuedClient`, so an offline gesture queues and the
    // board renders it provisionally.
    WeekAddBegin,
    WeekAdjustBegin,
    WeekInputChar(char),
    WeekInputBackspace,
    WeekInputSubmit,
    WeekInputCancel,
    WeekDrop,
    /// `s` — the Plan↔timer seam (#116): start (or stop & switch) the timer
    /// bound to the selected plan item's activity. Nothing running starts it
    /// outright; a timer already elsewhere warns first (naming the running
    /// session) then switches on the second press; a still-queued row (an
    /// offline declare the server hasn't minted) refuses. Reuses
    /// `QueuedClient::start_timer` — the verb, not the Timer screen.
    WeekStartTimer,
    /// An offline declare landed in the queue: its title, for the provisional
    /// `◔ … queued` row the board renders until the create replays.
    WeekPlanQueued(String),
    /// `i` — the retro reflection (#117): the week board reads its current note
    /// body and dispatches `WeekReflectEdit` to open `$EDITOR` (the git-commit
    /// pattern). No-op until the week has loaded.
    WeekReflect,
    /// App-level: stash the seeded note body for the run loop to open in
    /// `$EDITOR`, tagged to persist back to `iso_week`'s note. Mirrors
    /// `CaptureEditExternal` — the terminal-owned suspend/spawn is the run loop's.
    WeekReflectEdit {
        iso_week: String,
        seed: String,
    },
    /// The editor saved: persist the reflection through `QueuedClient`. An empty
    /// `body` clears the note deliberately (the server treats empty as clear).
    WeekReflectSave {
        iso_week: String,
        body: String,
    },
    /// The editor aborted (quit-without-write): keep the stored note untouched
    /// (capture-is-sacred across the boundary) — the board only says so.
    WeekReflectAbort,
    /// An offline reflection landed in the queue: the written body, for the retro
    /// band's `◔ queued` render until the note write replays.
    WeekReflectQueued(String),

    // Segment audit (`Progress ▸ audit`, `:audit`).
    AuditLoaded(Box<AuditRead>),
    /// The audit read failed — carries the Tier-2 reason line (built from
    /// `messages::fail_reason`). Replaces the old swallow (a bare notify + no
    /// state change) that left a failed read looking like a clean, empty log.
    AuditLoadFailed(String),
    AuditReload,
    AuditMove(i32),
    /// `a` — looks right: acknowledge, clearing the duration flags for good.
    AuditAcknowledge,
    AuditAcknowledged(Box<AuditAcknowledged>),
    /// `t` — the trim preset: PATCH the duration down to the long fence.
    AuditTrim,
    /// `d` — delete the segment (asks twice).
    AuditDelete,
    /// `f` — route to the activity edit.
    AuditFix,
    TimerBindInput(char),
    TimerBindBackspace,
    TimerBindMove(i32),
    TimerBindSubmit,
    TimerCandidatesLoaded(Vec<TimerCandidate>),

    // Activities table (`src/app/screens/activities.rs`). The first surface to
    // expose `meta.page`: `ActivitiesLoaded` carries the page's rows plus the
    // pagination meta; mutations (complete/archive/duplicate) refetch the page
    // via `RefreshActivities` rather than patching a row in place. The rows
    // arrive already folded with the pending queue (`queue::fold_activities`,
    // #109) — still-queued creates render `◔ … provisional · queued` mixed
    // with the confirmed, and queued segment minutes ride their parent row.
    ActivitiesLoaded {
        items: Vec<crate::queue::FoldedActivity>,
        page: u32,
        per_page: u32,
        total: u32,
    },
    ActivitiesLoadFailed(String),
    ActivitiesMove(i32),
    /// `n`/`N` — step to the next/previous *visible* row matching the live
    /// client-side query (the `/` filter still narrows the page). Inert with no
    /// query.
    ActivitiesMatchStep(i32),
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
    /// The notes read failed — carries the Tier-2 reason line (built from
    /// `messages::fail_reason`). Replaces the old `NotesLoaded(vec![])`-on-error
    /// that made a failure indistinguishable from an empty ledger.
    NotesLoadFailed(String),
    NotesMove(i32),
    /// `n`/`N` — step to the next/previous loaded row matching the live query
    /// (client-side; `/` still owns the server re-query). Inert with no query.
    NotesMatchStep(i32),
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
    /// Detach the open note from its book (`unlink_note`) — a book-detach, not
    /// an archive; the note survives (notes.dc.html §Note detail, `u`).
    NotesUnlinkSelected,
    /// The guarded permanent delete on the detail (`delete_note`): the first
    /// press arms ("delete (permanent)"), a second confirms, any other key
    /// disarms. Live-only — an offline delete refuses honestly.
    NotesDeleteArm,
    NotesDeleteConfirm,
    NotesDeleteDisarm,
    RefreshNotes,

    // Review (`src/app/screens/review.rs`) — one screen, three stages: the
    // dashboard read, the rating sitting (queue head → rate → next until the
    // queue drains), and a secondary browse-all state. `ReviewRate` carries one
    // of the four ratings as a single keystroke; `ReviewRated` is the async
    // server result (advances the sitting, or refetches the browse page).
    ReviewDashboardLoaded(Box<Dashboard>),
    ReviewLoadFailed(String),
    RefreshReview,
    ReviewOpenDashboard,
    ReviewOpenBrowse,
    ReviewStartSitting,
    ReviewExitSitting,
    ReviewRate(Rating),
    ReviewRated(Box<RateResult>),
    ReviewRateFailed,
    ReviewBrowseLoaded {
        items: Vec<Topic>,
        page: u32,
        per_page: u32,
        total: u32,
    },
    ReviewBrowseMove(i32),
    ReviewBrowseJumpStart,
    ReviewBrowseJumpEnd,
    ReviewBrowsePageNext,
    ReviewBrowsePagePrev,
    ReviewBrowseCycleSort,
    ReviewBrowseSearchInput(char),
    ReviewBrowseSearchBackspace,
    ReviewBrowseSearchSubmit,
    ReviewBrowseSearchCancel,
    ReviewBrowseOpenDetail,
    ReviewBrowseDetailLoaded(Box<Topic>),
    ReviewBrowseCloseDetail,

    // Inbox — the draft-triage screen (`src/app/screens/inbox.rs`, `:inbox`,
    // `g i`) over the assisted-capture automations. `InboxLoaded` carries the
    // pending drafts (sorted expiring-first on receipt); the verbs
    // (accept/reject/ack) are LIVE-ONLY fire-then-re-read calls — not queued —
    // so each spawns a server PATCH and, on a resolved outcome, dispatches
    // `RefreshInbox` (the draft left the scope). A stale `422` is a soft
    // re-read; a transport failure refuses (offline) via `InboxActionFailed`,
    // which clears the in-flight guard without a re-read.
    InboxLoaded(Vec<Task>),
    InboxLoadFailed(String),
    RefreshInbox,
    InboxMove(i32),
    /// `⏎`/`l` on the list — open the selected draft's detail.
    InboxOpen,
    /// `h`/`Esc` on the detail — back to the pending list.
    InboxCloseDetail,
    /// `J`/`K` on the detail — step to the next/previous pending draft.
    InboxDraftStep(i32),
    /// `⏎` on the detail — accept (the server's `complete`, which mints the
    /// activity).
    InboxAccept,
    /// `a` — acknowledge (seen, keep for later).
    InboxAck,
    /// `x` — open the optional-reason capture (§Inbox · reject).
    InboxRejectBegin,
    InboxRejectInput(char),
    InboxRejectBackspace,
    /// `⏎` in the reason capture — reject with the typed reason (bare if empty).
    InboxRejectSubmit,
    /// `Esc` in the reason capture — cancel the reject (no server call).
    InboxRejectCancel,
    /// A live-only verb failed without a re-read (offline / server rejection) —
    /// clears the in-flight guard so the gesture can be retried.
    InboxActionFailed,

    // Connect — the git-source connect flow (`src/app/screens/connect.rs`),
    // reachable from the Inbox screen via `c` (the design's §Connect · git
    // source). The capture sources (git / calendar) with their connect state,
    // the plain-language trust statement rendered verbatim before connecting,
    // and the honest requirement pointer when GitHub isn't connected. The verbs
    // (connect / disconnect / sync) are LIVE-ONLY — the same epic deviation as
    // the triage verbs (#94): connecting needs the server, so an offline gesture
    // refuses honestly rather than synthesizing an opt-in that never happened.
    ConnectLoaded(Vec<CaptureSource>),
    ConnectLoadFailed(String),
    RefreshConnect,
    ConnectMove(i32),
    /// `c` — open the trust/confirm prompt for the selected source (or the
    /// requirement pointer when it isn't connectable).
    ConnectBegin,
    /// `d` — arm the disconnect confirm for the selected (connected) source.
    ConnectDisconnectBegin,
    /// `s` — enqueue a scan for the selected connected source.
    ConnectSync,
    /// `y`/`⏎` — proceed with whatever prompt is open (connect or disconnect).
    ConnectPromptSubmit,
    /// `Esc`/`n` — dismiss the open prompt without a server call.
    ConnectPromptCancel,
    /// Feed-URL capture keys (calendar connect only).
    ConnectFeedInput(char),
    ConnectFeedBackspace,
    /// A live-only verb failed without a re-read (offline / server rejection) —
    /// clears the in-flight guard so the gesture can be retried.
    ConnectActionFailed,

    // Queue inspector — the intent-log board (`src/app/screens/queue.rs`,
    // `:queue`, `g q`). `QueueLoaded` carries the same `store.intents()` read
    // the headless `engineer queue` table prints (one source of truth). The
    // gestures: `r` retry (a reconnect drain streaming the shipped transcript),
    // `x` drop the selected *diverged* write (armed → confirmed → the #109
    // `drop_intent`), `⏎` route a diverged intent to the Timer's shipped
    // reconcile panel. `QueueRefresh` reloads after a drain / drop lands.
    QueueLoaded(Vec<Intent>),
    QueueLoadFailed(String),
    QueueRefresh,
    QueueSelectMove(i32),
    /// `r` — retry now: drain the queue through `drain_reporting`, then reload.
    QueueRetry,
    /// `x` — drop the selected diverged write (first press arms, second drops).
    QueueDropSelected,
    /// `⏎` — open a diverged intent's reconcile flow (routed to the Timer panel).
    QueueOpenReconcile,

    // Quick-capture overlay (`src/app/capture.rs`). Reachable from any screen
    // via the `<Space>` leader; also opened pre-filled to edit an existing note
    // from the browser. `CaptureOpen*`/`CaptureClose`/`CaptureSaved` are handled
    // by `App` (they create or drop `App::capture`); the rest are routed to the
    // live overlay reducer.
    CaptureOpen,
    /// Open quick-capture prefilled with text (the `:note <text>` palette
    /// action). The overlay is a *new* draft — safer than a direct create, since
    /// the user can add an anchor or Ctrl-S immediately.
    CaptureOpenText(String),
    CaptureOpenEdit(Box<Note>),
    CaptureClose,
    CaptureSaved,
    CaptureKey(crossterm::event::KeyEvent),
    /// `Ctrl-E` — hand the note body off to `$EDITOR` (the run loop suspends the
    /// TUI, spawns the editor, and feeds the edited text back into `content`).
    CaptureEditExternal,
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
    /// The richer chapter/section anchor picker over the book's `anchor_data`
    /// (notes.dc.html §Anchor picker). Open kicks the fetch/mount; the rest
    /// drive the shared fuzzy picker while it's open.
    CaptureAnchorPickerOpen,
    CaptureAnchorPickerClose,
    CaptureAnchorPickerSubmit,
    CaptureAnchorMove(i32),
    CaptureAnchorInput(char),
    CaptureAnchorBackspace,
    CaptureAnchorDataLoaded(Box<AnchorData>),

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
