//! Timer screen — the bind / pause / stop moments plus the "clock first, name
//! it later" blank-start flow (daily-loop.brief.md §5, timer.html). The
//! persistent header cell is rendered by the chrome from the app-owned
//! snapshot; this screen owns the interactions.
//!
//! States, driven by the live snapshot:
//! - **Absent** — no live timer. `s` starts a blank clock ("name it later").
//! - **Live, unbound** — a running blank timer. `/` opens a candidate search to
//!   bind it (select an activity, or type a title to mint a new one); `p`
//!   pauses/resumes; `d` discards (an unbound timer has no segment to write).
//! - **Live, bound** — an activity + elapsed; `p` pauses/resumes, `x` stops.
//! - **Stopped** — the written segment (minutes + activity) so the ledger is
//!   trusted; `↵` dismisses.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Timer as TimerSnapshot, TimerCandidate, TimerStopped};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// The four timer sub-verbs the `:` palette dispatches (`:timer start|pause|
/// resume|stop`). Defined here, next to the actions they drive, so the grammar
/// table and this screen share one spelling of the inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerVerb {
    Start,
    Pause,
    Resume,
    Stop,
}

impl TimerVerb {
    /// Canonical names, in help/completion order. The grammar table's argument
    /// set and `from_name` both read from here.
    pub const NAMES: &'static [&'static str] = &["start", "pause", "resume", "stop"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Self::Start),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

/// Run a `:timer <verb>` palette action against the app-owned snapshot, without
/// routing through the screen (so it works from any screen and never races the
/// screen's own load). Valid transitions spawn the same API op the on-screen
/// keys do; the header cell reflects the result. An invalid transition returns
/// the notify-tile warning to surface — the unbound-stop message matches the one
/// the Timer screen shows for the same mistake.
pub(crate) fn palette_dispatch(
    verb: TimerVerb,
    snap: Option<&TimerSnapshot>,
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
) -> Option<(Level, String)> {
    let running = snap.is_some_and(|s| s.running);
    let paused = snap.is_some_and(|s| s.paused);
    let bound = snap.is_some_and(|s| s.bound);
    match verb {
        TimerVerb::Start => {
            if running {
                Some((Level::Warning, "a timer is already running".into()))
            } else {
                spawn_start_blank(api, tx);
                None
            }
        }
        TimerVerb::Pause => {
            if !running {
                Some((Level::Warning, "no timer running".into()))
            } else if paused {
                Some((Level::Warning, "timer is already paused".into()))
            } else {
                spawn_op(api, tx, TimerOp::Pause);
                None
            }
        }
        TimerVerb::Resume => {
            if !running {
                Some((Level::Warning, "no timer running".into()))
            } else if !paused {
                Some((Level::Warning, "timer isn't paused".into()))
            } else {
                spawn_op(api, tx, TimerOp::Resume);
                None
            }
        }
        TimerVerb::Stop => {
            if !running {
                Some((Level::Warning, "no timer to stop".into()))
            } else if bound {
                spawn_stop(api, tx);
                None
            } else {
                Some((
                    Level::Warning,
                    "bind the timer before stopping (or `d` to discard)".into(),
                ))
            }
        }
    }
}

/// Displayed elapsed for a snapshot: the last server `elapsed_seconds` plus the
/// monotonic time since it was fetched — but only while actually advancing (a
/// paused clock is frozen). Shared with the header cell so both tick in step.
pub(crate) fn live_elapsed(snap: &TimerSnapshot, base: Option<Instant>) -> i64 {
    let base_secs = snap.elapsed_seconds.unwrap_or(0);
    if snap.running && !snap.paused {
        base_secs + base.map(|b| b.elapsed().as_secs() as i64).unwrap_or(0)
    } else {
        base_secs
    }
}

/// What a bind submission would act on, resolved from the current selection.
enum BindTarget {
    Existing(i64),
    Create(String),
}

#[derive(Default)]
enum Stage {
    #[default]
    Loading,
    Absent,
    Live,
    Stopped {
        result: TimerStopped,
        label: Option<String>,
    },
}

#[derive(Default)]
pub struct Timer {
    stage: Stage,
    snapshot: Option<TimerSnapshot>,
    /// Monotonic baseline for ticking the displayed elapsed between snapshots.
    base: Option<Instant>,
    // Bind panel (running-unbound → search candidates or mint a new activity).
    binding: bool,
    query: String,
    candidates: Vec<TimerCandidate>,
    cand_state: ListState,
}

