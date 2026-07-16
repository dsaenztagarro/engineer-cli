//! Week board — the planned-vs-done readout for one ISO week, and the plan-write
//! gestures (week-planning.dc.html §Week · board / §Week · add an intent). One
//! row per plan item with a derived state pill (` done ` / ` live ` / ` hold ` /
//! ` untouched `), a logged-vs-planned meter, the summary line, and the retro
//! band that reads the stored week note. Step weeks with `[` / `]`; `t` returns
//! to this week. The TUI twin of the shipped `engineer week` readout — the plan
//! and the actuals stay one ledger (`GET /api/v1/weeks/:iso_week`).
//!
//! Declaring the week is a keystroke: `a` opens the one-line intent input and
//! `⏎` declares a `planned` activity carrying `planned_on` for the shown week
//! (a plan item *is* a planned activity — no second ledger); `e` adjusts the
//! selected item's title; `d` drops it (archived, confirmed on a second press).
//! Every write routes through `QueuedClient`, so an offline gesture queues and
//! the board renders it provisionally (`◔ … queued`) until it replays.
//!
//! `s` is the Plan↔timer seam (#116): it starts — or stops & switches — the
//! timer bound to the selected item's activity through the same
//! `QueuedClient::start_timer` plumbing the Timer screen uses (the verb, not a
//! copy of the screen). Nothing running starts it outright; a timer already
//! elsewhere warns first (naming the running session) and switches on the
//! second `s`; a still-queued row refuses (the server hasn't minted the
//! activity yet). The board marks the running item with a green `● live` pill,
//! read straight off the header timer snapshot. The `$EDITOR` reflection write
//! is the remaining follow-on slice of the epic.

use jiff::civil::Date;
use jiff::{ToSpan, Zoned};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ActivityCreate, ApiClient, PlanItem, PlanState, Timer, Week as WeekData};
use crate::app::action::Action;
use crate::queue::WriteOutcome;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

/// Meter bar width in cells (matches the Progress screen's ten-block bar).
const BAR_WIDTH: usize = 10;

/// The one-line intent input (§Week · add an intent), when open. `Add` declares
/// a new plan item; `Edit` adjusts the selected item's title in place. Both
/// share the `i`/`Esc` insert grammar: typing fills the buffer, `⏎` writes, an
/// empty buffer or `Esc` cancels.
enum Input {
    Add { buf: String },
    Edit { id: i64, buf: String },
}

impl Input {
    fn buf(&self) -> &str {
        match self {
            Input::Add { buf } | Input::Edit { buf, .. } => buf,
        }
    }

    fn buf_mut(&mut self) -> &mut String {
        match self {
            Input::Add { buf } | Input::Edit { buf, .. } => buf,
        }
    }
}

#[derive(Default)]
pub struct Week {
    data: Option<WeekData>,
    /// Weeks relative to the current study week: 0 = this week, -1 = last week.
    offset: i32,
    loading: bool,
    error: Option<String>,
    /// Full-row `▌` cursor over the plan rows — the row `e` (adjust) / `d`
    /// (drop) act on.
    selected: usize,
    /// The one-line intent input, when open (`a` to add, `e` to adjust).
    input: Option<Input>,
    /// The plan item armed for drop; a second `d` on the same row confirms.
    drop_armed: Option<i64>,
    /// The activity id armed for a stop-&-switch start; a second `s` on the same
    /// row (while a timer runs elsewhere) confirms the switch. Cleared by any
    /// cursor move or reload — the switch-confirm idiom, the Timer screen's verb.
    start_armed: Option<i64>,
    /// The header timer snapshot, cached from the app's forwarded `TimerLoaded` /
    /// `TimerProvisional` polls. The board reads it two ways: to mark the running
    /// item ` live ` (when its `activity_id` is on the shown week) and to name a
    /// session the seam would switch away from. `None` = nothing running.
    running: Option<Timer>,
    /// Titles declared offline this session — rendered as provisional `◔ …
    /// queued` rows until the create replays and a live refetch returns the real
    /// row. The queue is the ledger; this is only the render of what's pending,
    /// cleared on any authoritative reload or week step.
    provisional: Vec<String>,
    /// A reflection written offline this session — the retro band renders it
    /// marked `◔ queued` until the note write replays and a live refetch returns
    /// the stored note. Like `provisional`, only the render of what's pending.
    provisional_note: Option<String>,
    /// Queue + read-cache paths for the write seam (`None` = shared XDG; tests
    /// inject a scratch dir so a spawned write never touches the real queue).
    queue_paths: QueuePaths,
}

