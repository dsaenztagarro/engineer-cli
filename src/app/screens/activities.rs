//! Activities table — the core domain surface (daily-loop.brief.md §5,
//! the web `Activities.html` IA translated to a character grid). A dense,
//! scannable ledger of recent activities: a semantic status pill, kind, title,
//! domain (by name — the terminal palette has no per-domain colours), duration,
//! and a relative "when". It is the first screen to expose `meta.page`
//! pagination, and it carries the row actions of the daily loop: complete,
//! archive/unarchive (reversible, so it toggles quietly with no confirm),
//! duplicate ("do this again"), a detail read, and binding the live timer to
//! the selected activity.
//!
//! Filtering: `f` cycles a single, TUI-light filter ring that folds the two
//! server-side axes worth cycling — lifecycle status and archived — into one
//! key (all → planned → started → completed → archived). Kind is free-form on
//! the wire and ill-suited to blind cycling, so it is reachable through `/`,
//! which filters the *loaded page* client-side (the activities list API exposes
//! no `q` text search) across title, kind, and domain.
//!
//! Mutations refetch the current page rather than patching the row in place, so
//! the visible ledger always mirrors the server (the notes-browser discipline).
//!
//! Offline honesty (#109, §Segment audit · mixed): every fetch folds the
//! pending queue over the server page (`queue::fold_activities`) — a
//! still-queued create renders as a full `◔ … provisional · queued` row mixed
//! with the confirmed, and a queued segment's minutes ride its parent row as
//! `◔+Nm`. Read-time composition only: nothing provisional is ever written
//! into a cache, and the rows settle to plain on the next fetch after the
//! drain. Row actions refuse on a provisional row — there is no server id to
//! act on yet.
//!
//! Offline writes (#110): the row's lifecycle verbs — complete (`c`),
//! archive/unarchive (`a`), duplicate (`d`) — route through `QueuedClient` like
//! every other write, so an offline gesture queues (confirming `· queued
//! (offline)`) instead of bouncing. Complete/unarchive replay plain (naturally
//! idempotent); duplicate mints a fresh copy and replays plain too (not in the
//! `Idempotency-Key` opt-in set, ADR 0036), so a lost-ack re-fire makes a
//! visible, archivable second copy — the accepted risk over a lost gesture.

use jiff::Timestamp;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{Activity, ActivityFilters, ApiClient};
use crate::app::action::Action;
use crate::queue::{self, FoldedActivity, QueueStore, WriteOutcome};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

/// Page size we request. Also the divisor for "page N of M" when the server
/// omits `per_page` from `meta`.
const PER_PAGE: u32 = 25;

/// The filter ring cycled by `f`: `(label, status, archived)`. `all` is the
/// unfiltered default; the middle three are the conventional lifecycle values
/// sent as the server `status=` filter; `archived` folds in archived-only rows.
const FILTERS: [(&str, Option<&str>, Option<&str>); 5] = [
    ("all", None, None),
    ("planned", Some("planned"), None),
    ("started", Some("started"), None),
    ("completed", Some("completed"), None),
    ("archived", None, Some("true")),
];

pub struct Activities {
    /// The server page with the pending queue folded over it (#109) — the
    /// provisional rows carry negative ids, exactly as the queue minted them.
    items: Vec<FoldedActivity>,
    state: TableState,
    /// 1-based current page (server-side).
    page: u32,
    per_page: u32,
    total: u32,
    /// Index into `FILTERS`.
    filter_idx: usize,
    /// Client-side narrow over the loaded page (no server text search).
    query: String,
    searching: bool,
    loading: bool,
    /// `Some` while the full-field detail read is open, over the table.
    detail: Option<Activity>,
    /// Queue location for the read-time fold; `None` (production) reads the
    /// shared XDG queue. Tests inject a scratch dir.
    queue_paths: QueuePaths,
}

impl Default for Activities {
    fn default() -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            items: vec![],
            state,
            page: 1,
            per_page: PER_PAGE,
            total: 0,
            filter_idx: 0,
            query: String::new(),
            searching: false,
            loading: false,
            detail: None,
            queue_paths: None,
        }
    }
}

impl Activities {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        let (_, status, archived) = FILTERS[self.filter_idx];
        let filters = ActivityFilters {
            status: status.map(str::to_string),
            archived: archived.map(str::to_string),
            page: Some(self.page),
            per_page: Some(self.per_page),
            ..Default::default()
        };
        let paths = self.queue_paths.clone();
        tokio::spawn(async move {
            match api.list_activities(&filters).await {
                Ok(list) => {
                    // Fold the pending queue over the fetched page — read-time
                    // composition, re-read fresh on every fetch so a drained
                    // intent's row settles to plain on the very next load.
                    // Best-effort on the read side, like the timer fold: an
                    // unreadable queue folds as empty (enqueue stays loud).
                    let intents = read_queue(&paths);
                    let _ = tx.send(Action::ActivitiesLoaded {
                        items: queue::fold_activities(list.data, &intents),
                        page: list.meta.page,
                        per_page: list.meta.per_page,
                        total: list.meta.total,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::ActivitiesLoadFailed(format!(
                        "activities load failed: {e}"
                    )));
                }
            }
        });
    }

