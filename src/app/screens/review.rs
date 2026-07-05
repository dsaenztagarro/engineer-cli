//! Review screen — spaced repetition over topics (daily-loop.brief.md §5,
//! the web `review.html` IA translated to a character grid). One screen, three
//! stages:
//!
//!   dashboard — the read: the due-queue count + estimated minutes, the streak
//!               stats, and the due queue in urgency order (a preview; the
//!               sitting drives the actual order).
//!   sitting   — the payoff: the queue head → rate with a single keystroke
//!               (`f`/`z`/`s`/`i` = forgot/fuzzy/solid/instant) → the server
//!               returns the next due topic → advance automatically → the queue
//!               drains to a quiet "done" view. `Esc` exits mid-sitting cleanly
//!               (each rating is committed per topic, so no confirmation).
//!   browse    — a secondary state: the full topic catalogue, paginated, with
//!               the API's sort ring on `s` and a server-side `q` search on `/`;
//!               `↵` opens a topic detail read with a one-off rate option.
//!
//! No ASCII heatmap: the epic's brief keeps the terminal dashboard a minimal
//! read — the streak and this-month counts already convey review cadence, and
//! the web app owns the heatmap. The `Dashboard.heatmap` payload is parsed by
//! the API layer but deliberately not rendered here.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Dashboard, Topic, TopicFilters};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// The four spaced-repetition ratings, each a single keystroke. `f`/`z`/`s`/`i`
/// sidesteps the collision `r` would cause (the global refresh key) while
/// staying mnemonic — a distinct letter of each word (forgot / fuZZy / solid /
/// instant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rating {
    Forgot,
    Fuzzy,
    Solid,
    Instant,
}

impl Rating {
    /// The wire value the rate endpoint expects (also the display label).
    pub fn as_str(self) -> &'static str {
        match self {
            Rating::Forgot => "forgot",
            Rating::Fuzzy => "fuzzy",
            Rating::Solid => "solid",
            Rating::Instant => "instant",
        }
    }

    /// The keystroke that selects this rating.
    fn key(self) -> char {
        match self {
            Rating::Forgot => 'f',
            Rating::Fuzzy => 'z',
            Rating::Solid => 's',
            Rating::Instant => 'i',
        }
    }

    /// Map a rating keystroke to its rating. `s` is solid — in the rating
    /// contexts (the sitting, the browse detail) it never means the dashboard's
    /// "start", which those stages have moved past.
    pub fn from_char(c: char) -> Option<Rating> {
        [
            Rating::Forgot,
            Rating::Fuzzy,
            Rating::Solid,
            Rating::Instant,
        ]
        .into_iter()
        .find(|r| r.key() == c)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Dashboard,
    Sitting,
    Browse,
}

/// The browse sort ring cycled by `s` (the `TopicFilters` `sort` values).
/// Urgency first, so it opens in the dashboard-queue order.
const SORTS: [(&str, &str); 6] = [
    ("urgency", "urgency"),
    ("recent", "recent"),
    ("most_reviewed", "most reviewed"),
    ("least_reviewed", "least reviewed"),
    ("longest_interval", "longest interval"),
    ("az", "a–z"),
];

/// Fallback divisor for "page N of M" when the server omits `per_page`. The
/// topics endpoint controls its own page size, echoed back in `meta`.
const PER_PAGE: u32 = 25;

pub struct Review {
    stage: Stage,

    // ---- dashboard ----
    dashboard: Option<Dashboard>,
    loading: bool,
    error: Option<String>,

    // ---- sitting ----
    /// The topic currently being rated; `None` once the queue drains.
    current: Option<Topic>,
    rated: u32,
    done: bool,
    /// A rate request is in flight — guards against double-rating one topic.
    rating_in_flight: bool,

    // ---- browse ----
    topics: Vec<Topic>,
    browse_state: TableState,
    page: u32,
    per_page: u32,
    total: u32,
    sort_idx: usize,
    /// The server-side `q` search text.
    query: String,
    searching: bool,
    browse_loading: bool,
    /// `Some` while the topic detail read is open, over the browse table.
    detail: Option<Topic>,
}