impl Week {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
        // Pull a fresh header timer snapshot so the seam knows — right away, not
        // one poll interval later — whether a timer is running (the running row
        // marker, and the switch-confirm's naming of the running session). The
        // app's poll forwards `TimerLoaded`/`TimerProvisional` to this screen.
        let _ = tx.send(Action::RefreshTimer);
    }

    /// While the one-line intent input is open it owns every key, so a typed
    /// letter fills the buffer rather than firing the board keymap (the same
    /// modal-input idiom the Progress inline editor uses).
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.input.as_ref()?;
        match key.code {
            KeyCode::Esc => Some(Action::WeekInputCancel),
            KeyCode::Enter => Some(Action::WeekInputSubmit),
            KeyCode::Backspace => Some(Action::WeekInputBackspace),
            KeyCode::Char(c) => Some(Action::WeekInputChar(c)),
            _ => None,
        }
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let api = api.clone();
        let tx = tx.clone();
        // `get_week` needs a concrete ISO week — the current week can't defer to
        // a server default the way Progress does, so offset 0 resolves too.
        let iso = super::iso_week_for_offset(self.offset);
        tokio::spawn(async move {
            match api.get_week(&iso).await {
                Ok(week) => {
                    let _ = tx.send(Action::WeekLoaded(Box::new(week)));
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("week load failed: {e}"),
                    });
                    let _ = tx.send(Action::WeekLoadFailed(e.to_string()));
                }
            }
        });
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::WeekLoaded(week) => {
                self.data = Some(*week);
                self.loading = false;
                self.error = None;
                // Server truth supersedes the provisional rows and note: a synced
                // declare/reflection is now in the payload, and the queue still
                // holds any that haven't replayed (the header `↑N` and
                // `engineer queue` show it).
                self.provisional.clear();
                self.provisional_note = None;
                self.drop_armed = None;
                self.start_armed = None;
                // Keep the cursor in range as the plan changes week to week.
                let n = self.item_count();
                self.selected = self.selected.min(n.saturating_sub(1));
            }
            Action::WeekLoadFailed(e) => {
                self.loading = false;
                self.error = Some(e);
            }
            Action::WeekStep(delta) => {
                self.offset += delta;
                self.loading = true;
                self.reset_transient();
                self.fetch(api, tx);
            }
            Action::WeekReset => {
                if self.offset != 0 {
                    self.offset = 0;
                    self.loading = true;
                    self.reset_transient();
                    self.fetch(api, tx);
                }
            }
            Action::RefreshWeek => {
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::WeekSelectMove(delta) => {
                // The cursor spans the real rows *and* the provisional queued
                // rows below them — `s` on a queued row is where the seam's
                // "still queued" refusal lives.
                let n = self.total_rows() as i32;
                if n > 0 {
                    self.selected = (self.selected as i32 + delta).clamp(0, n - 1) as usize;
                }
                self.drop_armed = None;
                self.start_armed = None;
            }
            // --- plan writes (#115) ---
            Action::WeekAddBegin => {
                if self.input.is_none() {
                    self.input = Some(Input::Add { buf: String::new() });
                    self.drop_armed = None;
                }
            }
            Action::WeekAdjustBegin => {
                // Prefill with the current title so the edit starts from the truth.
                if self.input.is_none() {
                    if let Some((id, title)) = self.selected_item().map(|i| (i.id, i.title.clone()))
                    {
                        self.input = Some(Input::Edit { id, buf: title });
                        self.drop_armed = None;
                    }
                }
            }
            Action::WeekInputChar(c) => {
                if let Some(input) = self.input.as_mut() {
                    input.buf_mut().push(c);
                }
            }
            Action::WeekInputBackspace => {
                if let Some(input) = self.input.as_mut() {
                    input.buf_mut().pop();
                }
            }
            Action::WeekInputCancel => self.input = None,
            Action::WeekInputSubmit => {
                let input = self.input.take()?;
                let title = input.buf().trim().to_string();
                // An empty buffer cancels — never declare a nameless intent.
                if title.is_empty() {
                    return None;
                }
                match input {
                    Input::Add { .. } => {
                        let create = ActivityCreate {
                            title: title.clone(),
                            planned_on: Some(self.planned_on()),
                            ..Default::default()
                        };
                        spawn_declare(api, tx, self.queue_paths.clone(), create, title);
                    }
                    Input::Edit { id, .. } => {
                        spawn_adjust(api, tx, self.queue_paths.clone(), id, title);
                    }
                }
            }
            Action::WeekDrop => {
                let id = match self.selected_item() {
                    Some(item) => item.id,
                    None => return None,
                };
                if self.drop_armed == Some(id) {
                    self.drop_armed = None;
                    spawn_drop(api, tx, self.queue_paths.clone(), id);
                } else {
                    self.drop_armed = Some(id);
                    return Some((
                        Level::Warning,
                        "press d again to drop this intent (archived, not deleted)".into(),
                    ));
                }
            }
            Action::WeekPlanQueued(title) => self.provisional.push(title),
            // `i` — the retro reflection (#117). Seed the editor with the current
            // note body and hand off to the app, which owns the terminal suspend
            // + `$EDITOR` spawn (the git-commit pattern). No-op until loaded.
            Action::WeekReflect => {
                if let Some(data) = &self.data {
                    let iso_week = data.week.id.clone();
                    // Seed from the local pending note if one is queued, else the
                    // stored server note — so re-editing an offline reflection
                    // starts from what the user just wrote, not the stale server body.
                    let seed = self
                        .provisional_note
                        .clone()
                        .unwrap_or_else(|| data.note.body.clone());
                    let _ = tx.send(Action::WeekReflectEdit { iso_week, seed });
                }
            }
            // The editor saved — persist through the queue seam (empty clears).
            Action::WeekReflectSave { iso_week, body } => {
                spawn_reflect(api, tx, self.queue_paths.clone(), iso_week, body);
            }
            // The editor aborted — the note is untouched; just say so.
            Action::WeekReflectAbort => {
                return Some((Level::Info, "reflection unchanged".into()));
            }
            // An offline reflection landed in the queue — render it marked queued.
            Action::WeekReflectQueued(body) => self.provisional_note = Some(body),
            Action::WeekStartTimer => return self.start_on_selected(api, tx),
            // The header poll forwards its snapshot to the current screen; cache
            // it (only while a clock runs) so the board can mark the running row
            // and the seam can name a session it would switch away from.
            Action::TimerLoaded(t) | Action::TimerProvisional(t) => {
                self.running = if t.running { Some(*t) } else { None };
            }
            _ => {}
        }
        None
    }

    /// The `s` gesture (the Plan↔timer seam): start — or stop & switch — the
    /// timer bound to the selected plan item's activity. Nothing running starts
    /// it outright (`switch: false`); a timer already elsewhere warns first,
    /// naming the running session, and switches on the second press
    /// (`switch: true`) — the Timer screen's confirm idiom, reused as the verb.
    /// A still-queued row (an offline declare the server hasn't minted) refuses:
    /// there is no activity id to bind to yet.
    fn start_on_selected(
        &mut self,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        // Past the real rows sits a provisional (queued-declare) row — no server
        // activity yet, so the start can't bind. Refuse honestly.
        if self.selected >= self.item_count() {
            self.start_armed = None;
            return Some((
                Level::Warning,
                "still queued — sync first, then start the timer".into(),
            ));
        }
        let (id, title) = {
            let item = self.selected_item()?;
            (item.id, item.title.clone())
        };
        // Defensive twin of the check above: a negative sentinel id is a
        // not-yet-minted create, same refusal.
        if id < 0 {
            self.start_armed = None;
            return Some((
                Level::Warning,
                "still queued — sync first, then start the timer".into(),
            ));
        }

        match self.running.as_ref().filter(|t| t.running) {
            // Already timing this very item — a fresh start would only stop &
            // restart the same work; say so rather than churn a segment.
            Some(t) if t.activity_id == Some(id) => {
                self.start_armed = None;
                Some((Level::Info, format!("already timing {title}")))
            }
            // A timer runs on something else: the switch-confirm. First press
            // names the running session and asks again; the second switches.
            Some(t) => {
                if self.start_armed == Some(id) {
                    self.start_armed = None;
                    spawn_start_on_plan(api, tx, self.queue_paths.clone(), id, true, title);
                    None
                } else {
                    self.start_armed = Some(id);
                    let running = t.label.clone().unwrap_or_else(|| "untitled".into());
                    Some((
                        Level::Warning,
                        format!(
                            "already timing {running} — press s again to stop & save it, then start {title}"
                        ),
                    ))
                }
            }
            // Nothing running: start bound outright, no switch.
            None => {
                self.start_armed = None;
                spawn_start_on_plan(api, tx, self.queue_paths.clone(), id, false, title);
                None
            }
        }
    }

    /// Clear the per-week transient UI (open input, armed drop, provisional
    /// rows) — run when the shown week changes so nothing leaks across weeks.
    fn reset_transient(&mut self) {
        self.input = None;
        self.drop_armed = None;
        self.start_armed = None;
        self.provisional.clear();
        self.provisional_note = None;
    }

    fn selected_item(&self) -> Option<&PlanItem> {
        self.data.as_ref()?.items().nth(self.selected)
    }

    /// Every selectable board row: the server's plan items plus the provisional
    /// queued-declare rows the cursor also moves over.
    fn total_rows(&self) -> usize {
        self.item_count() + self.provisional.len()
    }

    /// Whether a running timer is bound to this activity — the board's ` live `
    /// mark, read off the cached header snapshot (only present while running).
    fn is_live(&self, id: i64) -> bool {
        self.running
            .as_ref()
            .is_some_and(|t| t.running && t.activity_id == Some(id))
    }

    /// The day a board declare lands on: today when the shown week is the current
    /// week, else the shown week's Monday. The design's `Add intent · <week>`
    /// header anchors the intent to the shown week, not a specific weekday, and
    /// this mirrors `engineer plan add`'s today-default for the live week.
    fn planned_on(&self) -> Date {
        let today = Zoned::now().date();
        if self.offset == 0 {
            today
        } else {
            self.data
                .as_ref()
                .and_then(|d| d.week.monday)
                .unwrap_or(today)
        }
    }

    fn item_count(&self) -> usize {
        self.data.as_ref().map_or(0, |d| d.items().count())
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Week · planned vs done");

        let Some(data) = &self.data else {
            let body = if let Some(err) = &self.error {
                Paragraph::new(Line::from(Span::styled(
                    format!("could not load the week: {err}"),
                    Style::default().fg(theme::DANGER),
                )))
            } else {
                Paragraph::new("loading…")
            };
            frame.render_widget(body.block(block), area);
            return;
        };

        let items: Vec<&PlanItem> = data.items().collect();
        let mut lines: Vec<Line> = vec![week_header(data), Line::from("")];

        // The one-line intent input rides above the rows while open.
        if let Some(input) = &self.input {
            lines.push(input_line(input, data));
            lines.push(Line::from(""));
        }

        if items.is_empty() && self.provisional.is_empty() {
            lines.extend(empty_lines());
        } else {
            let title_w = items
                .iter()
                .map(|i| i.title.chars().count())
                .chain(self.provisional.iter().map(|t| t.chars().count()))
                .max()
                .unwrap_or(16)
                .clamp(16, 32);
            for (i, item) in items.iter().enumerate() {
                lines.push(plan_row(
                    item,
                    title_w,
                    i == self.selected,
                    self.is_live(item.id),
                ));
            }
            // Declared-offline rows, still waiting on the queue — selectable so
            // `s` on one can refuse honestly (no server activity to bind yet).
            for (j, title) in self.provisional.iter().enumerate() {
                lines.push(provisional_row(
                    title,
                    title_w,
                    self.selected == items.len() + j,
                ));
            }
            // The drop confirm prompt, under the rows when a row is armed.
            if self.drop_armed.is_some() {
                lines.push(Line::from(Span::styled(
                    "  drop this intent? press d again — archived, not deleted",
                    Style::default().fg(theme::WARN),
                )));
            }
            lines.push(Line::from(""));
            lines.push(summary_line(data));
        }

        lines.extend(retro_lines(data, self.provisional_note.as_deref()));

        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    pub fn hints(&self) -> Line<'static> {
        if let Some(input) = &self.input {
            let verb = match input {
                Input::Add { .. } => "declare",
                Input::Edit { .. } => "save",
            };
            return widgets::footer_hints(&[("⏎", verb), ("Esc", "cancel")]);
        }
        widgets::footer_hints(&[
            ("j/k", "select"),
            ("s", "start"),
            ("a", "add"),
            ("e", "adjust"),
            ("d", "drop"),
            ("i", "reflect"),
            ("[", "prev wk"),
            ("]", "next wk"),
            ("t", "this wk"),
            ("h", "home"),
        ])
    }
}