    /// Total pages implied by `meta.total`/`per_page`. When the server omits
    /// pagination metadata the screen behaves as a single page.
    fn total_pages(&self) -> u32 {
        if self.per_page == 0 {
            1
        } else {
            self.total.div_ceil(self.per_page).max(1)
        }
    }

    /// The rows visible after the client-side `/` narrow (all rows when empty).
    fn visible(&self) -> Vec<&FoldedActivity> {
        if self.query.is_empty() {
            return self.items.iter().collect();
        }
        let q = self.query.to_ascii_lowercase();
        self.items
            .iter()
            .filter(|f| matches_query(&f.activity, &q))
            .collect()
    }

    fn selected(&self) -> Option<FoldedActivity> {
        let vis = self.visible();
        self.state
            .selected()
            .and_then(|i| vis.get(i).cloned().cloned())
    }

    /// The selected row when it names a real server record — the row actions'
    /// gate. A provisional (still-queued) row has no server id to act on, so
    /// the gesture refuses with the way forward instead of 404ing blind.
    fn selected_confirmed(&self) -> Result<Option<Activity>, (Level, String)> {
        match self.selected() {
            Some(f) if f.is_provisional() => Err((
                Level::Warning,
                "still queued — it syncs when the wire returns; resolve a stuck write in `engineer queue`".into(),
            )),
            Some(f) => Ok(Some(f.activity)),
            None => Ok(None),
        }
    }

