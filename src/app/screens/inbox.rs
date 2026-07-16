//! Inbox — the draft-triage screen over the assisted-capture automations
//! (`/api/v1/automations/tasks`, assisted-capture.dc.html). Phase 2 of
//! assisted-capture: the headless verbs shipped in #90 (`engineer inbox`), this
//! is their face. One screen, two stages:
//!
//!   list  — §Inbox · pending: the pending drafts in urgency order (expiring
//!           first), the Review screen's triage grammar wholesale — a `N pending`
//!           count header, full-row `▌` selection, `j`/`k`, and a per-row due
//!           badge that reads `expires_at` instead of a review's due date.
//!   draft — §Inbox · draft: one draft opened — the prompt (the question), the
//!           proposed context, the entity, and the expiry — before it's written.
//!
//! The three verbs each map to one server call: **accept** (`⏎` on the draft →
//! `complete`, which mints the activity), **reject** (`x` → the optional-reason
//! capture, §Inbox · reject), **acknowledge** (`a` → keep for later). Each is a
//! fire-then-re-read verb: the write leaves the client's hands, and the screen
//! re-reads the pending scope after it lands (a draft leaves the scope, so the
//! client never trusts a cached row). A stale-draft `422` surfaces as "already
//! moved on" via the notify tile, not a crash.
//!
//! The verbs are **live-only** — not routed through `QueuedClient` (unlike the
//! timer/week writes). An offline accept can't mint the activity or confirm the
//! `422`, so a synthesized outcome would be a lie; the honest move is a clear
//! offline refusal. See the epic #118 decision log.

use crossterm::event::{KeyCode, KeyEvent};
use jiff::Timestamp;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, ApiError, Task};
use crate::app::action::Action;
use crate::inbox_cli::{ACCEPTED, ACKNOWLEDGED, ALREADY_MOVED_ON, REJECTED};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// The live-only refusal: triage is a server write whose outcome (a minted
/// activity, or the `422`) can't be synthesized offline. Recorded on epic #118.
const OFFLINE_REFUSAL: &str = "offline — triage needs the server; retry online";

/// A draft that expires within this window earns the escalated amber badge — the
/// design's "escalates once" rule (the ambient count's `▾`, the row's warn pill).
const EXPIRING_SOON_SECS: i64 = 48 * 3600;
/// Under this window a draft is *urgent* — the danger badge (the design's red
/// "3h left" treatment).
const EXPIRING_URGENT_SECS: i64 = 12 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    List,
    Draft,
}

/// The optional-reason capture opened by `x` (§Inbox · reject). The reject fires
/// once, on commit — a single terminal `PATCH :reject` — so `Esc` can cancel it
/// outright (there is no un-reject verb to undo a fired one).
struct Rejecting {
    id: i64,
    /// The draft's prompt, kept for the reject panel's summary line.
    prompt: String,
    reason: String,
}

/// One triage verb, carrying what its server call needs.
enum Verb {
    Accept,
    Acknowledge,
    Reject(Option<String>),
}

impl Verb {
    /// The shared past-tense outcome word (one vocabulary with `engineer inbox`).
    fn word(&self) -> &'static str {
        match self {
            Verb::Accept => ACCEPTED,
            Verb::Acknowledge => ACKNOWLEDGED,
            Verb::Reject(_) => REJECTED,
        }
    }
}

pub struct Inbox {
    /// Pending drafts, sorted expiring-first (the queue the design sorts on).
    tasks: Vec<Task>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    stage: Stage,
    /// `Some` while the reject-reason capture is open (modal over the current
    /// stage) — owns keys via `intercept_key`.
    rejecting: Option<Rejecting>,
    /// A triage verb is in flight — guards a second fire before the re-read.
    in_flight: bool,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            selected: 0,
            loading: false,
            error: None,
            stage: Stage::List,
            rejecting: None,
            in_flight: false,
        }
    }
}