impl Default for Review {
    fn default() -> Self {
        let mut browse_state = TableState::default();
        browse_state.select(Some(0));
        Self {
            stage: Stage::Dashboard,
            dashboard: None,
            loading: false,
            error: None,
            current: None,
            rated: 0,
            done: false,
            rating_in_flight: false,
            topics: vec![],
            browse_state,
            page: 1,
            per_page: PER_PAGE,
            total: 0,
            sort_idx: 0,
            query: String::new(),
            searching: false,
            browse_loading: false,
            detail: None,
        }
    }
}

impl Review {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch_dashboard(api, tx);
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    fn fetch_dashboard(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        tokio::spawn(async move {
            match api.review_dashboard().await {
                Ok(d) => {
                    let _ = tx.send(Action::ReviewDashboardLoaded(Box::new(d)));
                }
                Err(e) => {
                    let _ = tx.send(Action::ReviewLoadFailed(format!("review load failed: {e}")));
                }
            }
        });
    }

    fn fetch_browse(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        let filters = TopicFilters {
            sort: Some(SORTS[self.sort_idx].0.to_string()),
            q: (!self.query.is_empty()).then(|| self.query.clone()),
            page: Some(self.page),
            ..Default::default()
        };
        tokio::spawn(async move {
            match api.list_topics(&filters).await {
                Ok(list) => {
                    let _ = tx.send(Action::ReviewBrowseLoaded {
                        items: list.data,
                        page: list.meta.page,
                        per_page: list.meta.per_page,
                        total: list.meta.total,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::ReviewLoadFailed(format!("topics load failed: {e}")));
                }
            }
        });
    }

    fn spawn_rate(
        &self,
        subdomain_id: i64,
        rating: Rating,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) {
        let (api, tx) = (api.clone(), tx.clone());
        let wire = rating.as_str();
        tokio::spawn(async move {
            match api.rate_topic(subdomain_id, wire).await {
                Ok(res) => {
                    let _ = tx.send(Action::ReviewRated(Box::new(res)));
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("rate failed: {e}"),
                    });
                    // Release the guard so the topic can be re-rated.
                    let _ = tx.send(Action::ReviewRateFailed);
                }
            }
        });
    }

    /// The rating contexts (the sitting, the browse detail) and the browse
    /// search prompt own keys before the global keymap.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.stage {
            Stage::Sitting => {
                if self.done {
                    return match key.code {
                        KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h') => {
                            Some(Action::ReviewExitSitting)
                        }
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Esc => Some(Action::ReviewExitSitting),
                    KeyCode::Char(c) => Rating::from_char(c).map(Action::ReviewRate),
                    _ => None,
                }
            }
            Stage::Browse => {
                if self.detail.is_some() {
                    return match key.code {
                        KeyCode::Char(c) if Rating::from_char(c).is_some() => {
                            Rating::from_char(c).map(Action::ReviewRate)
                        }
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('h') => {
                            Some(Action::ReviewBrowseCloseDetail)
                        }
                        _ => None,
                    };
                }
                if self.searching {
                    return match key.code {
                        KeyCode::Esc => Some(Action::ReviewBrowseSearchCancel),
                        KeyCode::Enter => Some(Action::ReviewBrowseSearchSubmit),
                        KeyCode::Backspace => Some(Action::ReviewBrowseSearchBackspace),
                        KeyCode::Char(c) => Some(Action::ReviewBrowseSearchInput(c)),
                        _ => None,
                    };
                }
                if matches!(key.code, KeyCode::Char('/')) {
                    self.searching = true;
                    self.query.clear();
                    return Some(Action::Notify {
                        level: Level::Info,
                        text: "search topics · Enter to run · Esc clears".into(),
                    });
                }
                None
            }
            Stage::Dashboard => None,
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::ReviewDashboardLoaded(d) => {
                self.dashboard = Some(*d);
                self.loading = false;
                self.error = None;
            }
            Action::ReviewLoadFailed(e) => {
                self.loading = false;
                self.browse_loading = false;
                self.error = Some(e.clone());
                return Some((Level::Error, e));
            }
            Action::RefreshReview => {
                self.loading = true;
                self.fetch_dashboard(api, tx);
            }
            Action::ReviewOpenDashboard => {
                self.stage = Stage::Dashboard;
                self.loading = true;
                self.fetch_dashboard(api, tx);
            }
            Action::ReviewOpenBrowse => {
                self.stage = Stage::Browse;
                self.detail = None;
                self.searching = false;
                self.query.clear();
                self.page = 1;
                self.browse_state.select(Some(0));
                self.browse_loading = true;
                self.fetch_browse(api, tx);
            }
            Action::ReviewStartSitting => {
                let head = self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.queue.first().cloned());
                match head {
                    Some(topic) => {
                        self.stage = Stage::Sitting;
                        self.current = Some(topic);
                        self.rated = 0;
                        self.done = false;
                        self.rating_in_flight = false;
                    }
                    None => {
                        return Some((Level::Info, "nothing due — you're all caught up".into()))
                    }
                }
            }
            Action::ReviewExitSitting => {
                self.stage = Stage::Dashboard;
                self.current = None;
                self.done = false;
                self.rating_in_flight = false;
                // Reflect the topics just rated (queue shrinks, streak grows).
                self.loading = true;
                self.fetch_dashboard(api, tx);
            }
            Action::ReviewRate(rating) => {
                if self.rating_in_flight {
                    return None;
                }
                let subject = match self.stage {
                    Stage::Sitting => self.current.as_ref().map(|t| t.subdomain_id),
                    Stage::Browse => self.detail.as_ref().map(|t| t.subdomain_id),
                    Stage::Dashboard => None,
                };
                if let Some(id) = subject {
                    self.rating_in_flight = true;
                    self.spawn_rate(id, rating, api, tx);
                }
            }
            Action::ReviewRated(res) => {
                self.rating_in_flight = false;
                match self.stage {
                    Stage::Sitting => {
                        self.rated += 1;
                        match res.next_topic.clone() {
                            Some(next) => self.current = Some(next),
                            None => {
                                self.current = None;
                                self.done = true;
                            }
                        }
                    }
                    Stage::Browse => {
                        // A one-off rate from the detail read: close it and
                        // refetch the page so freshness/state mirror the server.
                        self.detail = None;
                        self.browse_loading = true;
                        self.fetch_browse(api, tx);
                        return Some((Level::Success, "rated".into()));
                    }
                    Stage::Dashboard => {}
                }
            }
            Action::ReviewRateFailed => self.rating_in_flight = false,
            Action::ReviewBrowseLoaded {
                items,
                page,
                per_page,
                total,
            } => {
                self.topics = items;
                self.browse_loading = false;
                if page > 0 {
                    self.page = page;
                }
                if per_page > 0 {
                    self.per_page = per_page;
                }
                self.total = total;
                self.clamp_selection();
            }
            Action::ReviewBrowseMove(d) => self.move_cursor(d),
            Action::ReviewBrowseJumpStart => {
                self.browse_state
                    .select((!self.topics.is_empty()).then_some(0));
            }
            Action::ReviewBrowseJumpEnd => {
                if !self.topics.is_empty() {
                    self.browse_state.select(Some(self.topics.len() - 1));
                }
            }
            Action::ReviewBrowsePageNext => {
                if self.page < self.total_pages() {
                    self.page += 1;
                    self.enter_browse_page(api, tx);
                }
            }
            Action::ReviewBrowsePagePrev => {
                if self.page > 1 {
                    self.page -= 1;
                    self.enter_browse_page(api, tx);
                }
            }
            Action::ReviewBrowseCycleSort => {
                self.sort_idx = (self.sort_idx + 1) % SORTS.len();
                self.page = 1;
                self.enter_browse_page(api, tx);
            }
            Action::ReviewBrowseSearchInput(c) => self.query.push(c),
            Action::ReviewBrowseSearchBackspace => {
                self.query.pop();
            }
            Action::ReviewBrowseSearchSubmit => {
                self.searching = false;
                self.page = 1;
                self.browse_state.select(Some(0));
                self.browse_loading = true;
                self.fetch_browse(api, tx);
            }
            Action::ReviewBrowseSearchCancel => {
                self.searching = false;
                let had_query = !self.query.is_empty();
                self.query.clear();
                if had_query {
                    self.page = 1;
                    self.browse_state.select(Some(0));
                    self.browse_loading = true;
                    self.fetch_browse(api, tx);
                }
            }
            Action::ReviewBrowseOpenDetail => {
                if let Some(t) = self.selected_topic() {
                    // Open instantly from the row, then refine with the full
                    // record (the prompts/forecasts the list omits).
                    let id = t.subdomain_id;
                    self.detail = Some(t);
                    let (api, tx) = (api.clone(), tx.clone());
                    tokio::spawn(async move {
                        if let Ok(full) = api.get_topic(id).await {
                            let _ = tx.send(Action::ReviewBrowseDetailLoaded(Box::new(full)));
                        }
                    });
                }
            }
            Action::ReviewBrowseDetailLoaded(t) => {
                if self.detail.is_some() {
                    self.detail = Some(*t);
                }
            }
            Action::ReviewBrowseCloseDetail => self.detail = None,
            _ => {}
        }
        None
    }

    /// Shared reset when the browse page changes (paging or sorting): close the
    /// search prompt (the `q` text persists across pages/sorts), park the cursor
    /// at the top, and refetch.
    fn enter_browse_page(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.searching = false;
        self.browse_state.select(Some(0));
        self.browse_loading = true;
        self.fetch_browse(api, tx);
    }

    fn total_pages(&self) -> u32 {
        if self.per_page == 0 {
            1
        } else {
            self.total.div_ceil(self.per_page).max(1)
        }
    }

    fn selected_topic(&self) -> Option<Topic> {
        self.browse_state
            .selected()
            .and_then(|i| self.topics.get(i).cloned())
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.topics.len();
        if len == 0 {
            return;
        }
        let cur = self.browse_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.browse_state.select(Some(next as usize));
    }

    fn clamp_selection(&mut self) {
        let len = self.topics.len();
        match self.browse_state.selected() {
            Some(i) if i >= len => self.browse_state.select(len.checked_sub(1)),
            None if len > 0 => self.browse_state.select(Some(0)),
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self.stage {
            Stage::Dashboard => self.render_dashboard(frame, area),
            Stage::Sitting => self.render_sitting(frame, area),
            Stage::Browse => {
                if let Some(t) = self.detail.clone() {
                    self.render_detail(frame, area, &t);
                } else {
                    self.render_browse(frame, area);
                }
            }
        }
    }

    fn render_dashboard(&self, frame: &mut Frame, area: Rect) {
        let block = bordered("Review");
        let Some(d) = &self.dashboard else {
            let body = if let Some(err) = &self.error {
                Paragraph::new(Line::from(Span::styled(
                    format!("could not load review: {err}"),
                    Style::default().fg(theme::DANGER),
                )))
            } else {
                Paragraph::new("loading…")
            };
            frame.render_widget(body.block(block), area);
            return;
        };

        let mut lines: Vec<Line> = Vec::new();
        let due = d.queue.len();

        if due == 0 {
            lines.push(Line::from(Span::styled(
                "Nothing due — you're all caught up ✓",
                Style::default().fg(theme::SUCCESS),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{due} topic{} due", if due == 1 { "" } else { "s" }),
                    theme::header(),
                ),
                Span::styled(
                    format!("  ·  ~{} min", d.est_minutes.max(0)),
                    theme::muted(),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "press ↵ to start the sitting",
                theme::muted(),
            )));
        }

        lines.push(Line::from(""));
        lines.push(stats_line(d));
        lines.push(Line::from(""));

        if due > 0 {
            lines.push(Line::from(Span::styled(
                "QUEUE · urgency order",
                theme::header(),
            )));
            let name_w = d
                .queue
                .iter()
                .map(|t| topic_name(t).chars().count())
                .max()
                .unwrap_or(10)
                .clamp(10, 28);
            for t in &d.queue {
                lines.push(queue_line(t, name_w));
            }
        }

        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_sitting(&self, frame: &mut Frame, area: Rect) {
        let block = bordered("Review · sitting");

        if self.done {
            let lines = vec![
                Line::from(Span::styled(
                    "Sitting complete ✓",
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!(
                        "You rated {} topic{}.",
                        self.rated,
                        if self.rated == 1 { "" } else { "s" }
                    ),
                    theme::muted(),
                )),
                Line::from(""),
                Line::from(Span::styled("press ↵ or Esc to return", theme::muted())),
            ];
            frame.render_widget(Paragraph::new(lines).block(block), area);
            return;
        }

        let Some(t) = &self.current else {
            frame.render_widget(Paragraph::new("loading…").block(block), area);
            return;
        };

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            topic_name(t),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(dn) = &t.domain_name {
            lines.push(Line::from(Span::styled(dn.clone(), theme::muted())));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(context_line(t), theme::muted())));

        if !t.notes.is_empty() {
            lines.push(Line::from(""));
            for n in &t.notes {
                lines.push(Line::from(format!("• {}", n.title)));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "How well did you remember?",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(rating_hints_line(t));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} rated this sitting", self.rated),
            theme::muted(),
        )));

        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_browse(&mut self, frame: &mut Frame, area: Rect) {
        let block =
            bordered(self.browse_title()).title_bottom(self.browse_status().right_aligned());

        if self.browse_loading && self.topics.is_empty() {
            frame.render_widget(Paragraph::new("loading…").block(block), area);
            return;
        }
        if self.topics.is_empty() {
            let msg = if !self.query.is_empty() {
                "No topics match that search."
            } else {
                "No topics."
            };
            frame.render_widget(Paragraph::new(msg).block(block), area);
            return;
        }

        let rows: Vec<Row> = self.topics.iter().map(topic_row).collect();
        let table = Table::new(
            rows,
            [
                Constraint::Min(16),    // topic
                Constraint::Length(18), // domain
                Constraint::Length(10), // state
                Constraint::Length(6),  // reviews
                Constraint::Length(8),  // interval
            ],
        )
        .header(Row::new(vec!["TOPIC", "DOMAIN", "STATE", "REV", "IVL"]).style(theme::header()))
        .block(block)
        .row_highlight_style(theme::selection())
        .highlight_symbol("▌ ");
        frame.render_stateful_widget(table, area, &mut self.browse_state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect, t: &Topic) {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            topic_name(t),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(dn) = &t.domain_name {
            lines.push(Line::from(Span::styled(dn.clone(), theme::muted())));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(context_line(t), theme::muted())));

        if !t.notes.is_empty() {
            lines.push(Line::from(""));
            for n in &t.notes {
                lines.push(Line::from(format!("• {}", n.title)));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("rate:", theme::muted())));
        lines.push(rating_hints_line(t));

        frame.render_widget(
            Paragraph::new(lines)
                .block(bordered("Review · topic"))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn browse_title(&self) -> String {
        let sort = SORTS[self.sort_idx].1;
        if self.searching || !self.query.is_empty() {
            format!("Review · browse · {sort} · /{}_", self.query)
        } else {
            format!("Review · browse · {sort}")
        }
    }

    fn browse_status(&self) -> Line<'static> {
        let total = if self.total > 0 {
            self.total
        } else {
            self.topics.len() as u32
        };
        let mut s = format!(
            " page {} of {} · {} total ",
            self.page.max(1),
            self.total_pages(),
            total
        );
        if self.browse_loading {
            s.push_str("· loading… ");
        }
        Line::from(Span::styled(s, theme::muted()))
    }

    pub fn hints(&self) -> Line<'static> {
        match self.stage {
            Stage::Dashboard => widgets::footer_hints(&[
                ("↵/s", "start"),
                ("b", "browse"),
                ("r", "refresh"),
                ("h", "home"),
            ]),
            Stage::Sitting => {
                if self.done {
                    widgets::footer_hints(&[("↵/Esc", "done")])
                } else {
                    widgets::footer_hints(&[
                        ("f", "forgot"),
                        ("z", "fuzzy"),
                        ("s", "solid"),
                        ("i", "instant"),
                        ("Esc", "exit"),
                    ])
                }
            }
            Stage::Browse => {
                if self.detail.is_some() {
                    widgets::footer_hints(&[("f/z/s/i", "rate"), ("↵/Esc", "close")])
                } else if self.searching {
                    Line::from(Span::styled(
                        "type to search topics · Enter to run · Esc to clear",
                        theme::muted(),
                    ))
                } else {
                    widgets::footer_hints(&[
                        ("j/k", "move"),
                        ("↵", "detail"),
                        ("s", "sort"),
                        ("[ ]", "page"),
                        ("/", "find"),
                        ("h", "back"),
                    ])
                }
            }
        }
    }
}