    /// The detail read and the search prompt own keys before the global keymap.
    pub fn intercept_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Action> {
        use crossterm::event::KeyCode;
        if self.detail.is_some() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('h') => {
                    Some(Action::ActivitiesCloseDetail)
                }
                _ => None,
            };
        }
        if !self.searching {
            if matches!(key.code, KeyCode::Char('/')) {
                self.searching = true;
                self.query.clear();
                return Some(Action::Notify {
                    level: Level::Info,
                    text: "filter: type to narrow this page · Esc clears".into(),
                });
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => Some(Action::ActivitiesSearchCancel),
            KeyCode::Enter => Some(Action::ActivitiesSearchSubmit),
            KeyCode::Backspace => Some(Action::ActivitiesSearchBackspace),
            KeyCode::Char(c) => Some(Action::ActivitiesSearchInput(c)),
            _ => None,
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::ActivitiesLoaded {
                items,
                page,
                per_page,
                total,
            } => {
                self.items = items;
                self.loading = false;
                // Adopt the server's echoed pagination when present; a missing
                // `meta` (all zeros) leaves the requested page/size intact.
                if page > 0 {
                    self.page = page;
                }
                if per_page > 0 {
                    self.per_page = per_page;
                }
                self.total = total;
                self.clamp_selection();
            }
            Action::ActivitiesLoadFailed(msg) => {
                self.loading = false;
                return Some((Level::Error, msg));
            }
            Action::ActivitiesMove(d) => self.move_cursor(d),
            Action::ActivitiesJumpStart => {
                self.state.select((!self.visible().is_empty()).then_some(0));
            }
            Action::ActivitiesJumpEnd => {
                let len = self.visible().len();
                if len > 0 {
                    self.state.select(Some(len - 1));
                }
            }
            Action::ActivitiesPageNext => {
                if self.page < self.total_pages() {
                    self.page += 1;
                    self.enter_page(api, tx);
                }
            }
            Action::ActivitiesPagePrev => {
                if self.page > 1 {
                    self.page -= 1;
                    self.enter_page(api, tx);
                }
            }
            Action::ActivitiesCycleFilter => {
                self.filter_idx = (self.filter_idx + 1) % FILTERS.len();
                self.page = 1;
                self.enter_page(api, tx);
            }
            Action::ActivitiesSearchInput(c) => {
                self.query.push(c);
                self.state.select(Some(0));
            }
            Action::ActivitiesSearchBackspace => {
                self.query.pop();
                self.state.select(Some(0));
            }
            Action::ActivitiesSearchSubmit => self.searching = false,
            Action::ActivitiesSearchCancel => {
                self.searching = false;
                self.query.clear();
                self.state.select(Some(0));
            }
            Action::ActivitiesOpenDetail => {
                if let Some(f) = self.selected() {
                    // Open instantly from the row, then refine with the full
                    // record (segments count, generated notes the list omits).
                    // A provisional row has no server record to refine from —
                    // the local fold is everything there is, honestly.
                    let id = f.activity.id;
                    self.detail = Some(f.activity);
                    if id < 0 {
                        return None;
                    }
                    let (api, tx) = (api.clone(), tx.clone());
                    tokio::spawn(async move {
                        if let Ok(full) = api.get_activity(id).await {
                            let _ = tx.send(Action::ActivitiesDetailLoaded(Box::new(full)));
                        }
                    });
                }
            }
            Action::ActivitiesDetailLoaded(a) => {
                if self.detail.is_some() {
                    self.detail = Some(*a);
                }
            }
            Action::ActivitiesCloseDetail => self.detail = None,
            Action::ActivitiesComplete => {
                if let Some(a) = match self.selected_confirmed() {
                    Ok(a) => a,
                    Err(warn) => return Some(warn),
                } {
                    let (api, tx) = (api.clone(), tx.clone());
                    let paths = self.queue_paths.clone();
                    let title = a.title.clone();
                    tokio::spawn(async move {
                        let queued = match open_queued(&api, &paths) {
                            Ok(q) => q,
                            Err(e) => return notify_seam_error(&tx, "complete failed", e),
                        };
                        match queued.complete_activity(a.id).await {
                            Ok(WriteOutcome::Confirmed(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: format!("completed · {title}"),
                                });
                                let _ = tx.send(Action::RefreshActivities);
                            }
                            Ok(WriteOutcome::Provisional(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Info,
                                    text: format!(
                                        "completed · {title} · queued (offline) — will sync"
                                    ),
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("complete failed: {e}"),
                                });
                            }
                        }
                    });
                }
            }
            Action::ActivitiesArchive => {
                if let Some(a) = match self.selected_confirmed() {
                    Ok(a) => a,
                    Err(warn) => return Some(warn),
                } {
                    let archived = a.is_archived();
                    let (api, tx) = (api.clone(), tx.clone());
                    let paths = self.queue_paths.clone();
                    tokio::spawn(async move {
                        let queued = match open_queued(&api, &paths) {
                            Ok(q) => q,
                            Err(e) => return notify_seam_error(&tx, "archive failed", e),
                        };
                        let res = if archived {
                            queued.unarchive_activity(a.id).await
                        } else {
                            queued.archive_activity(a.id).await
                        };
                        match res {
                            Ok(WriteOutcome::Confirmed(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: if archived {
                                        "unarchived".into()
                                    } else {
                                        "archived".into()
                                    },
                                });
                                let _ = tx.send(Action::RefreshActivities);
                            }
                            Ok(WriteOutcome::Provisional(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Info,
                                    text: if archived {
                                        "unarchived · queued (offline) — will sync".into()
                                    } else {
                                        "archived · queued (offline) — will sync".into()
                                    },
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("archive failed: {e}"),
                                });
                            }
                        }
                    });
                }
            }
            Action::ActivitiesDuplicate => {
                if let Some(a) = match self.selected_confirmed() {
                    Ok(a) => a,
                    Err(warn) => return Some(warn),
                } {
                    let (api, tx) = (api.clone(), tx.clone());
                    let paths = self.queue_paths.clone();
                    let title = a.title.clone();
                    tokio::spawn(async move {
                        let queued = match open_queued(&api, &paths) {
                            Ok(q) => q,
                            Err(e) => return notify_seam_error(&tx, "duplicate failed", e),
                        };
                        match queued.duplicate_activity(a.id).await {
                            Ok(WriteOutcome::Confirmed(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: format!("duplicated · {title}"),
                                });
                                let _ = tx.send(Action::RefreshActivities);
                            }
                            Ok(WriteOutcome::Provisional(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Info,
                                    text: format!(
                                        "duplicated · {title} · queued (offline) — will sync"
                                    ),
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("duplicate failed: {e}"),
                                });
                            }
                        }
                    });
                }
            }
            Action::ActivitiesStartTimer => {
                if let Some(a) = match self.selected_confirmed() {
                    Ok(a) => a,
                    Err(warn) => return Some(warn),
                } {
                    let (api, tx) = (api.clone(), tx.clone());
                    let paths = self.queue_paths.clone();
                    let title = a.title.clone();
                    tokio::spawn(async move {
                        let queued = match open_queued(&api, &paths) {
                            Ok(q) => q,
                            Err(e) => return notify_seam_error(&tx, "timer start failed", e),
                        };
                        // Start (switching away from any running timer) bound to
                        // this activity, so the segment lands on the right work.
                        match queued.start_timer(Some(a.id), true).await {
                            Ok(WriteOutcome::Confirmed(t)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: format!("timer started · {title}"),
                                });
                                // Refresh the app-owned header cell snapshot.
                                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
                            }
                            Ok(WriteOutcome::Provisional(t)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Info,
                                    text: format!(
                                        "timer started · {title} · queued (offline) — will sync"
                                    ),
                                });
                                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("timer start failed: {e}"),
                                });
                            }
                        }
                    });
                }
            }
            Action::RefreshActivities => {
                self.loading = true;
                self.fetch(api, tx);
            }
            _ => {}
        }
        None
    }

    /// Shared reset when the loaded page changes (paging or filtering): drop the
    /// page-scoped search, park the cursor at the top, and refetch.
    fn enter_page(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.searching = false;
        self.query.clear();
        self.state.select(Some(0));
        self.loading = true;
        self.fetch(api, tx);
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.visible().len();
        if len == 0 {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.state.select(Some(next as usize));
    }

    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        match self.state.selected() {
            Some(i) if i >= len => self.state.select(len.checked_sub(1)),
            None if len > 0 => self.state.select(Some(0)),
            _ => {}
        }
    }

    fn panel_title(&self) -> String {
        if self.searching || !self.query.is_empty() {
            return format!("Activities · /{}_", self.query);
        }
        match FILTERS[self.filter_idx].0 {
            "all" => "Activities".to_string(),
            label => format!("Activities · {label}"),
        }
    }

    /// The bottom-border status line: `page N of M · X total` (+ a loading note).
    fn status_line(&self) -> Line<'static> {
        let total = if self.total > 0 {
            self.total
        } else {
            self.items.len() as u32
        };
        let mut s = format!(
            " page {} of {} · {} total ",
            self.page.max(1),
            self.total_pages(),
            total
        );
        if self.loading {
            s.push_str("· loading… ");
        }
        Line::from(Span::styled(s, theme::muted()))
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(a) = self.detail.clone() {
            self.render_detail(frame, area, &a);
            return;
        }

        let block = bordered(self.panel_title()).title_bottom(self.status_line().right_aligned());

        if self.loading && self.items.is_empty() {
            frame.render_widget(Paragraph::new("loading…").block(block), area);
            return;
        }
        if self.visible().is_empty() {
            let msg = if !self.query.is_empty() {
                "No activities match that filter on this page."
            } else {
                "No activities here. Log one with `a`, or cycle filters with `f`."
            };
            frame.render_widget(Paragraph::new(msg).block(block), area);
            return;
        }

        let now = Timestamp::now();
        let rows: Vec<Row> = self
            .visible()
            .iter()
            .map(|f| activity_row(f, now))
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(9),  // status pill (or the ◔ queued mark)
                Constraint::Length(10), // kind
                Constraint::Min(16),    // title (clipped by the column)
                Constraint::Length(14), // domain (by name)
                Constraint::Length(11), // duration (+ the folded ◔+Nm queued)
                Constraint::Length(10), // when (relative)
            ],
        )
        .header(
            Row::new(vec!["STATUS", "KIND", "TITLE", "DOMAIN", "DUR", "WHEN"])
                .style(theme::header()),
        )
        .block(block)
        .row_highlight_style(theme::selection())
        .highlight_symbol("▌ ");
        frame.render_stateful_widget(table, area, &mut self.state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect, a: &Activity) {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![
            widgets::activity_status_pill(a.status.as_deref()),
            Span::raw("  "),
            Span::styled(
                a.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));

        let mut tags: Vec<String> = Vec::new();
        if let Some(k) = &a.kind {
            tags.push(format!("kind {k}"));
        }
        if let Some(i) = &a.intent {
            tags.push(format!("intent {i}"));
        }
        if let Some(b) = &a.bloom_level {
            tags.push(format!("bloom {b}"));
        }
        if let Some(d) = &a.domain_name {
            tags.push(format!("domain {d}"));
        }
        if !tags.is_empty() {
            lines.push(Line::from(Span::styled(tags.join("  ·  "), theme::muted())));
        }

        let mut timing: Vec<String> = Vec::new();
        if let Some(d) = a.duration_minutes {
            timing.push(format!("{d} min"));
        }
        if let Some(c) = a.segments_count {
            timing.push(format!("{c} segment{}", if c == 1 { "" } else { "s" }));
        }
        if let Some(t) = a.started_at {
            timing.push(format!("started {}", t.strftime("%Y-%m-%d %H:%M")));
        }
        if let Some(t) = a.ended_at {
            timing.push(format!("ended {}", t.strftime("%H:%M")));
        }
        if !timing.is_empty() {
            lines.push(Line::from(Span::styled(
                timing.join("  ·  "),
                theme::muted(),
            )));
        }
        if a.is_archived() {
            lines.push(Line::from(Span::styled("archived", theme::muted())));
        }

        if let Some(notes) = a.notes_generated.as_deref().filter(|n| !n.is_empty()) {
            lines.push(Line::from(""));
            for raw in notes.split('\n') {
                lines.push(Line::from(raw.to_string()));
            }
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(bordered("Activity"))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    pub fn hints(&self) -> Line<'static> {
        if self.detail.is_some() {
            return widgets::footer_hints(&[("↵/Esc", "close"), ("h", "back")]);
        }
        if self.searching {
            return Line::from(Span::styled(
                "type to filter this page · Enter to keep · Esc to clear",
                theme::muted(),
            ));
        }
        widgets::footer_hints(&[
            ("j/k", "move"),
            ("↵", "detail"),
            ("c", "done"),
            ("a", "archive"),
            ("d", "dup"),
            ("t", "timer"),
            ("f", "filter"),
            ("[ ]", "page"),
            ("/", "find"),
            ("h", "back"),
        ])
    }
}