impl Inbox {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        tokio::spawn(async move {
            match api.list_pending_tasks().await {
                Ok(tasks) => {
                    let _ = tx.send(Action::InboxLoaded(tasks));
                }
                Err(e) => {
                    let _ = tx.send(Action::InboxLoadFailed(format!("inbox load failed: {e}")));
                }
            }
        });
    }

    /// Fire a triage verb against the *live* server (never the queue) and, on a
    /// resolved outcome, re-read the pending scope. A `422` is the stale-draft
    /// soft re-read; a transport failure is the honest offline refusal.
    fn spawn_verb(&self, verb: Verb, id: i64, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        let word = verb.word();
        tokio::spawn(async move {
            let result = match verb {
                Verb::Accept => api.complete_task(id).await,
                Verb::Acknowledge => api.acknowledge_task(id).await,
                Verb::Reject(reason) => api.reject_task(id, reason).await,
            };
            match result {
                Ok(_) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Success,
                        text: format!("{word} · draft #{id}"),
                    });
                    // Fire-then-re-read: the draft left the scope — re-read it.
                    let _ = tx.send(Action::RefreshInbox);
                }
                Err(ApiError::Problem { status: 422, .. }) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Warning,
                        text: ALREADY_MOVED_ON.to_string(),
                    });
                    let _ = tx.send(Action::RefreshInbox);
                }
                Err(ApiError::Transport(_)) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: OFFLINE_REFUSAL.to_string(),
                    });
                    let _ = tx.send(Action::InboxActionFailed);
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("triage failed: {e}"),
                    });
                    let _ = tx.send(Action::InboxActionFailed);
                }
            }
        });
    }

    /// The reject-reason capture owns keys before the global keymap while open.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.rejecting.is_some() {
            return match key.code {
                KeyCode::Enter => Some(Action::InboxRejectSubmit),
                KeyCode::Esc => Some(Action::InboxRejectCancel),
                KeyCode::Backspace => Some(Action::InboxRejectBackspace),
                KeyCode::Char(c) => Some(Action::InboxRejectInput(c)),
                _ => None,
            };
        }
        None
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::InboxLoaded(mut tasks) => {
                // Expiring-first: soonest expiry leads; drafts with no expiry sink
                // to the bottom (the pipeline's problem to re-raise, not ours).
                tasks.sort_by_key(|t| t.expires_at.map(|e| e.as_second()).unwrap_or(i64::MAX));
                self.tasks = tasks;
                self.loading = false;
                self.error = None;
                self.in_flight = false;
                self.clamp_selection();
                if self.tasks.is_empty() {
                    self.stage = Stage::List;
                }
            }
            Action::InboxLoadFailed(e) => {
                self.loading = false;
                self.error = Some(e.clone());
                return Some((Level::Error, e));
            }
            Action::RefreshInbox => {
                // Re-read resets to the list — the draft that was acted on is
                // gone from the scope, so the detail it filled is stale.
                self.loading = true;
                self.stage = Stage::List;
                self.rejecting = None;
                self.in_flight = false;
                self.fetch(api, tx);
            }
            Action::InboxMove(delta) => {
                if self.stage == Stage::List {
                    self.move_selection(delta);
                }
            }
            Action::InboxOpen => {
                if !self.tasks.is_empty() {
                    self.stage = Stage::Draft;
                }
            }
            Action::InboxCloseDetail => self.stage = Stage::List,
            Action::InboxDraftStep(delta) => {
                if self.stage == Stage::Draft {
                    self.move_selection(delta);
                }
            }
            Action::InboxAccept => self.fire(Verb::Accept, api, tx),
            Action::InboxAck => self.fire(Verb::Acknowledge, api, tx),
            Action::InboxRejectBegin => {
                if let Some(t) = self.current() {
                    self.rejecting = Some(Rejecting {
                        id: t.id,
                        prompt: draft_prompt(t).to_string(),
                        reason: String::new(),
                    });
                }
            }
            Action::InboxRejectInput(c) => {
                if let Some(r) = self.rejecting.as_mut() {
                    r.reason.push(c);
                }
            }
            Action::InboxRejectBackspace => {
                if let Some(r) = self.rejecting.as_mut() {
                    r.reason.pop();
                }
            }
            Action::InboxRejectSubmit => {
                if let Some(r) = self.rejecting.take() {
                    let reason = r.reason.trim();
                    let reason = (!reason.is_empty()).then(|| reason.to_string());
                    if !self.in_flight {
                        self.in_flight = true;
                        self.spawn_verb(Verb::Reject(reason), r.id, api, tx);
                    }
                }
            }
            Action::InboxRejectCancel => self.rejecting = None,
            Action::InboxActionFailed => self.in_flight = false,
            _ => {}
        }
        None
    }

    /// Fire an accept/ack verb on the selected draft, guarded against a double
    /// fire while one is still in flight.
    fn fire(&mut self, verb: Verb, api: &ApiClient, tx: &UnboundedSender<Action>) {
        if self.in_flight {
            return;
        }
        if let Some(id) = self.current().map(|t| t.id) {
            self.in_flight = true;
            self.spawn_verb(verb, id, api, tx);
        }
    }

    fn current(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.tasks.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    fn clamp_selection(&mut self) {
        let len = self.tasks.len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(r) = self.rejecting.as_ref() {
            render_reject(frame, area, r);
            return;
        }
        match self.stage {
            Stage::List => self.render_list(frame, area),
            Stage::Draft => self.render_draft(frame, area),
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let pending = self.tasks.len();
        let block = bordered(format!("Inbox · {pending} pending"));

        if self.tasks.is_empty() {
            if self.loading && self.error.is_none() {
                frame.render_widget(Paragraph::new("loading…").block(block), area);
            } else {
                render_zero(frame, area, block);
            }
            return;
        }

        // The chunks: the count header, then the queue table.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(block.inner(area));
        frame.render_widget(block, area);

        frame.render_widget(Paragraph::new(count_header(pending)), chunks[0]);

        let rows: Vec<Row> = self.tasks.iter().map(draft_row).collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(10), // expires badge
                Constraint::Min(20),    // proposed draft
                Constraint::Length(12), // source
                Constraint::Length(24), // clustered from / entity
            ],
        )
        .header(
            Row::new(vec![
                "EXPIRES",
                "PROPOSED DRAFT",
                "SOURCE",
                "CLUSTERED FROM",
            ])
            .style(theme::header()),
        )
        .row_highlight_style(theme::selection())
        .highlight_symbol("▌ ");
        let mut state = TableState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(table, chunks[1], &mut state);
    }

    fn render_draft(&mut self, frame: &mut Frame, area: Rect) {
        let Some(t) = self.current() else {
            self.stage = Stage::List;
            return;
        };
        let block = bordered("Inbox · draft");
        let mut lines: Vec<Line> = Vec::new();

        // Position + expiry breadcrumb (design: "1 of 3 pending · 2d left").
        let mut crumb = vec![Span::styled(
            format!("{} of {} pending", self.selected + 1, self.tasks.len()),
            theme::muted(),
        )];
        let badge = due_badge(t);
        if !badge.content.is_empty() {
            crumb.push(Span::styled("   ·   ", theme::muted()));
            crumb.push(badge);
        }
        lines.push(Line::from(crumb));
        lines.push(Line::from(""));

        // The prompt — the question the automation asks before it writes.
        lines.push(Line::from(Span::styled(
            (t.automation.as_deref().unwrap_or("automation")).to_string() + " proposes",
            theme::header(),
        )));
        lines.push(Line::from(Span::styled(
            draft_prompt(t).to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // The proposed activity — the entity it targets, and the context fields.
        if let Some(name) = t.entity.as_ref().and_then(|e| e.name.as_deref()) {
            let kind = t
                .entity
                .as_ref()
                .and_then(|e| e.kind.as_deref())
                .map(|k| format!("  ·  {k}"))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled("entity    ", theme::muted()),
                Span::styled(
                    name.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(kind, theme::muted()),
            ]));
        }
        for (k, v) in context_fields(t) {
            lines.push(Line::from(vec![
                Span::styled(format!("{k:<10}"), theme::muted()),
                Span::raw(v),
            ]));
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    pub fn hints(&self) -> Line<'static> {
        if self.rejecting.is_some() {
            return widgets::footer_hints(&[("⏎", "reject"), ("Esc", "cancel")]);
        }
        match self.stage {
            Stage::List => widgets::footer_hints(&[
                ("j/k", "move"),
                ("⏎", "open"),
                ("x", "reject"),
                ("a", "ack"),
                ("h", "home"),
            ]),
            Stage::Draft => widgets::footer_hints(&[
                ("⏎", "accept"),
                ("x", "reject"),
                ("a", "ack"),
                ("J/K", "next"),
                ("h", "back"),
            ]),
        }
    }
}

/// The `N pending` count header — the Review dashboard's count grammar, reading
/// `expires_at` instead of a due date, so drafts are read expiring-first.
fn count_header(pending: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{pending} pending"), theme::header()),
        Span::styled("  ·  expiring first", theme::muted()),
    ])
}