/// `streak 3 days · best 9 · 12 this month · avg 21d interval`.
fn stats_line(d: &Dashboard) -> Line<'static> {
    let s = &d.stats;
    let mut parts = vec![
        format!(
            "streak {} day{}",
            s.current_streak,
            if s.current_streak == 1 { "" } else { "s" }
        ),
        format!("best {}", s.longest_streak),
        format!("{} this month", s.this_month),
    ];
    if let Some(avg) = s.avg_interval {
        parts.push(format!("avg {avg}d interval"));
    }
    Line::from(Span::styled(parts.join("  ·  "), theme::muted()))
}

/// One queue preview row: `Consensus            distributed systems   due · 3×`.
fn queue_line(t: &Topic, name_w: usize) -> Line<'static> {
    let name = pad_or_truncate(&topic_name(t), name_w);
    let domain = pad_or_truncate(&t.domain_name.clone().unwrap_or_default(), 18);
    Line::from(vec![
        Span::styled(format!("{name}  "), Style::default()),
        Span::styled(format!("{domain}  "), theme::muted()),
        Span::styled(freshness(t), theme::muted()),
    ])
}

fn topic_row(t: &Topic) -> Row<'static> {
    let iv = t.interval_days.map(|d| format!("{d}d")).unwrap_or_default();
    Row::new(vec![
        Cell::from(topic_name(t)),
        Cell::from(t.domain_name.clone().unwrap_or_default()).style(theme::muted()),
        Cell::from(t.state.clone()).style(theme::muted()),
        Cell::from(format!("{}×", t.review_count)).style(theme::muted()),
        Cell::from(iv).style(theme::muted()),
    ])
}