/// Declare a plan item — a `planned` activity with `planned_on` set — through
/// the queue seam (`a` + `⏎`). A confirmed create refetches the week (the server
/// now has the row); an offline one queues and the board renders the provisional
/// title until it replays.
fn spawn_declare(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    create: ActivityCreate,
    title: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "declare failed", e),
        };
        match queued.create_activity(&create).await {
            Ok(WriteOutcome::Confirmed(a)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("planned · {}", a.title),
                });
                let _ = tx.send(Action::RefreshWeek);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::WeekPlanQueued(title));
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "declared · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_seam_error(&tx, "declare failed", e),
        }
    });
}

/// Adjust the selected plan item's title in place (`e` + `⏎`) through the queue
/// seam. Confirmed refetches; offline queues.
fn spawn_adjust(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    id: i64,
    title: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "adjust failed", e),
        };
        match queued.update_activity(id, &title).await {
            Ok(WriteOutcome::Confirmed(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("adjusted · {title}"),
                });
                let _ = tx.send(Action::RefreshWeek);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "adjusted · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_seam_error(&tx, "adjust failed", e),
        }
    });
}

/// Drop the selected plan item — archive it (`d`, second press) through the
/// queue seam. Confirmed refetches; offline queues.
fn spawn_drop(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths, id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "drop failed", e),
        };
        match queued.archive_activity(id).await {
            Ok(WriteOutcome::Confirmed(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: "dropped · archived".into(),
                });
                let _ = tx.send(Action::RefreshWeek);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "dropped · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_seam_error(&tx, "drop failed", e),
        }
    });
}

/// Start (or stop & switch) the timer bound to a plan item's activity through
/// the queue seam (`s`). `switch` rides the intent, so an offline switch replays
/// as stop & save then start — the same server verb the Timer screen defers. A
/// confirmed start forwards the fresh snapshot (`TimerLoaded`), which lands the
/// header cell *and*, forwarded back to this screen, marks the running row; an
/// offline start streams the provisional clock (`TimerProvisional`) the same way.
fn spawn_start_on_plan(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    activity_id: i64,
    switch: bool,
    title: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "timer start failed", e),
        };
        match queued.start_timer(Some(activity_id), switch).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("timer started · {title}"),
                });
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: format!("timer started · {title} · queued (offline) — will sync"),
                });
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
            }
            Err(e) => notify_seam_error(&tx, "timer start failed", e),
        }
    });
}

/// Persist the week's retro reflection through the queue seam (`i`, save-quit).
/// The route upserts, so an offline write replays idempotently. A confirmed
/// write refetches the week (the note is now on the server); an offline one
/// queues and the retro band renders the local body marked `◔ queued` until it
/// replays. An empty `body` is a deliberate clear (the server treats empty as
/// clear) — the same call, so the confirm/queue paths are identical.
fn spawn_reflect(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    iso_week: String,
    body: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "reflection failed", e),
        };
        let cleared = body.trim().is_empty();
        match queued.update_week_note(&iso_week, &body).await {
            Ok(WriteOutcome::Confirmed(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: if cleared {
                        "reflection cleared".into()
                    } else {
                        "reflection saved".into()
                    },
                });
                let _ = tx.send(Action::RefreshWeek);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::WeekReflectQueued(body));
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "reflection · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_seam_error(&tx, "reflection failed", e),
        }
    });
}

/// The one-line intent input (§Week · add an intent): an INSERT badge, the
/// context label (`Add intent · <week>` or `Adjust intent`), and the buffer with
/// a block cursor.
fn input_line(input: &Input, data: &WeekData) -> Line<'static> {
    let label = match input {
        Input::Add { .. } => format!("Add intent · {}", data.week.id),
        Input::Edit { .. } => "Adjust intent".to_string(),
    };
    Line::from(vec![
        Span::styled(
            " INSERT ",
            Style::default().fg(Color::Black).bg(theme::ACCENT),
        ),
        Span::styled(format!(" {label}  "), theme::muted()),
        Span::raw(input.buf().to_string()),
        Span::styled("█", Style::default().fg(theme::ACCENT)),
    ])
}