fn matches_query(a: &Activity, q: &str) -> bool {
    let hit = |o: &Option<String>| {
        o.as_deref()
            .is_some_and(|s| s.to_ascii_lowercase().contains(q))
    };
    a.title.to_ascii_lowercase().contains(q)
        || hit(&a.kind)
        || hit(&a.domain_name)
        || hit(&a.status)
}

fn activity_row(f: &FoldedActivity, now: Timestamp) -> Row<'static> {
    let a = &f.activity;
    let when = a
        .started_at
        .map(|t| fmt_relative(t, now))
        .unwrap_or_default();

    // §Segment audit · mixed: a still-queued create is a full ◔ row — the
    // amber mark where the pill would be, the state named in the title cell,
    // dim against the confirmed rows. Same table, one glyph.
    if f.is_provisional() {
        let amber = Style::default().fg(theme::WARN);
        let dur = a
            .duration_minutes
            .map(|d| format!("{d}m"))
            .unwrap_or_default();
        return Row::new(vec![
            Cell::from("◔ queued").style(amber),
            Cell::from(a.kind.clone().unwrap_or_default()).style(theme::muted()),
            Cell::from(Line::from(vec![
                Span::styled(a.title.clone(), amber),
                Span::styled("  provisional · queued", theme::muted()),
            ])),
            Cell::from(a.domain_name.clone().unwrap_or_default()).style(theme::muted()),
            Cell::from(dur).style(amber),
            Cell::from(when).style(theme::muted()),
        ]);
    }

    let title_style = if a.is_archived() {
        theme::muted()
    } else {
        Style::default()
    };
    // Queued segment minutes ride beside the confirmed duration, marked —
    // never summed into it as if the server had acknowledged them.
    let dur_cell = if f.queued_minutes > 0 {
        Cell::from(Line::from(vec![
            Span::styled(
                a.duration_minutes
                    .map(|d| format!("{d}m"))
                    .unwrap_or_default(),
                theme::muted(),
            ),
            Span::styled(
                format!(" ◔+{}m", f.queued_minutes),
                Style::default().fg(theme::WARN),
            ),
        ]))
    } else {
        Cell::from(
            a.duration_minutes
                .map(|d| format!("{d}m"))
                .unwrap_or_default(),
        )
        .style(theme::muted())
    };
    Row::new(vec![
        Cell::from(widgets::activity_status_pill(a.status.as_deref())),
        Cell::from(a.kind.clone().unwrap_or_default()).style(theme::muted()),
        Cell::from(a.title.clone()).style(title_style),
        Cell::from(a.domain_name.clone().unwrap_or_default()).style(theme::muted()),
        dur_cell,
        Cell::from(when).style(theme::muted()),
    ])
}