impl Timer {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.stage = Stage::Loading;
        spawn_load(api, tx);
    }

    /// While the bind panel is open, the screen owns every key (a live search):
    /// characters filter, arrows pick, Enter binds/creates, Esc closes. This
    /// runs before the global keymap, so the timer keeps running untouched.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if !self.binding {
            return None;
        }
        match key.code {
            KeyCode::Esc => Some(Action::TimerBindCancel),
            KeyCode::Enter => Some(Action::TimerBindSubmit),
            KeyCode::Backspace => Some(Action::TimerBindBackspace),
            KeyCode::Up => Some(Action::TimerBindMove(-1)),
            KeyCode::Down => Some(Action::TimerBindMove(1)),
            KeyCode::Char(c) => Some(Action::TimerBindInput(c)),
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
            // Snapshot update (from on_enter, the header poll, or a completed
            // op). A pending stop confirmation is preserved — the user hasn't
            // acknowledged the written segment yet.
            Action::TimerLoaded(t) => {
                if matches!(self.stage, Stage::Stopped { .. }) {
                    return None;
                }
                let t = *t;
                self.stage = if t.running {
                    Stage::Live
                } else {
                    Stage::Absent
                };
                if t.bound || !t.running {
                    self.close_bind_panel();
                }
                self.base = Some(Instant::now());
                self.snapshot = Some(t);
            }
            Action::TimerReload => spawn_load(api, tx),
            Action::TimerStartBlank => {
                if matches!(self.stage, Stage::Absent) {
                    spawn_start_blank(api, tx);
                }
            }
            Action::TimerPauseResume => {
                if matches!(self.stage, Stage::Live) {
                    if self.snapshot.as_ref().is_some_and(|s| s.paused) {
                        spawn_op(api, tx, TimerOp::Resume);
                    } else {
                        spawn_op(api, tx, TimerOp::Pause);
                    }
                }
            }
            Action::TimerStop => {
                if !matches!(self.stage, Stage::Live) {
                    return None;
                }
                if self.snapshot.as_ref().is_some_and(|s| s.bound) {
                    spawn_stop(api, tx);
                } else {
                    return Some((
                        Level::Warning,
                        "bind the timer before stopping (or `d` to discard)".into(),
                    ));
                }
            }
            Action::TimerStopped(result) => {
                let label = self.snapshot.as_ref().and_then(|s| s.label.clone());
                self.close_bind_panel();
                self.stage = Stage::Stopped {
                    result: *result,
                    label,
                };
            }
            Action::TimerDismissStopped => {
                if matches!(self.stage, Stage::Stopped { .. }) {
                    self.stage = Stage::Loading;
                    spawn_load(api, tx);
                }
            }
            Action::TimerDiscard => {
                let unbound = self.snapshot.as_ref().is_some_and(|s| !s.bound);
                if matches!(self.stage, Stage::Live) && unbound {
                    spawn_discard(api, tx);
                }
            }
            Action::TimerBindBegin => {
                let unbound = self.snapshot.as_ref().is_some_and(|s| !s.bound);
                if matches!(self.stage, Stage::Live) && unbound {
                    self.binding = true;
                    self.query.clear();
                    self.candidates.clear();
                    self.cand_state.select(Some(0));
                    spawn_candidates(api, tx, String::new());
                }
            }
            Action::TimerBindCancel => self.close_bind_panel(),
            Action::TimerBindInput(c) => {
                self.query.push(c);
                self.cand_state.select(Some(0));
                spawn_candidates(api, tx, self.query.clone());
            }
            Action::TimerBindBackspace => {
                self.query.pop();
                self.cand_state.select(Some(0));
                spawn_candidates(api, tx, self.query.clone());
            }
            Action::TimerBindMove(delta) => self.move_selection(delta),
            Action::TimerCandidatesLoaded(list) => {
                self.candidates = list;
                let len = self.bind_rows_len();
                if len == 0 {
                    self.cand_state.select(None);
                } else if self.cand_state.selected().unwrap_or(0) >= len {
                    self.cand_state.select(Some(len - 1));
                }
            }
            Action::TimerBindSubmit => {
                if let Some(target) = self.bind_target() {
                    self.close_bind_panel();
                    spawn_bind(api, tx, target);
                }
            }
            _ => {}
        }
        None
    }

    fn close_bind_panel(&mut self) {
        self.binding = false;
        self.query.clear();
        self.candidates.clear();
    }

    /// Candidate rows plus a trailing "create" row when the query is non-empty.
    fn bind_rows_len(&self) -> usize {
        self.candidates.len() + usize::from(!self.query.trim().is_empty())
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.bind_rows_len();
        if len == 0 {
            self.cand_state.select(None);
            return;
        }
        let cur = self.cand_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.cand_state.select(Some(next as usize));
    }

    fn bind_target(&self) -> Option<BindTarget> {
        let sel = self.cand_state.selected()?;
        if sel < self.candidates.len() {
            Some(BindTarget::Existing(self.candidates[sel].id))
        } else if !self.query.trim().is_empty() {
            Some(BindTarget::Create(self.query.trim().to_string()))
        } else {
            None
        }
    }

    fn elapsed_line(&self) -> Line<'static> {
        let Some(snap) = self.snapshot.as_ref() else {
            return Line::from("");
        };
        let secs = live_elapsed(snap, self.base);
        let (glyph, color, word) = if snap.paused {
            ("‖", theme::WARN, "paused")
        } else {
            ("●", theme::ACCENT, "running")
        };
        Line::from(vec![
            Span::styled(
                format!("{glyph} {}", widgets::fmt_elapsed(secs)),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   {word}"), theme::muted()),
        ])
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match &self.stage {
            Stage::Loading => {
                frame.render_widget(Paragraph::new("loading…").block(bordered("Timer")), area);
            }
            Stage::Absent => {
                let lines = vec![
                    Line::from(""),
                    Line::from("No timer running."),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("Press "),
                        Span::styled("s", theme::focused()),
                        Span::raw(" to start the clock — name it later."),
                    ]),
                ];
                frame.render_widget(Paragraph::new(lines).block(bordered("Timer")), area);
            }
            Stage::Stopped { result, label } => {
                let activity = label
                    .clone()
                    .unwrap_or_else(|| format!("activity #{}", result.activity_id));
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "✓ segment written",
                        Style::default()
                            .fg(theme::SUCCESS)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!("{} min", result.minutes),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  →  ", theme::muted()),
                        Span::raw(activity),
                    ]),
                    Line::from(Span::styled(
                        format!("segment #{}", result.segment_id),
                        theme::muted(),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("Press "),
                        Span::styled("↵", theme::focused()),
                        Span::raw(" to dismiss."),
                    ]),
                ];
                frame.render_widget(
                    Paragraph::new(lines).block(bordered("Timer · stopped")),
                    area,
                );
            }
            Stage::Live if self.binding => self.render_bind_panel(frame, area),
            Stage::Live => {
                let bound = self.snapshot.as_ref().is_some_and(|s| s.bound);
                let mut lines = vec![self.elapsed_line(), Line::from("")];
                if bound {
                    let label = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.label.clone())
                        .unwrap_or_default();
                    lines.push(Line::from(Span::styled(
                        label,
                        Style::default().add_modifier(Modifier::BOLD),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "Not bound to an activity yet.",
                        theme::muted(),
                    )));
                    lines.push(Line::from(Span::styled(
                        "Press / to bind it, or d to discard.",
                        theme::muted(),
                    )));
                }
                frame.render_widget(Paragraph::new(lines).block(bordered("Timer")), area);
            }
        }
    }

    fn render_bind_panel(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Timer · bind");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner);

        let header = vec![
            self.elapsed_line(),
            Line::from(""),
            Line::from(vec![
                Span::raw("bind to  "),
                Span::styled(format!("{}_", self.query), theme::focused()),
            ]),
        ];
        frame.render_widget(Paragraph::new(header), rows[0]);

        let mut items: Vec<ListItem> = self
            .candidates
            .iter()
            .map(|c| ListItem::new(Line::from(c.title.clone())))
            .collect();
        if !self.query.trim().is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                format!("+ new activity: \"{}\"", self.query.trim()),
                Style::default().fg(theme::ACCENT),
            ))));
        }
        if items.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "type to search your activities…",
                theme::muted(),
            ))));
        }

        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, rows[1], &mut self.cand_state);
    }

    pub fn hints(&self) -> Line<'static> {
        match &self.stage {
            Stage::Loading => widgets::footer_hints(&[("h", "home")]),
            Stage::Absent => widgets::footer_hints(&[("s", "start"), ("h", "home")]),
            Stage::Stopped { .. } => widgets::footer_hints(&[("↵", "dismiss"), ("h", "home")]),
            Stage::Live if self.binding => Line::from(Span::styled(
                "type to search · ↑/↓ pick · ↵ bind/create · Esc cancel",
                theme::muted(),
            )),
            Stage::Live => {
                let snap = self.snapshot.as_ref();
                let paused = snap.is_some_and(|s| s.paused);
                let bound = snap.is_some_and(|s| s.bound);
                let pp = if paused {
                    ("p", "resume")
                } else {
                    ("p", "pause")
                };
                if bound {
                    widgets::footer_hints(&[pp, ("x", "stop"), ("h", "home")])
                } else {
                    widgets::footer_hints(&[("/", "bind"), pp, ("d", "discard"), ("h", "home")])
                }
            }
        }
    }
}