/// A plan item declared offline — a `◔ … queued` stand-in the board shows until
/// the create replays and a live refetch returns the real row. Selectable (`▌`)
/// so `s` can land on it and refuse the start — the server hasn't minted it yet.
fn provisional_row(title: &str, title_w: usize, selected: bool) -> Line<'static> {
    let marker = if selected { "▌ " } else { "  " };
    Line::from(vec![
        Span::styled(marker, Style::default().fg(theme::ACCENT)),
        Span::styled("◔ ", Style::default().fg(theme::ACCENT)),
        Span::raw(format!("{}  ", super::pad_or_truncate(title, title_w))),
        Span::styled("queued", theme::muted()),
    ])
}

/// `2026-W29 · wed · day 3 of 7` — the same week frame Progress speaks, minus the
/// now-tick the weeks endpoint doesn't carry. The `weekday · day N of 7` tail is
/// derived from the week's Monday and today; a payload without a Monday shows the
/// bare id.
fn week_header(data: &WeekData) -> Line<'static> {
    let mut spans = vec![Span::styled(data.week.id.clone(), theme::header())];
    if let Some(monday) = data.week.monday {
        let idx = elapsed_index(monday, Zoned::now().date());
        let weekday = monday
            .checked_add((idx as i64).days())
            .map(|d| d.strftime("%a").to_string().to_lowercase())
            .unwrap_or_default();
        spans.push(Span::styled(
            format!(" · {weekday} · day {} of 7", idx + 1),
            theme::muted(),
        ));
    }
    Line::from(spans)
}

/// Which day of the week (0..=6) is being lived: the count of whole days from the
/// week's Monday to `today`, clamped into the week. A future week reads day 1, a
/// closed (past) week day 7 — the same clamp Progress applies to its server tick.
fn elapsed_index(monday: Date, today: Date) -> u32 {
    let mut idx = 0u32;
    let mut day = monday;
    while day < today && idx < 6 {
        match day.tomorrow() {
            Ok(next) => day = next,
            Err(_) => break,
        }
        idx += 1;
    }
    idx
}

/// One plan row: `▌ read  SICP — chapters 2 & 3  ██████████  done   3h10 / 3h`.
/// The kind column, the title, a logged-vs-planned meter tinted by the derived
/// state, the state pill, and the time. `▌` (accent) flags the selected row; a
/// row a timer is running on wins the marker with a green `●` and a green
/// ` live ` pill (§board · live now), overriding the selection mark and derived
/// pill — the running clock is louder than the cursor.
fn plan_row(item: &PlanItem, title_w: usize, selected: bool, live: bool) -> Line<'static> {
    let state = item.retro_state();
    let color = if live {
        theme::SUCCESS
    } else {
        state_color(state)
    };
    let logged = item.logged_minutes.unwrap_or(0);
    let planned = item.size_minutes.unwrap_or(0);
    let fraction = if planned > 0 {
        logged as f64 / planned as f64
    } else if logged > 0 {
        1.0
    } else {
        0.0
    };

    let (marker, marker_color) = if live {
        ("● ", theme::SUCCESS)
    } else if selected {
        ("▌ ", theme::ACCENT)
    } else {
        ("  ", theme::ACCENT)
    };
    let kind = item.kind.as_deref().unwrap_or("");
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(marker.to_string(), Style::default().fg(marker_color)),
        Span::styled(super::pad_or_truncate(kind, 8), theme::muted()),
        Span::raw(format!(
            " {}  ",
            super::pad_or_truncate(&item.title, title_w)
        )),
    ];
    // A tick-free pace bar coloured by the derived state (green while live);
    // empty cells read as the dim `··········` the design shows for an untouched
    // intent.
    spans.extend(widgets::pace_bar(fraction, 0.0, BAR_WIDTH, color, false));
    spans.push(Span::raw("  "));
    spans.push(if live { live_pill() } else { plan_pill(state) });
    spans.push(Span::raw(format!(
        "  {} / {}",
        fmt_hm(logged),
        fmt_hm(planned)
    )));
    Line::from(spans)
}

/// The running-now pill: the board's ` live ` treatment for the item a timer is
/// bound to (green, §board panel). Distinct from the derived `PlanState::Live` —
/// this reads the client's live clock, not the server's canvas state.
fn live_pill() -> Span<'static> {
    Span::styled(
        " live ",
        Style::default().fg(Color::Black).bg(theme::SUCCESS),
    )
}

/// The derived state as a black-ink pill, the shipped `status_pill` idiom keyed
/// to the week's own vocabulary (` done ` / ` live ` / ` hold ` / ` untouched `).
fn plan_pill(state: PlanState) -> Span<'static> {
    let (label, bg) = match state {
        PlanState::Done => (" done ", theme::SUCCESS),
        PlanState::Live => (" live ", theme::ACCENT),
        PlanState::Hold => (" hold ", theme::WARN),
        PlanState::Untouched => (" untouched ", theme::MUTED),
    };
    Span::styled(label, Style::default().fg(Color::Black).bg(bg))
}

fn state_color(state: PlanState) -> Color {
    match state {
        PlanState::Done => theme::SUCCESS,
        PlanState::Live => theme::ACCENT,
        PlanState::Hold => theme::WARN,
        PlanState::Untouched => theme::MUTED,
    }
}

/// `planned 3 · done 1 · 4.2h logged` — the same summary the headless
/// `engineer week` readout prints, read from the server's `planned_vs_done`.
fn summary_line(data: &WeekData) -> Line<'static> {
    let pvd = &data.planned_vs_done;
    Line::from(Span::styled(
        format!(
            "planned {} · done {} · {:.1}h logged",
            pvd.planned,
            pvd.done,
            pvd.logged_minutes as f64 / 60.0
        ),
        theme::muted(),
    ))
}

/// The retro band — the one stored line (`i` writes it in `$EDITOR`). Renders a
/// reflection written offline this session (`provisional`, marked `◔ queued`)
/// over the stored `note.body`; an unwritten week shows the calm empty state
/// that teaches the `i` gesture.
fn retro_lines(data: &WeekData, provisional: Option<&str>) -> Vec<Line<'static>> {
    // A queued reflection is what the user just wrote — show it, marked pending,
    // until the write replays and a live refetch returns the stored note.
    let (body, queued) = match provisional {
        Some(p) => (p.trim(), true),
        None => (data.note.body.trim(), false),
    };

    let mut header = vec![Span::styled(
        "reflection · this week's note",
        theme::muted(),
    )];
    if queued && !body.is_empty() {
        header.push(Span::styled(
            "  ◔ queued",
            Style::default().fg(theme::ACCENT),
        ));
    }
    let mut lines = vec![Line::from(""), Line::from(header)];

    if body.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("No reflection yet. ", theme::muted()),
            Span::styled("i", Style::default().fg(theme::ACCENT)),
            Span::styled(" to write why in $EDITOR.", theme::muted()),
        ]));
    } else {
        for line in body.lines() {
            lines.push(Line::from(Span::raw(line.to_string())));
        }
    }
    lines
}

/// §Week · nothing declared — the calm invitation. Points at the in-app `a`
/// gesture first (it exists now), then the shipped `engineer plan add` one-liner
/// (the design shows both).
fn empty_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Nothing declared for this week yet.",
            theme::muted(),
        )),
        Line::from(vec![
            Span::styled("Say what it's for — ", theme::muted()),
            Span::styled("`a`", Style::default().fg(theme::ACCENT)),
            Span::styled(" to add an intent, or ", theme::muted()),
            Span::styled("`engineer plan add`", theme::muted()),
            Span::styled(" from the shell.", theme::muted()),
        ]),
    ]
}