/// The queue for the read-time fold — best-effort like every read-side queue
/// touch: an unreadable queue reads as empty here (`engineer queue` is the
/// loud surface for that), and the fetched page renders unfolded.
fn read_queue(paths: &QueuePaths) -> Vec<crate::queue::Intent> {
    let store = match paths {
        Some((queue, _)) => QueueStore::at(queue.clone()),
        None => match QueueStore::open_default() {
            Ok(store) => store,
            Err(_) => return Vec::new(),
        },
    };
    store.intents().unwrap_or_default()
}

/// A compact relative "when": `now`, `5m ago`, `3h ago`, `2d ago`, `3w ago`,
/// then a bare `YYYY-MM-DD` for anything older. Future timestamps (a planned
/// copy) read `soon`.
fn fmt_relative(ts: Timestamp, now: Timestamp) -> String {
    let secs = now.as_second() - ts.as_second();
    if secs < 0 {
        return "soon".into();
    }
    let mins = secs / 60;
    if mins < 1 {
        return "now".into();
    }
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d ago");
    }
    if days < 30 {
        return format!("{}w ago", days / 7);
    }
    ts.strftime("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn setup() -> (Activities, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Activities::default(), api, tx)
    }

    fn activity(json: serde_json::Value) -> Activity {
        serde_json::from_value(json).unwrap()
    }

    async fn feed(
        s: &mut Activities,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    fn loaded(items: Vec<Activity>, page: u32, per_page: u32, total: u32) -> Action {
        Action::ActivitiesLoaded {
            // An empty queue folds to the confirmed rows unchanged — the
            // shape `fetch` sends when nothing is pending.
            items: queue::fold_activities(items, &[]),
            page,
            per_page,
            total,
        }
    }

    fn three() -> Vec<Activity> {
        vec![
            activity(
                serde_json::json!({ "id": 1, "title": "Read SICP", "kind": "reading", "status": "started" }),
            ),
            activity(
                serde_json::json!({ "id": 2, "title": "Solve DP", "kind": "problem", "status": "completed" }),
            ),
            activity(
                serde_json::json!({ "id": 3, "title": "Refactor api", "kind": "coding", "status": "planned" }),
            ),
        ]
    }

    #[tokio::test]
    async fn loaded_sets_items_and_pagination_meta() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 2, 25, 42)).await;
        assert_eq!(s.items.len(), 3);
        assert_eq!(s.page, 2);
        assert_eq!(s.per_page, 25);
        assert_eq!(s.total, 42);
        assert_eq!(s.total_pages(), 2); // ceil(42/25)
    }

    #[tokio::test]
    async fn loaded_without_meta_keeps_requested_page() {
        let (mut s, api, tx) = setup();
        s.page = 3;
        // meta all-zero (server omitted it) must not reset the page to 0.
        feed(&mut s, &api, &tx, loaded(three(), 0, 0, 0)).await;
        assert_eq!(s.page, 3);
        assert_eq!(s.total_pages(), 1);
    }

    #[tokio::test]
    async fn loaded_clamps_stale_selection() {
        let (mut s, api, tx) = setup();
        s.state.select(Some(9));
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        assert_eq!(s.state.selected(), Some(2));
    }

    #[tokio::test]
    async fn loaded_empty_deselects() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(vec![], 1, 25, 0)).await;
        assert_eq!(s.state.selected(), None);
    }

    #[tokio::test]
    async fn move_clamps_within_visible_bounds() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        feed(&mut s, &api, &tx, Action::ActivitiesMove(-5)).await;
        assert_eq!(s.state.selected(), Some(0));
        feed(&mut s, &api, &tx, Action::ActivitiesMove(9)).await;
        assert_eq!(s.state.selected(), Some(2));
    }

    #[tokio::test]
    async fn page_next_advances_then_clamps_at_last_page() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 42)).await; // 2 pages
        feed(&mut s, &api, &tx, Action::ActivitiesPageNext).await;
        assert_eq!(s.page, 2);
        feed(&mut s, &api, &tx, Action::ActivitiesPageNext).await;
        assert_eq!(s.page, 2); // clamped
    }

    #[tokio::test]
    async fn page_prev_clamps_at_one() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 42)).await;
        feed(&mut s, &api, &tx, Action::ActivitiesPagePrev).await;
        assert_eq!(s.page, 1);
    }

    #[tokio::test]
    async fn cycle_filter_walks_the_ring_and_wraps() {
        let (mut s, api, tx) = setup();
        assert_eq!(FILTERS[s.filter_idx].0, "all");
        for expected in ["planned", "started", "completed", "archived", "all"] {
            feed(&mut s, &api, &tx, Action::ActivitiesCycleFilter).await;
            assert_eq!(FILTERS[s.filter_idx].0, expected);
        }
    }

    #[tokio::test]
    async fn cycle_filter_resets_to_first_page() {
        let (mut s, api, tx) = setup();
        s.page = 4;
        feed(&mut s, &api, &tx, Action::ActivitiesCycleFilter).await;
        assert_eq!(s.page, 1);
    }

    #[tokio::test]
    async fn search_narrows_visible_then_cancel_restores() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        s.searching = true;
        for c in "sicp".chars() {
            feed(&mut s, &api, &tx, Action::ActivitiesSearchInput(c)).await;
        }
        assert_eq!(s.visible().len(), 1);
        assert_eq!(s.visible()[0].activity.id, 1);
        feed(&mut s, &api, &tx, Action::ActivitiesSearchCancel).await;
        assert!(!s.searching);
        assert!(s.query.is_empty());
        assert_eq!(s.visible().len(), 3);
    }

    #[tokio::test]
    async fn search_matches_kind_not_only_title() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        s.searching = true;
        for c in "coding".chars() {
            feed(&mut s, &api, &tx, Action::ActivitiesSearchInput(c)).await;
        }
        assert_eq!(s.visible().len(), 1);
        assert_eq!(s.visible()[0].activity.id, 3);
    }

    #[tokio::test]
    async fn open_then_close_detail_transitions() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        feed(&mut s, &api, &tx, Action::ActivitiesOpenDetail).await;
        assert!(s.detail.is_some());
        assert_eq!(s.detail.as_ref().unwrap().id, 1);
        feed(&mut s, &api, &tx, Action::ActivitiesCloseDetail).await;
        assert!(s.detail.is_none());
    }

    #[test]
    fn fmt_relative_reads_common_buckets() {
        let now: Timestamp = "2026-07-05T12:00:00Z".parse().unwrap();
        let ago = |secs: i64| {
            let t = Timestamp::from_second(now.as_second() - secs).unwrap();
            fmt_relative(t, now)
        };
        assert_eq!(ago(30), "now");
        assert_eq!(ago(5 * 60), "5m ago");
        assert_eq!(ago(3 * 3600), "3h ago");
        assert_eq!(ago(2 * 86_400), "2d ago");
        assert_eq!(ago(21 * 86_400), "3w ago");
        // A future stamp (a planned copy) reads "soon".
        let future = Timestamp::from_second(now.as_second() + 3600).unwrap();
        assert_eq!(fmt_relative(future, now), "soon");
    }

    // ---- action dispatch (member actions refetch the page) ----

    fn srv_client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    async fn recv_matching(
        rx: &mut mpsc::UnboundedReceiver<Action>,
        pred: impl Fn(&Action) -> bool,
    ) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(a)) if pred(&a) => return true,
                Ok(Some(_)) => continue,
                _ => return false,
            }
        }
    }

    #[tokio::test]
    async fn complete_refetches_the_page_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/1/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1, "title": "Read SICP", "status": "completed"
            })))
            .mount(&server)
            .await;

        let api = srv_client(&server);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (queue_path, _) = scratch_queue();
        let mut s = Activities {
            queue_paths: Some((queue_path, std::path::PathBuf::new())),
            ..Activities::default()
        };
        s.handle(loaded(three(), 1, 25, 3), &api, &tx).await;
        s.state.select(Some(0));
        s.handle(Action::ActivitiesComplete, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshActivities)).await);
    }

    #[tokio::test]
    async fn archive_takes_the_unarchive_path_for_an_archived_row() {
        let server = MockServer::start().await;
        // Only the unarchive route is mounted — if the reducer chose `archive`
        // instead, no RefreshActivities would arrive and the test would fail.
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/5/unarchive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "Old", "status": "completed"
            })))
            .mount(&server)
            .await;

        let api = srv_client(&server);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (queue_path, _) = scratch_queue();
        let mut s = Activities {
            queue_paths: Some((queue_path, std::path::PathBuf::new())),
            ..Activities::default()
        };
        let archived = activity(serde_json::json!({
            "id": 5, "title": "Old", "archived_at": "2026-07-01T00:00:00Z"
        }));
        s.handle(loaded(vec![archived], 1, 25, 1), &api, &tx).await;
        s.state.select(Some(0));
        s.handle(Action::ActivitiesArchive, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshActivities)).await);
    }

    #[tokio::test]
    async fn duplicate_refetches_the_page_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/2/duplicate"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 99, "title": "Solve DP", "status": "planned"
            })))
            .mount(&server)
            .await;

        let api = srv_client(&server);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (queue_path, _) = scratch_queue();
        let mut s = Activities {
            queue_paths: Some((queue_path, std::path::PathBuf::new())),
            ..Activities::default()
        };
        s.handle(loaded(three(), 1, 25, 3), &api, &tx).await;
        s.state.select(Some(1)); // id 2
        s.handle(Action::ActivitiesDuplicate, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshActivities)).await);
    }

    #[tokio::test]
    async fn start_timer_binds_selected_and_refreshes_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 1
            })))
            .mount(&server)
            .await;

        let api = srv_client(&server);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (queue_path, _) = scratch_queue();
        let mut s = Activities {
            queue_paths: Some((queue_path, std::path::PathBuf::new())),
            ..Activities::default()
        };
        s.handle(loaded(three(), 1, 25, 3), &api, &tx).await;
        s.state.select(Some(0));
        s.handle(Action::ActivitiesStartTimer, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::TimerLoaded(_))).await);
    }

    // ---- provisional rows in the read (#109, §Segment audit · mixed) ----

    use crate::queue::IntentKind;

    /// A per-test scratch queue so the fold never touches the shared XDG state.
    fn scratch_queue() -> (std::path::PathBuf, QueueStore) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-activities-screen-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        let store = QueueStore::at(&path);
        (path, store)
    }

    fn render_activities(s: &mut Activities) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(110, 20)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// The full read path, offline work included: the fetch folds the pending
    /// queue over the server page — the queued create is a `◔` row mixed with
    /// the confirmed, and the queued segment's minutes ride its parent row.
    #[tokio::test]
    async fn fetch_folds_the_pending_queue_over_the_server_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "id": 1, "title": "Raft leader election", "kind": "build",
                      "status": "started", "duration_minutes": 52 }
                ],
                "meta": { "page": 1, "per_page": 25, "total": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (queue_path, store) = scratch_queue();
        store
            .enqueue(IntentKind::ActivityCreate {
                body: crate::api::ActivityCreate {
                    title: "Paxos made live".into(),
                    duration_minutes: Some(20),
                    ..Default::default()
                },
            })
            .unwrap();
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 1,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 14,
            })
            .unwrap();

        let api = srv_client(&server);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Activities {
            queue_paths: Some((queue_path, std::path::PathBuf::new())),
            ..Activities::default()
        };
        s.on_enter(&api, &tx);
        let loaded = loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("the load lands")
            {
                Some(a @ Action::ActivitiesLoaded { .. }) => break a,
                Some(_) => continue,
                None => panic!("channel closed"),
            }
        };
        s.handle(loaded, &api, &tx).await;

        assert_eq!(s.items.len(), 2, "confirmed + provisional, one list");
        assert_eq!(s.items[0].queued_minutes, 14, "the queued segment rides #1");
        assert!(s.items[1].is_provisional());
        assert_eq!(s.items[1].activity.title, "Paxos made live");

        let text = render_activities(&mut s);
        assert!(text.contains("◔ queued"), "the mark: {text}");
        assert!(text.contains("provisional · queued"), "the state: {text}");
        assert!(text.contains("◔+14m"), "queued minutes beside DUR: {text}");
        assert!(text.contains("Raft leader election"), "mixed: {text}");
    }

    /// The after-drain read: the same fetch with an emptied queue folds to the
    /// plain page — provisional rows clear on the very next load.
    #[tokio::test]
    async fn provisional_rows_clear_after_the_drain() {
        let (mut s, api, tx) = setup();
        let queued = queue::fold_activities(
            three(),
            &[crate::queue::Intent {
                id: 3,
                idempotency_key: "key-3".into(),
                stream: "activity".into(),
                queued_at: jiff::Timestamp::now(),
                kind: IntentKind::ActivityCreate {
                    body: crate::api::ActivityCreate {
                        title: "Paxos made live".into(),
                        ..Default::default()
                    },
                },
                state: crate::queue::IntentState::Pending,
                attempts: 0,
                last_error: None,
            }],
        );
        feed(
            &mut s,
            &api,
            &tx,
            Action::ActivitiesLoaded {
                items: queued,
                page: 1,
                per_page: 25,
                total: 3,
            },
        )
        .await;
        assert_eq!(s.items.len(), 4, "the ◔ row is in the list");

        // The next fetch after the drain: the queue is empty, the fold is
        // identity — the row settled into (or left) the server page.
        feed(&mut s, &api, &tx, loaded(three(), 1, 25, 3)).await;
        assert_eq!(s.items.len(), 3);
        assert!(s.items.iter().all(|f| !f.is_provisional()));
    }

    #[tokio::test]
    async fn row_actions_refuse_on_a_provisional_row() {
        let (mut s, api, tx) = setup();
        let mut rows = three();
        rows.push(activity(serde_json::json!({
            "id": -3, "title": "Paxos made live", "status": "planned"
        })));
        feed(&mut s, &api, &tx, loaded(rows, 1, 25, 3)).await;
        s.state.select(Some(3)); // the provisional row

        for action in [
            Action::ActivitiesComplete,
            Action::ActivitiesArchive,
            Action::ActivitiesDuplicate,
            Action::ActivitiesStartTimer,
        ] {
            let warned = s.handle(action, &api, &tx).await;
            let (level, text) = warned.expect("the refusal is surfaced");
            assert_eq!(level, Level::Warning);
            assert!(text.contains("still queued"), "{text}");
        }

        // The detail opens from the local fold — there is no server record to
        // refine from, and no fetch is spawned for a negative id.
        feed(&mut s, &api, &tx, Action::ActivitiesOpenDetail).await;
        assert_eq!(s.detail.as_ref().unwrap().title, "Paxos made live");
    }

    // ---- offline row actions enqueue through the queue (#110) ----

    #[tokio::test]
    async fn offline_row_actions_enqueue_through_the_queue() {
        // A dead address: each live write bounces (Transport) and the verb
        // queues on its row's stream, confirming `queued (offline)` rather than
        // failing — the wiring proof for the QueuedClient seam (its own
        // enqueue/synthesis is covered in `queue::client`).
        let (queue_path, _) = scratch_queue();
        let api = ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Activities {
            queue_paths: Some((queue_path.clone(), std::path::PathBuf::new())),
            ..Activities::default()
        };
        s.handle(loaded(three(), 1, 25, 3), &api, &tx).await;

        s.state.select(Some(0)); // id 1 → complete
        s.handle(Action::ActivitiesComplete, &api, &tx).await;
        s.state.select(Some(2)); // id 3 → duplicate
        s.handle(Action::ActivitiesDuplicate, &api, &tx).await;

        // Each verb enqueues before it notifies, so two `queued (offline)`
        // notices mean both intents have landed.
        for _ in 0..2 {
            assert!(
                recv_matching(&mut rx, |a| matches!(
                    a,
                    Action::Notify { text, .. } if text.contains("queued (offline)")
                ))
                .await,
                "each offline verb confirms queued"
            );
        }

        let intents = QueueStore::at(&queue_path).pending().unwrap();
        let words: Vec<&str> = intents.iter().map(|i| i.kind.word()).collect();
        assert_eq!(intents.len(), 2, "both verbs queued: {words:?}");
        assert!(words.contains(&"complete"), "{words:?}");
        assert!(words.contains(&"duplicate"), "{words:?}");
    }
}