/// Timer ops that resolve to a fresh snapshot (`TimerLoaded`).
enum TimerOp {
    Pause,
    Resume,
}

fn spawn_load(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.timer().await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("timer load failed: {e}"),
                });
            }
        }
    });
}

fn spawn_start_blank(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.start_timer(None, false).await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("start failed: {e}"),
                });
            }
        }
    });
}

fn spawn_op(api: &ApiClient, tx: &UnboundedSender<Action>, op: TimerOp) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let result = match op {
            TimerOp::Pause => api.pause_timer().await,
            TimerOp::Resume => api.resume_timer().await,
        };
        match result {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("timer op failed: {e}"),
                });
            }
        }
    });
}

fn spawn_stop(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.stop_timer().await {
            Ok(stopped) => {
                let _ = tx.send(Action::TimerStopped(Box::new(stopped)));
                // Clear the header cell without disturbing the screen's
                // confirmation view (TimerCleared is app-only).
                let _ = tx.send(Action::TimerCleared);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("stop failed: {e}"),
                });
            }
        }
    });
}

fn spawn_discard(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Err(e) = api.discard_timer().await {
            let _ = tx.send(Action::Notify {
                level: Level::Error,
                text: format!("discard failed: {e}"),
            });
            return;
        }
        // Re-fetch so the screen lands on Absent and the header clears.
        match api.timer().await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(_) => {
                let _ = tx.send(Action::TimerCleared);
            }
        }
    });
}