/// The four rating keys as black-on-accent caps with labels and, when the
/// payload carries them, the interval each rating would set (`→21d`).
fn rating_hints_line(t: &Topic) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, r) in [
        Rating::Forgot,
        Rating::Fuzzy,
        Rating::Solid,
        Rating::Instant,
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            format!(" {} ", r.key()),
            Style::default().fg(Color::Black).bg(theme::ACCENT),
        ));
        spans.push(Span::raw(format!(" {}", r.as_str())));
        if let Some(days) = t.forecasts.get(r.as_str()) {
            spans.push(Span::styled(format!(" →{days}d"), theme::muted()));
        }
    }
    Line::from(spans)
}

/// The sitting/detail context line: interval, reviews, and last-reviewed date.
fn context_line(t: &Topic) -> String {
    if t.review_count == 0 {
        return "new topic · not yet reviewed".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(iv) = t.interval_days {
        parts.push(format!("interval {iv}d"));
    }
    parts.push(format!(
        "{} review{}",
        t.review_count,
        if t.review_count == 1 { "" } else { "s" }
    ));
    if let Some(ts) = t.last_reviewed_at {
        parts.push(format!("last {}", ts.strftime("%Y-%m-%d")));
    }
    parts.join("  ·  ")
}

/// A compact queue freshness read: the state and review count, plus interval.
fn freshness(t: &Topic) -> String {
    let mut parts = vec![t.state.clone(), format!("{}×", t.review_count)];
    if let Some(iv) = t.interval_days {
        parts.push(format!("{iv}d"));
    }
    parts.join(" · ")
}

fn topic_name(t: &Topic) -> String {
    t.subdomain_name
        .clone()
        .unwrap_or_else(|| format!("topic #{}", t.subdomain_id))
}

fn pad_or_truncate(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len > width {
        let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        format!("{s:<width$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use crossterm::event::{KeyEvent, KeyModifiers};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn setup() -> (Review, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Review::default(), api, tx)
    }

    async fn feed(
        s: &mut Review,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    fn topic(json: serde_json::Value) -> Topic {
        serde_json::from_value(json).unwrap()
    }

    fn dashboard(queue: Vec<Topic>) -> Dashboard {
        let mut d: Dashboard = serde_json::from_value(serde_json::json!({
            "stats": { "current_streak": 3, "longest_streak": 9, "this_month": 12, "avg_interval": 21 },
            "est_minutes": 5,
            "heatmap": { "max": 4, "weeks": [] },
            "queue": []
        }))
        .unwrap();
        d.queue = queue;
        d
    }

    /// A `Review` mid-sitting on `topic` (bypasses the dashboard entry step).
    fn sitting(topic: Topic) -> Review {
        Review {
            stage: Stage::Sitting,
            current: Some(topic),
            ..Default::default()
        }
    }

    fn queue_topics() -> Vec<Topic> {
        vec![
            topic(serde_json::json!({
                "subdomain_id": 5, "domain_id": 1, "subdomain_name": "Consensus",
                "domain_name": "distributed systems", "state": "due", "review_count": 3
            })),
            topic(serde_json::json!({
                "subdomain_id": 6, "domain_id": 2, "subdomain_name": "B-trees",
                "domain_name": "databases", "state": "due", "review_count": 5
            })),
        ]
    }

    // ---- rating keys ----

    #[test]
    fn rating_from_char_maps_the_four_keys() {
        assert_eq!(Rating::from_char('f'), Some(Rating::Forgot));
        assert_eq!(Rating::from_char('z'), Some(Rating::Fuzzy));
        assert_eq!(Rating::from_char('s'), Some(Rating::Solid));
        assert_eq!(Rating::from_char('i'), Some(Rating::Instant));
        assert_eq!(Rating::from_char('r'), None); // the global refresh key is free
        assert_eq!(Rating::from_char('x'), None);
    }

    #[test]
    fn rating_wire_values_match_the_api_contract() {
        assert_eq!(Rating::Forgot.as_str(), "forgot");
        assert_eq!(Rating::Fuzzy.as_str(), "fuzzy");
        assert_eq!(Rating::Solid.as_str(), "solid");
        assert_eq!(Rating::Instant.as_str(), "instant");
    }

    #[test]
    fn sitting_keys_dispatch_ratings_and_exit() {
        let mut s = sitting(queue_topics().remove(0));
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('z'))),
            Some(Action::ReviewRate(Rating::Fuzzy))
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('s'))),
            Some(Action::ReviewRate(Rating::Solid))
        ));
        // A non-rating letter falls through so the global keymap keeps it.
        assert!(s.intercept_key(press(KeyCode::Char('q'))).is_none());
        // Esc exits the sitting cleanly.
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::ReviewExitSitting)
        ));
    }

    // ---- dashboard load ----

    #[tokio::test]
    async fn dashboard_loaded_sets_data_and_clears_loading() {
        let (mut s, api, tx) = setup();
        s.loading = true;
        feed(
            &mut s,
            &api,
            &tx,
            Action::ReviewDashboardLoaded(Box::new(dashboard(queue_topics()))),
        )
        .await;
        assert!(!s.loading);
        let d = s.dashboard.as_ref().unwrap();
        assert_eq!(d.queue.len(), 2);
        assert_eq!(d.stats.current_streak, 3);
        assert_eq!(d.est_minutes, 5);
    }

    // ---- the sitting state machine ----

    #[tokio::test]
    async fn start_sitting_opens_at_the_queue_head() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ReviewDashboardLoaded(Box::new(dashboard(queue_topics()))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::ReviewStartSitting).await;
        assert_eq!(s.stage, Stage::Sitting);
        assert_eq!(s.current.as_ref().unwrap().subdomain_id, 5); // head
        assert_eq!(s.rated, 0);
        assert!(!s.done);
    }

    #[tokio::test]
    async fn start_sitting_with_empty_queue_stays_on_dashboard() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ReviewDashboardLoaded(Box::new(dashboard(vec![]))),
        )
        .await;
        let note = s.handle(Action::ReviewStartSitting, &api, &tx).await;
        assert_eq!(s.stage, Stage::Dashboard);
        assert!(matches!(note, Some((Level::Info, _))));
    }

    #[tokio::test]
    async fn rate_advances_to_next_then_drains_to_done() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Sitting;
        s.current = Some(queue_topics().remove(0));

        // First rating: the server returns the next due topic → advance.
        let next = queue_topics().remove(1);
        feed(
            &mut s,
            &api,
            &tx,
            Action::ReviewRated(Box::new(rate_result(Some(next)))),
        )
        .await;
        assert_eq!(s.rated, 1);
        assert!(!s.done);
        assert_eq!(s.current.as_ref().unwrap().subdomain_id, 6);

        // Second rating: no next topic → the queue is drained.
        feed(
            &mut s,
            &api,
            &tx,
            Action::ReviewRated(Box::new(rate_result(None))),
        )
        .await;
        assert_eq!(s.rated, 2);
        assert!(s.done);
        assert!(s.current.is_none());
    }

    #[tokio::test]
    async fn exit_sitting_returns_to_dashboard() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Sitting;
        s.current = Some(queue_topics().remove(0));
        s.rated = 2;
        feed(&mut s, &api, &tx, Action::ReviewExitSitting).await;
        assert_eq!(s.stage, Stage::Dashboard);
        assert!(s.current.is_none());
        assert!(!s.done);
    }

    fn rate_result(next: Option<Topic>) -> crate::api::RateResult {
        let mut r: crate::api::RateResult = serde_json::from_value(serde_json::json!({
            "topic": { "subdomain_id": 5, "domain_id": 1, "state": "fresh", "review_count": 4 },
            "next_topic": null
        }))
        .unwrap();
        r.next_topic = next;
        r
    }

    // ---- browse: pagination + sort ring ----

    fn browse_loaded(items: Vec<Topic>, page: u32, per_page: u32, total: u32) -> Action {
        Action::ReviewBrowseLoaded {
            items,
            page,
            per_page,
            total,
        }
    }

    #[tokio::test]
    async fn browse_loaded_sets_pagination_meta() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        feed(&mut s, &api, &tx, browse_loaded(queue_topics(), 2, 25, 60)).await;
        assert_eq!(s.page, 2);
        assert_eq!(s.per_page, 25);
        assert_eq!(s.total, 60);
        assert_eq!(s.total_pages(), 3); // ceil(60/25)
    }

    #[tokio::test]
    async fn browse_page_next_advances_then_clamps_at_last() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        feed(&mut s, &api, &tx, browse_loaded(queue_topics(), 1, 25, 60)).await; // 3 pages
        feed(&mut s, &api, &tx, Action::ReviewBrowsePageNext).await;
        assert_eq!(s.page, 2);
        feed(&mut s, &api, &tx, Action::ReviewBrowsePageNext).await;
        assert_eq!(s.page, 3);
        feed(&mut s, &api, &tx, Action::ReviewBrowsePageNext).await;
        assert_eq!(s.page, 3); // clamped
    }

    #[tokio::test]
    async fn browse_page_prev_clamps_at_one() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        feed(&mut s, &api, &tx, browse_loaded(queue_topics(), 1, 25, 60)).await;
        feed(&mut s, &api, &tx, Action::ReviewBrowsePagePrev).await;
        assert_eq!(s.page, 1);
    }

    #[tokio::test]
    async fn browse_sort_ring_walks_and_wraps_resetting_the_page() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        s.page = 3;
        assert_eq!(SORTS[s.sort_idx].0, "urgency");
        for expected in [
            "recent",
            "most_reviewed",
            "least_reviewed",
            "longest_interval",
            "az",
            "urgency",
        ] {
            feed(&mut s, &api, &tx, Action::ReviewBrowseCycleSort).await;
            assert_eq!(SORTS[s.sort_idx].0, expected);
        }
        assert_eq!(s.page, 1); // each cycle resets to the first page
    }

    #[tokio::test]
    async fn browse_search_builds_query_and_cancel_clears_it() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        s.searching = true;
        for c in "btree".chars() {
            feed(&mut s, &api, &tx, Action::ReviewBrowseSearchInput(c)).await;
        }
        assert_eq!(s.query, "btree");
        feed(&mut s, &api, &tx, Action::ReviewBrowseSearchCancel).await;
        assert!(!s.searching);
        assert!(s.query.is_empty());
    }

    #[tokio::test]
    async fn browse_open_then_close_detail_transitions() {
        let (mut s, api, tx) = setup();
        s.stage = Stage::Browse;
        feed(&mut s, &api, &tx, browse_loaded(queue_topics(), 1, 25, 2)).await;
        s.browse_state.select(Some(0));
        feed(&mut s, &api, &tx, Action::ReviewBrowseOpenDetail).await;
        assert!(s.detail.is_some());
        assert_eq!(s.detail.as_ref().unwrap().subdomain_id, 5);
        feed(&mut s, &api, &tx, Action::ReviewBrowseCloseDetail).await;
        assert!(s.detail.is_none());
    }

    // ---- the rate keystroke → server → advance path ----

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
    async fn rate_posts_the_selected_rating_and_yields_the_next_topic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/review/topics/5/rate"))
            .and(body_json(serde_json::json!({ "rating": "solid" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "topic": { "subdomain_id": 5, "domain_id": 1, "state": "fresh", "review_count": 4 },
                "next_topic": { "subdomain_id": 6, "domain_id": 2, "state": "due", "review_count": 5 }
            })))
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = sitting(queue_topics().remove(0)); // subdomain_id 5

        s.handle(Action::ReviewRate(Rating::Solid), &api, &tx).await;
        assert!(s.rating_in_flight); // guarded until the result lands
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::ReviewRated(_))).await);
    }
}