/// §Inbox · zero — the calm empty state. Inbox zero is a success, not a void.
fn render_zero(frame: &mut Frame, area: Rect, block: ratatui::widgets::Block<'static>) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "✓ Inbox clear.",
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Nothing to triage. New commits surface here when the",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "automation clusters them — quietly, as a count.",
            theme::muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .block(block),
        area,
    );
}

/// The reject panel (§Inbox · reject) — the selected draft, then the optional
/// reason field. Reject fires on `⏎`; `Esc` cancels (no un-reject verb exists).
fn render_reject(frame: &mut Frame, area: Rect, r: &Rejecting) {
    let mut lines: Vec<Line> = Vec::new();
    // The draft being rejected, in the selection idiom.
    lines.push(Line::from(Span::styled(
        format!("▌ {}", r.prompt),
        theme::selection(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "Reject this draft",
            Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " — it leaves the inbox. Add a reason so the automation learns, or ⏎ to reject.",
            theme::muted(),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "reason · optional",
        theme::muted(),
    )));
    lines.push(Line::from(vec![
        Span::raw(r.reason.clone()),
        Span::styled("█", theme::muted()),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .block(bordered("Inbox · reject"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// One pending-list row: the due badge, the proposed draft, the source, and what
/// it clustered from (the entity).
fn draft_row(t: &Task) -> Row<'static> {
    Row::new(vec![
        Cell::from(Line::from(due_badge(t))),
        Cell::from(draft_prompt(t).to_string()),
        Cell::from(t.automation.clone().unwrap_or_else(|| "—".into())).style(theme::muted()),
        Cell::from(
            t.entity
                .as_ref()
                .and_then(|e| e.name.clone())
                .unwrap_or_default(),
        )
        .style(theme::muted()),
    ])
}

/// The proposed-draft text — the prompt (the human question), or a calm
/// placeholder when the payload carries none.
fn draft_prompt(t: &Task) -> &str {
    t.prompt.as_deref().unwrap_or("(draft)")
}

/// The `expires_at`-driven due badge (`status_pill` idiom): a black-on-colour
/// pill escalating with urgency — red under 12h, amber under 48h — else a muted
/// "Nd left". Empty when the draft carries no expiry.
fn due_badge(t: &Task) -> Span<'static> {
    let Some(expires) = t.expires_at else {
        return Span::raw(String::new());
    };
    let secs = expires.as_second() - Timestamp::now().as_second();
    if secs <= 0 {
        return Span::styled(
            " expired ",
            Style::default().fg(Color::Black).bg(theme::DANGER),
        );
    }
    let label = format!("{} left", left_text(secs));
    if secs < EXPIRING_URGENT_SECS {
        Span::styled(
            format!(" {label} "),
            Style::default().fg(Color::Black).bg(theme::DANGER),
        )
    } else if secs < EXPIRING_SOON_SECS {
        Span::styled(
            format!(" {label} "),
            Style::default().fg(Color::Black).bg(theme::WARN),
        )
    } else {
        Span::styled(label, theme::muted())
    }
}

/// A compact "time left" read: days at/over 48h, else hours, else minutes.
fn left_text(secs: i64) -> String {
    if secs >= 48 * 3600 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}m", (secs / 60).max(1))
    }
}

/// The proposed context as `(label, value)` pairs for the draft detail. A JSON
/// object renders one field per key (the proposed segment); anything else prints
/// compact under a single `context` label. Nothing shows for a null context.
fn context_fields(t: &Task) -> Vec<(String, String)> {
    match &t.context {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), scalar(v))).collect(),
        other => vec![("context".to_string(), scalar(other))],
    }
}