fn spawn_bind(api: &ApiClient, tx: &UnboundedSender<Action>, target: BindTarget) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let result = match target {
            BindTarget::Existing(id) => api.bind_timer(Some(id), None).await,
            BindTarget::Create(title) => api.bind_timer(None, Some(title)).await,
        };
        match result {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("bind failed: {e}"),
                });
            }
        }
    });
}

fn spawn_candidates(api: &ApiClient, tx: &UnboundedSender<Action>, query: String) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let q = if query.trim().is_empty() {
            None
        } else {
            Some(query.trim())
        };
        if let Ok(list) = api.timer_candidates(q).await {
            let _ = tx.send(Action::TimerCandidatesLoaded(list));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use tokio::sync::mpsc;

    /// A screen plus a live api/tx. The receiver is leaked so the background
    /// tasks spawned by intent actions still have a live sender (their results,
    /// which would hit a non-existent dev server, are irrelevant to state).
    fn setup() -> (Timer, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Timer::default(), api, tx)
    }

    fn snapshot(json: serde_json::Value) -> TimerSnapshot {
        serde_json::from_value(json).unwrap()
    }

    async fn feed(
        s: &mut Timer,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    #[tokio::test]
    async fn loaded_running_unbound_is_live_unbound() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false, "elapsed_seconds": 12
            })))),
        )
        .await;
        assert!(matches!(s.stage, Stage::Live));
        assert!(!s.snapshot.as_ref().unwrap().bound);
    }

    #[tokio::test]
    async fn loaded_not_running_is_absent() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        assert!(matches!(s.stage, Stage::Absent));
    }

    #[tokio::test]
    async fn bind_flow_reaches_a_target() {
        let (mut s, api, tx) = setup();
        // Live + unbound so the bind panel is allowed to open.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        assert!(s.binding);

        // Type a query, then candidates arrive.
        feed(&mut s, &api, &tx, Action::TimerBindInput('s')).await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerCandidatesLoaded(vec![
                TimerCandidate {
                    id: 7,
                    title: "SICP reading".into(),
                },
                TimerCandidate {
                    id: 9,
                    title: "systems".into(),
                },
            ]),
        )
        .await;

        // Selection starts on the first candidate.
        assert!(matches!(s.bind_target(), Some(BindTarget::Existing(7))));
        // Move past both candidates onto the synthetic "create" row.
        feed(&mut s, &api, &tx, Action::TimerBindMove(1)).await;
        feed(&mut s, &api, &tx, Action::TimerBindMove(1)).await;
        match s.bind_target() {
            Some(BindTarget::Create(title)) => assert_eq!(title, "s"),
            _ => panic!("expected the create row to be selected"),
        }
    }

    #[tokio::test]
    async fn bind_cancel_keeps_timer_running() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        assert!(!s.binding);
        // Still Live — cancelling the panel never touches the clock.
        assert!(matches!(s.stage, Stage::Live));
    }

    #[tokio::test]
    async fn stop_shows_written_segment_and_survives_a_late_poll() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "consensus"
            })))),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerStopped(Box::new(TimerStopped {
                stopped: true,
                activity_id: 9,
                segment_id: 41,
                minutes: 25,
            })),
        )
        .await;
        match &s.stage {
            Stage::Stopped { result, label } => {
                assert_eq!(result.minutes, 25);
                assert_eq!(label.as_deref(), Some("consensus"));
            }
            _ => panic!("expected stop confirmation"),
        }

        // A background header poll must not clear the confirmation.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        assert!(matches!(s.stage, Stage::Stopped { .. }));
    }

    /// `palette_dispatch` returns `Some(warning)` for an invalid transition and
    /// `None` when it accepts the action (and spawns the API op). The receiver
    /// is leaked so the spawned tasks keep a live sender.
    fn palette(verb: TimerVerb, snap: Option<serde_json::Value>) -> Option<(Level, String)> {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        let snap = snap.map(snapshot);
        super::palette_dispatch(verb, snap.as_ref(), &api, &tx)
    }

    #[test]
    fn timer_verb_names_round_trip() {
        for name in TimerVerb::NAMES {
            assert!(TimerVerb::from_name(name).is_some(), "{name}");
        }
        assert_eq!(TimerVerb::from_name("nope"), None);
    }

    #[tokio::test]
    async fn palette_start_accepts_when_no_timer_and_warns_when_running() {
        // No live timer → start is accepted (blank clock).
        assert!(palette(TimerVerb::Start, None).is_none());
        assert!(palette(
            TimerVerb::Start,
            Some(serde_json::json!({ "running": false })),
        )
        .is_none());
        // Already running → warn instead of starting a second clock.
        let warn = palette(
            TimerVerb::Start,
            Some(serde_json::json!({ "running": true, "bound": true })),
        );
        assert!(matches!(warn, Some((Level::Warning, _))));
    }

    #[tokio::test]
    async fn palette_pause_and_resume_validate_the_paused_state() {
        // Pause: needs a running, unpaused timer.
        assert!(palette(TimerVerb::Pause, None).is_some());
        assert!(palette(
            TimerVerb::Pause,
            Some(serde_json::json!({ "running": true, "paused": true })),
        )
        .is_some());
        assert!(palette(
            TimerVerb::Pause,
            Some(serde_json::json!({ "running": true, "paused": false })),
        )
        .is_none());
        // Resume: needs a paused timer.
        assert!(palette(
            TimerVerb::Resume,
            Some(serde_json::json!({ "running": true, "paused": false })),
        )
        .is_some());
        assert!(palette(
            TimerVerb::Resume,
            Some(serde_json::json!({ "running": true, "paused": true })),
        )
        .is_none());
    }

    #[tokio::test]
    async fn palette_stop_requires_a_bound_running_timer() {
        // Nothing running.
        assert!(palette(TimerVerb::Stop, None).is_some());
        // Running but unbound → the same warning the screen shows.
        let unbound = palette(
            TimerVerb::Stop,
            Some(serde_json::json!({ "running": true, "bound": false })),
        );
        match unbound {
            Some((Level::Warning, text)) => assert!(text.contains("bind"), "{text}"),
            other => panic!("expected an unbound-stop warning, got {other:?}"),
        }
        // Running and bound → accepted.
        assert!(palette(
            TimerVerb::Stop,
            Some(serde_json::json!({ "running": true, "bound": true })),
        )
        .is_none());
    }

    #[test]
    fn live_elapsed_ticks_only_while_running() {
        let running = snapshot(serde_json::json!({
            "running": true, "paused": false, "elapsed_seconds": 100
        }));
        // No monotonic base yet → just the server value.
        assert_eq!(live_elapsed(&running, None), 100);

        let paused = snapshot(serde_json::json!({
            "running": true, "paused": true, "elapsed_seconds": 100
        }));
        let long_ago = Instant::now() - std::time::Duration::from_secs(30);
        // Paused clock stays frozen despite an old base.
        assert_eq!(live_elapsed(&paused, Some(long_ago)), 100);
    }
}