/// Compact hours:minutes for a row's logged/planned: `0`, `45m`, `1h`, `3h10`.
fn fmt_hm(minutes: u32) -> String {
    if minutes == 0 {
        return "0".to_string();
    }
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WeekData {
        serde_json::from_value(serde_json::json!({
            "week": { "id": "2026-W29", "monday": "2026-07-13", "sunday": "2026-07-19", "closed": false },
            "days": [
                { "items": [
                    { "id": 1, "title": "SICP — chapters 2 & 3", "kind": "reading", "state": "done",
                      "done": true, "size_minutes": 180, "logged_minutes": 190 },
                    { "id": 2, "title": "Systems reading · 2 of 3", "kind": "reading", "state": "planned",
                      "done": false, "size_minutes": 180, "logged_minutes": 115 },
                    { "id": 3, "title": "The Raft paper (revisit)", "kind": "reading", "state": "left",
                      "done": false, "size_minutes": 60, "logged_minutes": 0 }
                ]}
            ],
            "planned_vs_done": { "planned": 3, "done": 1, "logged_minutes": 305, "planned_minutes": 420 },
            "note": { "body": "Read the paper first, build second." }
        }))
        .unwrap()
    }

    fn empty() -> WeekData {
        serde_json::from_value(serde_json::json!({
            "week": { "id": "2026-W30", "monday": "2026-07-20", "sunday": "2026-07-26" },
            "days": [],
            "planned_vs_done": {},
            "note": { "body": "" }
        }))
        .unwrap()
    }

    fn spans_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn ctx() -> (ApiClient, UnboundedSender<Action>) {
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:9/").unwrap(), "t".into());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        (api, tx)
    }

    fn loaded() -> Week {
        Week {
            data: Some(sample()),
            ..Default::default()
        }
    }

    /// A running header snapshot bound to `activity_id` — what the app forwards
    /// from a poll, and what the seam reads to mark the row and name a switch.
    fn running_on(activity_id: i64, label: &str) -> Timer {
        Timer {
            running: true,
            bound: true,
            activity_id: Some(activity_id),
            label: Some(label.into()),
            elapsed_seconds: Some(600),
            ..Default::default()
        }
    }

    /// A per-test scratch (queue.json, cache) so a spawned write lands in a
    /// throwaway dir, never the shared XDG queue.
    fn scratch_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-week-screen-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("queue.json"), dir.join("timer-cache.json"))
    }

    fn render_text(w: &mut Week) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|f| w.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn hints_text(w: &Week) -> String {
        w.hints().spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Poll the reducer's action channel until `pred` matches (or ~1s elapses) —
    /// the way to observe what a spawned write dispatched back.
    async fn wait_for(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Action>,
        pred: impl Fn(&Action) -> bool,
    ) -> bool {
        for _ in 0..100 {
            while let Ok(a) = rx.try_recv() {
                if pred(&a) {
                    return true;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        false
    }

    // --- pure render helpers ---

    #[test]
    fn elapsed_index_counts_days_into_the_week() {
        let mon = jiff::civil::date(2026, 7, 13); // Monday
        assert_eq!(elapsed_index(mon, jiff::civil::date(2026, 7, 13)), 0);
        assert_eq!(elapsed_index(mon, jiff::civil::date(2026, 7, 15)), 2); // Wed
                                                                           // A future week (today before Monday) clamps to day 1.
        assert_eq!(elapsed_index(mon, jiff::civil::date(2026, 7, 10)), 0);
        // A closed week (today past Sunday) clamps to day 7.
        assert_eq!(elapsed_index(mon, jiff::civil::date(2026, 8, 1)), 6);
    }

    #[test]
    fn week_header_shows_id_and_the_day_frame() {
        let text = spans_text(&week_header(&sample()));
        assert!(text.contains("2026-W29"), "{text}");
        assert!(text.contains("day "), "{text}");
        assert!(text.contains(" of 7"), "{text}");
    }

    #[test]
    fn plan_row_renders_pill_and_logged_vs_planned() {
        let data = sample();
        let items: Vec<&PlanItem> = data.items().collect();

        let done = spans_text(&plan_row(items[0], 24, false, false));
        assert!(done.contains("SICP"), "{done}");
        assert!(done.contains("done"), "{done}");
        // 190 logged / 180 planned → 3h10 / 3h.
        assert!(done.contains("3h10 / 3h"), "{done}");

        let hold = spans_text(&plan_row(items[1], 24, false, false));
        assert!(hold.contains("hold"), "{hold}");
        assert!(hold.contains("1h55 / 3h"), "{hold}");

        let untouched = spans_text(&plan_row(items[2], 24, false, false));
        assert!(untouched.contains("untouched"), "{untouched}");
        assert!(untouched.contains("0 / 1h"), "{untouched}");
    }

    #[test]
    fn plan_row_marks_the_selected_row() {
        let data = sample();
        let item = data.items().next().unwrap();
        assert!(spans_text(&plan_row(item, 24, true, false)).starts_with('▌'));
        assert!(!spans_text(&plan_row(item, 24, false, false)).starts_with('▌'));
    }

    #[test]
    fn plan_row_marks_the_running_row_live() {
        let data = sample();
        let item = data.items().next().unwrap();
        // A live row wins the marker with a green `●` and shows the ` live ` pill,
        // overriding both the selection `▌` and the derived ` done ` pill.
        let text = spans_text(&plan_row(item, 24, true, true));
        assert!(
            text.starts_with('●'),
            "the live marker wins the column: {text}"
        );
        assert!(text.contains("live"), "{text}");
        assert!(
            !text.contains("done"),
            "the live pill replaces the derived one: {text}"
        );
    }

    #[test]
    fn summary_line_reads_planned_done_logged() {
        // 305 logged minutes → 5.1h.
        assert_eq!(
            spans_text(&summary_line(&sample())),
            "planned 3 · done 1 · 5.1h logged"
        );
    }

    fn retro_text(data: &WeekData, provisional: Option<&str>) -> String {
        retro_lines(data, provisional)
            .iter()
            .map(spans_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn retro_band_shows_the_note_then_the_empty_state() {
        let written = retro_text(&sample(), None);
        assert!(
            written.contains("reflection · this week's note"),
            "{written}"
        );
        assert!(written.contains("Read the paper first"), "{written}");
        assert!(!written.contains("◔ queued"), "a stored note is not queued");

        let blank = retro_text(&empty(), None);
        assert!(blank.contains("No reflection yet."), "{blank}");
        // The empty state teaches the `i` gesture, per the design.
        assert!(blank.contains('i') && blank.contains("$EDITOR"), "{blank}");
    }

    #[test]
    fn retro_band_marks_a_queued_reflection() {
        // A reflection written offline renders over the stored note, marked
        // `◔ queued` until the write replays.
        let text = retro_text(&sample(), Some("Next week: read the paper first."));
        assert!(text.contains("◔ queued"), "{text}");
        assert!(text.contains("Next week: read the paper first."), "{text}");
        assert!(
            !text.contains("Read the paper first, build second"),
            "the local text supersedes the stored note: {text}"
        );
    }

    #[test]
    fn nothing_declared_points_at_a_first_then_plan_add() {
        let text: String = empty_lines()
            .iter()
            .map(|l| spans_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Nothing declared"), "{text}");
        // The gesture exists now — `a` is taught first, the shell verb second.
        let a_at = text.find("`a`").expect("teaches the `a` gesture");
        let verb_at = text
            .find("engineer plan add")
            .expect("teaches the shell verb");
        assert!(a_at < verb_at, "`a` comes first: {text}");
    }

    #[test]
    fn fmt_hm_reads_compact_hours_minutes() {
        assert_eq!(fmt_hm(0), "0");
        assert_eq!(fmt_hm(45), "45m");
        assert_eq!(fmt_hm(60), "1h");
        assert_eq!(fmt_hm(180), "3h");
        assert_eq!(fmt_hm(190), "3h10");
    }

    // --- reducer (Action in → state out) ---

    #[tokio::test]
    async fn load_populates_rows_and_clears_loading() {
        let (api, tx) = ctx();
        let mut w = Week {
            loading: true,
            ..Default::default()
        };
        w.handle(Action::WeekLoaded(Box::new(sample())), &api, &tx)
            .await;
        assert!(!w.loading);
        assert!(w.error.is_none());
        assert_eq!(w.item_count(), 3);
    }

    #[tokio::test]
    async fn load_failed_surfaces_the_error() {
        let (api, tx) = ctx();
        let mut w = Week {
            loading: true,
            ..Default::default()
        };
        w.handle(Action::WeekLoadFailed("boom".into()), &api, &tx)
            .await;
        assert!(!w.loading);
        assert_eq!(w.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn stepping_changes_the_week_and_reloads() {
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(Action::WeekStep(-1), &api, &tx).await;
        assert_eq!(w.offset, -1);
        assert!(w.loading, "a step kicks off a refetch");

        // `t` snaps back to this week and reloads.
        w.handle(Action::WeekReset, &api, &tx).await;
        assert_eq!(w.offset, 0);
        assert!(w.loading);
    }

    #[tokio::test]
    async fn week_reset_is_a_noop_at_the_current_week() {
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(Action::WeekReset, &api, &tx).await;
        assert_eq!(w.offset, 0);
        assert!(!w.loading, "already on this week — no refetch");
    }

    #[tokio::test]
    async fn select_move_clamps_within_the_plan() {
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(Action::WeekSelectMove(1), &api, &tx).await;
        assert_eq!(w.selected, 1);
        w.handle(Action::WeekSelectMove(9), &api, &tx).await;
        assert_eq!(w.selected, 2, "clamped at the last row");
        w.handle(Action::WeekSelectMove(-9), &api, &tx).await;
        assert_eq!(w.selected, 0, "clamped at the first row");
    }

    #[tokio::test]
    async fn load_keeps_the_cursor_in_range_as_the_plan_shrinks() {
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(Action::WeekSelectMove(2), &api, &tx).await;
        assert_eq!(w.selected, 2);
        // A week with one item pulls the cursor back to the only row.
        w.handle(Action::WeekLoaded(Box::new(one_item())), &api, &tx)
            .await;
        assert_eq!(w.selected, 0);
    }

    fn one_item() -> WeekData {
        serde_json::from_value(serde_json::json!({
            "week": { "id": "2026-W28", "monday": "2026-07-06" },
            "days": [ { "items": [
                { "id": 9, "title": "one thing", "state": "planned", "done": false }
            ]}],
            "planned_vs_done": { "planned": 1, "done": 0 },
            "note": { "body": "" }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn empty_week_renders_the_teaching_copy() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut w = Week {
            data: Some(empty()),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|f| w.render(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Nothing declared"), "{text}");
        assert!(text.contains("engineer plan add"), "{text}");
    }

    #[tokio::test]
    async fn loaded_week_renders_rows_and_the_summary() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut w = loaded();
        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|f| w.render(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("2026-W29"), "{text}");
        assert!(text.contains("done"), "{text}");
        assert!(text.contains("planned 3 · done 1"), "{text}");
    }

    // --- plan writes (#115): the input workflow, adjust, drop, offline ---

    #[test]
    fn planned_on_is_today_for_the_current_week_else_the_shown_monday() {
        // offset 0 → today, regardless of the loaded payload.
        let w = Week {
            offset: 0,
            data: Some(empty()),
            ..Default::default()
        };
        assert_eq!(w.planned_on(), Zoned::now().date());
        // A stepped week anchors to that week's Monday (one_item's monday).
        let w = Week {
            offset: -1,
            data: Some(one_item()),
            ..Default::default()
        };
        assert_eq!(w.planned_on(), jiff::civil::date(2026, 7, 6));
    }

    #[tokio::test]
    async fn add_opens_the_input_types_and_esc_cancels() {
        let (api, tx) = ctx();
        let mut w = Week {
            data: Some(empty()),
            ..Default::default()
        };

        w.handle(Action::WeekAddBegin, &api, &tx).await;
        for c in "one systems paper".chars() {
            w.handle(Action::WeekInputChar(c), &api, &tx).await;
        }
        let text = render_text(&mut w);
        assert!(text.contains("INSERT"), "the insert banner shows: {text}");
        assert!(text.contains("Add intent"), "{text}");
        assert!(
            text.contains("one systems paper"),
            "the buffer renders: {text}"
        );
        // Backspace edits the buffer in place.
        w.handle(Action::WeekInputBackspace, &api, &tx).await;
        assert!(
            render_text(&mut w).contains("one systems pape"),
            "backspace trims the last char"
        );

        // The hints speak the insert grammar while the input is open.
        let h = hints_text(&w);
        assert!(h.contains("declare") && h.contains("cancel"), "{h}");

        // Esc closes it — the banner is gone, the board reads normally again.
        w.handle(Action::WeekInputCancel, &api, &tx).await;
        assert!(!render_text(&mut w).contains("INSERT"), "cancelled");
        assert!(hints_text(&w).contains("add"), "back to the board hints");
    }

    #[tokio::test]
    async fn enter_declares_via_create_for_the_shown_week() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let today = Zoned::now().date().to_string();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/activities"))
            .and(body_partial_json(serde_json::json!({
                "activity": { "title": "one systems paper", "planned_on": today }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7, "title": "one systems paper", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(empty()),
            queue_paths: Some(scratch_paths()),
            ..Default::default()
        };

        w.handle(Action::WeekAddBegin, &api, &tx).await;
        for c in "one systems paper".chars() {
            w.handle(Action::WeekInputChar(c), &api, &tx).await;
        }
        w.handle(Action::WeekInputSubmit, &api, &tx).await;

        // A confirmed declare refetches the week — and the mock's expect(1)
        // verifies the create body (title + planned_on for the shown week).
        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::RefreshWeek)).await,
            "a confirmed declare refetches the week"
        );
    }

    #[tokio::test]
    async fn empty_buffer_declares_nothing() {
        // `a` then straight `⏎` never declares a nameless intent — the input
        // just closes, nothing spawns, the queue stays empty.
        use crate::queue::QueueStore;
        let (queue_path, cache_path) = scratch_paths();
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:1/").unwrap(), "t".into());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(empty()),
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Default::default()
        };
        w.handle(Action::WeekAddBegin, &api, &tx).await;
        w.handle(Action::WeekInputSubmit, &api, &tx).await;
        assert!(!render_text(&mut w).contains("INSERT"), "the input closed");
        // Give any (non-existent) spawn a beat, then prove the queue is untouched.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(QueueStore::at(&queue_path).pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn adjust_patches_the_selected_title() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/1"))
            .and(body_partial_json(serde_json::json!({
                "activity": { "title": "SICP — chapters 2 & 3 (rev)" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1, "title": "SICP — chapters 2 & 3 (rev)", "status": "planned"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some(scratch_paths()),
            ..Default::default()
        };
        // The selected row is 0 → id 1; `e` prefills its title, append then save.
        w.handle(Action::WeekAdjustBegin, &api, &tx).await;
        assert!(
            render_text(&mut w).contains("SICP — chapters 2 & 3"),
            "the editor prefills the current title"
        );
        for c in " (rev)".chars() {
            w.handle(Action::WeekInputChar(c), &api, &tx).await;
        }
        w.handle(Action::WeekInputSubmit, &api, &tx).await;

        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::RefreshWeek)).await,
            "a confirmed adjust refetches the week"
        );
    }

    #[tokio::test]
    async fn drop_arms_then_confirms_and_archives() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/activities/1/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1, "title": "SICP — chapters 2 & 3", "archived_at": "2026-07-16T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some(scratch_paths()),
            ..Default::default()
        };
        // First press arms and asks to confirm; the board shows the prompt.
        let warn = w.handle(Action::WeekDrop, &api, &tx).await;
        assert!(
            matches!(warn, Some((Level::Warning, _))),
            "first press asks to confirm"
        );
        assert!(
            render_text(&mut w).contains("press d again"),
            "the confirm prompt shows"
        );
        // Second press confirms → archive spawned, then refetch.
        let second = w.handle(Action::WeekDrop, &api, &tx).await;
        assert!(second.is_none(), "the confirm doesn't re-warn");
        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::RefreshWeek)).await,
            "a confirmed drop refetches the week"
        );
    }

    #[tokio::test]
    async fn moving_the_cursor_disarms_a_pending_drop() {
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(Action::WeekDrop, &api, &tx).await; // arm
        assert!(render_text(&mut w).contains("press d again"));
        w.handle(Action::WeekSelectMove(1), &api, &tx).await; // disarm
        assert!(
            !render_text(&mut w).contains("press d again"),
            "moving the cursor disarms the drop"
        );
    }

    #[tokio::test]
    async fn offline_add_enqueues_and_renders_the_provisional_row() {
        // The full wiring, offline: `a` → the declare helper → `QueuedClient` →
        // the persisted queue, plus the `◔ … queued` provisional row the board
        // renders until the create replays. A dead port forces the offline arm.
        use crate::queue::{IntentKind, QueueStore};

        let (queue_path, cache_path) = scratch_paths();
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:1/").unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(empty()),
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Default::default()
        };

        w.handle(Action::WeekAddBegin, &api, &tx).await;
        for c in "one systems paper".chars() {
            w.handle(Action::WeekInputChar(c), &api, &tx).await;
        }
        w.handle(Action::WeekInputSubmit, &api, &tx).await;

        // The spawned write lands in the queue (the dead port refuses fast).
        let store = QueueStore::at(&queue_path);
        let mut pending = store.pending().unwrap();
        for _ in 0..100 {
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            pending = store.pending().unwrap();
        }
        assert_eq!(pending.len(), 1, "the declare landed in the queue");
        assert_eq!(pending[0].kind.word(), "plan");
        match &pending[0].kind {
            IntentKind::ActivityCreate { body } => {
                assert_eq!(body.title, "one systems paper");
                assert_eq!(
                    body.planned_on,
                    Some(Zoned::now().date()),
                    "planned_on for the shown (current) week"
                );
            }
            other => panic!("expected an ActivityCreate intent, got {other:?}"),
        }

        // The spawn streamed the provisional row back; feed it and render it.
        assert!(
            wait_for(&mut rx, |a| {
                matches!(a, Action::WeekPlanQueued(t) if t == "one systems paper")
            })
            .await,
            "the provisional-row action is streamed back"
        );
        w.handle(
            Action::WeekPlanQueued("one systems paper".into()),
            &api,
            &tx,
        )
        .await;
        let text = render_text(&mut w);
        assert!(text.contains('◔'), "the queued marker shows: {text}");
        assert!(text.contains("one systems paper"), "{text}");
        assert!(text.contains("queued"), "{text}");
    }

    #[tokio::test]
    async fn a_live_reload_clears_the_provisional_rows() {
        // Server truth supersedes the pending render: a WeekLoaded (a synced
        // refetch) drops the `◔ queued` stand-ins.
        let (api, tx) = ctx();
        let mut w = Week {
            data: Some(empty()),
            ..Default::default()
        };
        w.handle(
            Action::WeekPlanQueued("one systems paper".into()),
            &api,
            &tx,
        )
        .await;
        assert!(render_text(&mut w).contains("one systems paper"));
        w.handle(Action::WeekLoaded(Box::new(sample())), &api, &tx)
            .await;
        let text = render_text(&mut w);
        assert!(
            !text.contains("one systems paper"),
            "provisional rows cleared"
        );
        assert!(text.contains("SICP"), "the server rows render");
    }

    // --- the timer seam (#116): start-on-plan, switch-confirm, offline, refusal ---

    #[tokio::test]
    async fn s_starts_the_timer_bound_to_the_selected_row() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Nothing running → a plain bound start: the activity id, no switch flag.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(serde_json::json!({ "activity_id": 1 })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some(scratch_paths()),
            ..Default::default()
        };
        // Selected row 0 → activity id 1; nothing running → an immediate start.
        let out = w.handle(Action::WeekStartTimer, &api, &tx).await;
        assert!(
            out.is_none(),
            "a first start with nothing running doesn't warn"
        );
        // The confirmed start forwards the fresh snapshot; the mock's expect(1)
        // asserts the body carried activity_id 1 with no switch.
        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::TimerLoaded(_))).await,
            "a confirmed start forwards the header snapshot"
        );
    }

    #[tokio::test]
    async fn s_switch_confirm_stops_and_switches_on_the_second_press() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The switch start carries switch:true — the server stops & saves first.
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .and(body_json(
                serde_json::json!({ "activity_id": 1, "switch": true }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": true, "activity_id": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some(scratch_paths()),
            // A timer already runs on a *different* activity (99).
            running: Some(running_on(99, "Read DDIA ch.7")),
            ..Default::default()
        };

        // First press warns, naming the running session — nothing spawns yet.
        let (level, text) = w
            .handle(Action::WeekStartTimer, &api, &tx)
            .await
            .expect("the first press warns");
        assert!(matches!(level, Level::Warning));
        assert!(
            text.contains("Read DDIA ch.7"),
            "names the running session: {text}"
        );
        assert!(text.contains("again"), "asks for a second press: {text}");

        // Second press switches → the switch:true start spawns (mock asserts it).
        let second = w.handle(Action::WeekStartTimer, &api, &tx).await;
        assert!(second.is_none(), "the confirm doesn't re-warn");
        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::TimerLoaded(_))).await,
            "a confirmed switch forwards the snapshot"
        );
    }

    #[tokio::test]
    async fn s_on_the_running_row_says_already_timing() {
        // Starting the row a timer already runs on would only stop & restart the
        // same work — inform instead of churning a segment.
        let (api, tx) = ctx();
        let mut w = Week {
            data: Some(sample()),
            running: Some(running_on(1, "SICP — chapters 2 & 3")),
            ..Default::default()
        };
        let (level, text) = w
            .handle(Action::WeekStartTimer, &api, &tx)
            .await
            .expect("starting the already-running row informs");
        assert!(matches!(level, Level::Info));
        assert!(text.contains("already timing"), "{text}");
    }

    #[tokio::test]
    async fn offline_start_enqueues_the_timer_start_intent() {
        // Offline: `s` → the seam helper → `QueuedClient` → the persisted queue.
        // The shipped `TimerStart` intent carries the plan item's activity id.
        use crate::queue::{IntentKind, QueueStore};

        let (queue_path, cache_path) = scratch_paths();
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:1/").unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Default::default()
        };
        // Row 0 → activity id 1; the dead port forces the offline arm.
        w.handle(Action::WeekStartTimer, &api, &tx).await;

        let store = QueueStore::at(&queue_path);
        let mut pending = store.pending().unwrap();
        for _ in 0..100 {
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            pending = store.pending().unwrap();
        }
        assert_eq!(pending.len(), 1, "the start landed in the queue");
        assert_eq!(pending[0].kind.word(), "start");
        match &pending[0].kind {
            IntentKind::TimerStart {
                activity_id,
                switch,
                ..
            } => {
                assert_eq!(
                    *activity_id,
                    Some(1),
                    "the intent carries the plan item's activity id"
                );
                assert!(!*switch, "nothing running → no switch");
            }
            other => panic!("expected a TimerStart intent, got {other:?}"),
        }
        // The provisional clock streams back to mark the row live.
        assert!(
            wait_for(&mut rx, |a| matches!(a, Action::TimerProvisional(_))).await,
            "the provisional snapshot is streamed back"
        );
    }

    #[tokio::test]
    async fn s_on_a_queued_row_refuses_still_queued() {
        // A provisional (offline-declared) row has no server activity yet — the
        // start refuses honestly rather than binding to a phantom id.
        let (api, tx) = ctx();
        let mut w = Week {
            data: Some(empty()),
            ..Default::default()
        };
        // One offline-declared row, cursor on it (no real rows above it).
        w.handle(
            Action::WeekPlanQueued("one systems paper".into()),
            &api,
            &tx,
        )
        .await;
        assert_eq!(w.selected, 0);
        let (level, text) = w
            .handle(Action::WeekStartTimer, &api, &tx)
            .await
            .expect("a queued row refuses the start");
        assert!(matches!(level, Level::Warning));
        assert!(text.contains("still queued"), "{text}");
    }

    #[tokio::test]
    async fn a_forwarded_timer_snapshot_marks_the_running_row_live() {
        // The app forwards the header poll's snapshot to the current screen; the
        // board caches it (while running) and marks the bound row ` live `.
        let (api, tx) = ctx();
        let mut w = loaded();
        w.handle(
            Action::TimerLoaded(Box::new(running_on(2, "systems"))),
            &api,
            &tx,
        )
        .await;
        let text = render_text(&mut w);
        assert!(text.contains('●'), "the live marker renders: {text}");
        assert!(text.contains("live"), "{text}");
        // A stopped snapshot clears the mark.
        w.handle(Action::TimerLoaded(Box::default()), &api, &tx)
            .await;
        assert!(
            !render_text(&mut w).contains('●'),
            "a stopped clock clears the live marker"
        );
    }

    // --- reflection (#117): the $EDITOR retro write ---

    /// An api + a live receiver — the reflect hand-off tests observe what `i`
    /// dispatches, so they can't drop the rx the way `ctx` does.
    fn ctx_rx() -> (
        ApiClient,
        UnboundedSender<Action>,
        tokio::sync::mpsc::UnboundedReceiver<Action>,
    ) {
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:9/").unwrap(), "t".into());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (api, tx, rx)
    }

    #[tokio::test]
    async fn reflect_opens_the_editor_seeded_with_the_current_note() {
        // `i` reads the shown week's stored note and hands the seed off to the
        // app (which owns the terminal suspend + $EDITOR spawn).
        let (api, tx, mut rx) = ctx_rx();
        let mut w = loaded();
        w.handle(Action::WeekReflect, &api, &tx).await;
        assert!(
            wait_for(&mut rx, |a| matches!(
                a,
                Action::WeekReflectEdit { iso_week, seed }
                    if iso_week == "2026-W29" && seed == "Read the paper first, build second."
            ))
            .await,
            "the reflect hand-off carries the week id and the current note as the seed"
        );
    }

    #[tokio::test]
    async fn reflect_before_load_is_a_noop() {
        let (api, tx, mut rx) = ctx_rx();
        let mut w = Week::default(); // never loaded
        w.handle(Action::WeekReflect, &api, &tx).await;
        assert!(rx.try_recv().is_err(), "no hand-off before the week loads");
    }

    #[tokio::test]
    async fn reflect_abort_leaves_the_note_untouched() {
        // Quit-without-write cancels — the board only says so (capture-is-sacred).
        let (api, tx) = ctx();
        let mut w = loaded();
        let (level, text) = w
            .handle(Action::WeekReflectAbort, &api, &tx)
            .await
            .expect("an abort notifies");
        assert!(matches!(level, Level::Info));
        assert!(text.contains("unchanged"), "{text}");
    }

    #[tokio::test]
    async fn reflect_save_offline_enqueues_and_renders_queued() {
        // The full wiring, offline: a save → the reflect helper → `QueuedClient`
        // → the persisted queue, plus the `◔ queued` retro band. A dead port
        // forces the offline arm.
        use crate::queue::{IntentKind, QueueStore};

        let (queue_path, cache_path) = scratch_paths();
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:1/").unwrap(), "t".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Week {
            data: Some(sample()),
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Default::default()
        };

        w.handle(
            Action::WeekReflectSave {
                iso_week: "2026-W29".into(),
                body: "Next week: read the paper first.".into(),
            },
            &api,
            &tx,
        )
        .await;

        // The spawned write lands in the queue (the dead port refuses fast).
        let store = QueueStore::at(&queue_path);
        let mut pending = store.pending().unwrap();
        for _ in 0..100 {
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            pending = store.pending().unwrap();
        }
        assert_eq!(pending.len(), 1, "the reflection landed in the queue");
        assert_eq!(pending[0].kind.word(), "reflect");
        match &pending[0].kind {
            IntentKind::WeekNoteWrite { iso_week, body } => {
                assert_eq!(iso_week, "2026-W29");
                assert_eq!(body, "Next week: read the paper first.");
            }
            other => panic!("expected a WeekNoteWrite intent, got {other:?}"),
        }

        // The spawn streamed the provisional note back; feed it and render it.
        assert!(
            wait_for(&mut rx, |a| {
                matches!(a, Action::WeekReflectQueued(b) if b == "Next week: read the paper first.")
            })
            .await,
            "the provisional-note action is streamed back"
        );
        w.handle(
            Action::WeekReflectQueued("Next week: read the paper first.".into()),
            &api,
            &tx,
        )
        .await;
        let text = render_text(&mut w);
        assert!(text.contains("◔ queued"), "the queued marker shows: {text}");
        assert!(text.contains("Next week: read the paper first."), "{text}");
    }
}