/// A JSON scalar as a bare string (no quotes on strings); containers compact.
fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whether a pending draft expires within the "soon" window — drives the ambient
/// count's escalation (Home's `▾` chip) and the row's warn/danger badge.
pub fn is_expiring_soon(t: &Task) -> bool {
    match t.expires_at {
        Some(e) => e.as_second() - Timestamp::now().as_second() < EXPIRING_SOON_SECS,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn setup() -> (Inbox, ApiClient, mpsc::UnboundedSender<Action>) {
        let api = ApiClient::with_token(Url::parse("http://localhost").unwrap(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Inbox::default(), api, tx)
    }

    async fn feed(
        s: &mut Inbox,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    /// A pending task with an expiry `hours` from now (or none).
    fn task(id: i64, prompt: &str, hours: Option<i64>) -> Task {
        let expires = hours.map(|h| {
            let secs = Timestamp::now().as_second() + h * 3600;
            serde_json::json!(Timestamp::from_second(secs).unwrap().to_string())
        });
        serde_json::from_value(serde_json::json!({
            "id": id, "automation": "git", "status": "pending",
            "prompt": prompt, "context": { "kind": "build", "commits": 14 },
            "entity": { "name": "Distributed Systems", "type": "domain" },
            "expires_at": expires,
        }))
        .unwrap()
    }

    fn render(s: &mut Inbox) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
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

    // ---- list: load, sort, move ----

    #[tokio::test]
    async fn loaded_sorts_expiring_first() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![
                task(1, "far", Some(48)),
                task(2, "soon", Some(3)),
                task(3, "mid", Some(24)),
            ]),
        )
        .await;
        // Soonest expiry leads.
        assert_eq!(s.tasks.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 3, 1]);
        assert!(!s.loading);
    }

    #[tokio::test]
    async fn no_expiry_sinks_to_the_bottom() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(1, "none", None), task(2, "soon", Some(2))]),
        )
        .await;
        assert_eq!(s.tasks.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 1]);
    }

    #[tokio::test]
    async fn move_clamps_at_both_ends() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![
                task(1, "a", Some(1)),
                task(2, "b", Some(2)),
                task(3, "c", Some(3)),
            ]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxMove(-1)).await; // already at top
        assert_eq!(s.selected, 0);
        feed(&mut s, &api, &tx, Action::InboxMove(1)).await;
        feed(&mut s, &api, &tx, Action::InboxMove(1)).await;
        feed(&mut s, &api, &tx, Action::InboxMove(1)).await; // clamps at last
        assert_eq!(s.selected, 2);
    }

    // ---- open / step the draft ----

    #[tokio::test]
    async fn open_and_close_the_draft_detail() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(1, "a", Some(1))]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxOpen).await;
        assert_eq!(s.stage, Stage::Draft);
        feed(&mut s, &api, &tx, Action::InboxCloseDetail).await;
        assert_eq!(s.stage, Stage::List);
    }

    #[tokio::test]
    async fn draft_step_walks_the_queue() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(1, "a", Some(1)), task(2, "b", Some(2))]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxOpen).await;
        feed(&mut s, &api, &tx, Action::InboxDraftStep(1)).await;
        assert_eq!(s.selected, 1);
        assert_eq!(s.current().unwrap().id, 2);
    }

    #[tokio::test]
    async fn open_on_empty_queue_stays_on_the_list() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, Action::InboxLoaded(vec![])).await;
        feed(&mut s, &api, &tx, Action::InboxOpen).await;
        assert_eq!(s.stage, Stage::List);
    }

    // ---- reject-reason capture ----

    #[tokio::test]
    async fn reject_begin_opens_input_and_accumulates_then_cancels() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(7, "vendored deps", Some(3))]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxRejectBegin).await;
        assert!(s.rejecting.is_some());
        for c in "noise".chars() {
            feed(&mut s, &api, &tx, Action::InboxRejectInput(c)).await;
        }
        feed(&mut s, &api, &tx, Action::InboxRejectBackspace).await;
        assert_eq!(s.rejecting.as_ref().unwrap().reason, "nois");
        // Esc cancels — no server call, the draft stays.
        feed(&mut s, &api, &tx, Action::InboxRejectCancel).await;
        assert!(s.rejecting.is_none());
    }

    #[test]
    fn intercept_routes_keys_only_while_rejecting() {
        use crossterm::event::KeyModifiers;
        let mut s = Inbox::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Closed: keys fall through to the global keymap.
        assert!(s.intercept_key(press(KeyCode::Char('x'))).is_none());
        s.rejecting = Some(Rejecting {
            id: 1,
            prompt: "p".into(),
            reason: String::new(),
        });
        // Even `u` types into the reason — the reject already fires on ⏎.
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('u'))),
            Some(Action::InboxRejectInput('u'))
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Enter)),
            Some(Action::InboxRejectSubmit)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::InboxRejectCancel)
        ));
    }

    // ---- the three verbs: the calls + the re-read ----

    #[tokio::test]
    async fn accept_completes_the_draft_then_rereads() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/7/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Inbox::default();
        s.handle(
            Action::InboxLoaded(vec![task(7, "build", Some(3))]),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::InboxAccept, &api, &tx).await;
        assert!(s.in_flight);
        // The verb fires and asks for a re-read.
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshInbox)).await);
    }

    #[tokio::test]
    async fn reject_submit_sends_the_reason() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/7/reject"))
            .and(body_json(serde_json::json!({ "reason": "vendored deps" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "status": "rejected"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Inbox::default();
        s.handle(
            Action::InboxLoaded(vec![task(7, "build", Some(3))]),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::InboxRejectBegin, &api, &tx).await;
        for c in "vendored deps".chars() {
            s.handle(Action::InboxRejectInput(c), &api, &tx).await;
        }
        s.handle(Action::InboxRejectSubmit, &api, &tx).await;
        assert!(s.rejecting.is_none());
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshInbox)).await);
    }

    #[tokio::test]
    async fn acknowledge_calls_the_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/7/acknowledge"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "status": "pending"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Inbox::default();
        s.handle(
            Action::InboxLoaded(vec![task(7, "build", Some(3))]),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::InboxAck, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshInbox)).await);
    }

    #[tokio::test]
    async fn stale_accept_surfaces_already_moved_on() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/automations/tasks/7/complete"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Cannot complete task", "status": 422
            })))
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Inbox::default();
        s.handle(
            Action::InboxLoaded(vec![task(7, "build", Some(3))]),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::InboxAccept, &api, &tx).await;
        // A soft re-read: a warning tile, then the pending scope refreshes.
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Warning, text } if text.contains("already moved on")
            ))
            .await
        );
    }

    #[tokio::test]
    async fn offline_verb_refuses_and_never_queues() {
        // No server — the request is a transport failure (offline).
        let api = ApiClient::with_token(Url::parse("http://127.0.0.1:1").unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Inbox::default();
        s.handle(
            Action::InboxLoaded(vec![task(7, "build", Some(3))]),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::InboxAccept, &api, &tx).await;
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Error, text } if text.contains("offline")
            ))
            .await
        );
        // The failure clears the in-flight guard (no re-read, the draft stays).
        s.handle(Action::InboxActionFailed, &api, &tx).await;
        assert!(!s.in_flight);
    }

    // ---- renders: the queue, the badge, the zero state ----

    #[tokio::test]
    async fn list_renders_the_count_and_a_due_badge() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(7, "Log 50m build", Some(3))]),
        )
        .await;
        let text = render(&mut s);
        assert!(text.contains("1 pending"), "count header: {text}");
        assert!(text.contains("Log 50m build"), "prompt: {text}");
        assert!(text.contains("left"), "due badge: {text}");
    }

    #[tokio::test]
    async fn empty_queue_renders_the_calm_zero_state() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, Action::InboxLoaded(vec![])).await;
        let text = render(&mut s);
        assert!(text.contains("Inbox clear"), "zero state: {text}");
        assert!(text.contains("Nothing to triage"), "zero copy: {text}");
    }

    #[tokio::test]
    async fn draft_detail_renders_prompt_entity_and_context() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(
                7,
                "Log 50m build on Distributed Systems?",
                Some(48),
            )]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxOpen).await;
        let text = render(&mut s);
        assert!(text.contains("Log 50m build"), "prompt: {text}");
        assert!(text.contains("Distributed Systems"), "entity: {text}");
        assert!(text.contains("build"), "context field: {text}");
        assert!(text.contains("git proposes"), "automation header: {text}");
    }

    #[tokio::test]
    async fn reject_panel_renders_the_reason_field() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::InboxLoaded(vec![task(7, "vendored", Some(3))]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::InboxRejectBegin).await;
        let text = render(&mut s);
        assert!(text.contains("Reject this draft"), "reject copy: {text}");
        assert!(text.contains("reason"), "reason field: {text}");
    }
}
