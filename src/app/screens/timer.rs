//! Timer screen — the watch face (timer.dc.html §Timer hero / §Paused): one
//! big number, a state label, and a foldable instrument rail. The persistent
//! header cell is rendered by the chrome from the app-owned snapshot; this
//! screen owns the interactions.
//!
//! States, driven by the live snapshot:
//! - **Absent** — no live timer. `s` starts a blank clock ("name it later").
//! - **Live** — the watch face. `SPC` (or `p`) pauses/resumes; `i` folds the
//!   rail; bound: `s` ends & saves; unbound: `/`/`b` open the bind search,
//!   `d` discards. Paused draws the frozen amber face — a paused timer never
//!   goes idle.
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
    /// `i` folds the instrument rail away; the number recenters into the calm
    /// watch face. Default is the cockpit (rail shown).
    rail_hidden: bool,
    /// Today's logged minutes (summed from today's activities) for the rail.
    today_minutes: Option<u32>,
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
        spawn_today(api, tx);
    }

    /// While the bind panel is open, the screen owns every key (a live search):
    /// characters filter, arrows pick, Enter binds/creates, Esc closes. This
    /// runs before the global keymap, so the timer keeps running untouched.
    ///
    /// On the live face the screen also claims `Space` as pause ⇄ resume (the
    /// design's `SPC`), so the leader is unavailable only while a clock runs on
    /// this screen — navigation still has `h`, `t`, and the `:` verbs.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if !self.binding {
            if key.code == KeyCode::Char(' ') && matches!(self.stage, Stage::Live) {
                return Some(Action::TimerPauseResume);
            }
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
            // `s` — stage-dependent primary: start when absent, end & save when
            // bound, and the bind-first warning when unbound (the full
            // bind-at-stop flow is its own ticket).
            Action::TimerSave => match self.stage {
                Stage::Absent => spawn_start_blank(api, tx),
                Stage::Live => {
                    if self.snapshot.as_ref().is_some_and(|s| s.bound) {
                        spawn_stop(api, tx);
                    } else {
                        return Some((
                            Level::Warning,
                            "bind the timer before saving (`/` bind · `d` discard)".into(),
                        ));
                    }
                }
                _ => {}
            },
            Action::TimerToggleRail => self.rail_hidden = !self.rail_hidden,
            Action::TimerModeSwitch => {
                return Some((
                    Level::Info,
                    "mode switch (stopwatch ⇄ focus) needs the focus API — requested upstream"
                        .into(),
                ));
            }
            Action::TimerTodayLoaded(minutes) => self.today_minutes = Some(minutes),
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

    /// The watch face (§Timer hero / §Paused): state label, the big number,
    /// context, activity — vertically centered, with the instrument rail on
    /// the right unless folded away with `i`.
    fn render_watch_face(&self, frame: &mut Frame, area: Rect) {
        let block = bordered("Timer");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Rail only when there's room for both the face and the instruments.
        let (face_area, rail_area) = if !self.rail_hidden && inner.width >= 64 {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(0), Constraint::Length(26)])
                .split(inner);
            (cols[0], Some(cols[1]))
        } else {
            (inner, None)
        };

        let Some(snap) = self.snapshot.as_ref() else {
            return;
        };
        let paused = snap.paused;
        let secs = live_elapsed(snap, self.base);

        let mut lines: Vec<Line<'static>> = Vec::new();
        // State label above the number.
        lines.push(if paused {
            Line::from(Span::styled(
                "‖  PAUSED — NOT COUNTING",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(
                "●  TRACKING",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
        });
        lines.push(Line::from(""));

        // The big number — muted while frozen, accent while counting.
        let digit_style = if paused {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        };
        lines.extend(big_time_lines(&widgets::fmt_elapsed(secs), digit_style));
        lines.push(Line::from(""));

        // Context under the number.
        if paused {
            lines.push(Line::from(Span::styled(
                "frozen · the paused gap is excluded from the total",
                theme::muted(),
            )));
            lines.push(Line::from(Span::styled(
                "a paused timer never goes idle",
                theme::muted(),
            )));
        } else if let Some(since) = snap.started_at.map(|ts| {
            let local = ts.to_zoned(jiff::tz::TimeZone::system());
            format!("since {}", local.strftime("%H:%M"))
        }) {
            lines.push(Line::from(Span::styled(since, theme::muted())));
        }
        lines.push(Line::from(""));

        // The activity line.
        if snap.bound {
            let label = snap.label.clone().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                label,
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "untitled",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )));
            lines.push(Line::from(Span::styled(
                "bind when you stop — / bind now · d discard",
                theme::muted(),
            )));
        }

        // Vertical centering: pad above with empty rows.
        let content_h = lines.len() as u16;
        let pad = face_area.height.saturating_sub(content_h) / 2;
        let mut padded: Vec<Line<'static>> = (0..pad).map(|_| Line::from("")).collect();
        padded.extend(lines);
        frame.render_widget(
            Paragraph::new(padded).alignment(ratatui::layout::Alignment::Center),
            face_area,
        );

        if let Some(rail) = rail_area {
            self.render_rail(frame, rail);
        }
    }

    /// The instrument rail. Focus instruments (interval gauge, pomodoro ticks)
    /// arrive with the focus display ticket; today's total is the one
    /// instrument the stopwatch face has data for.
    fn render_rail(&self, frame: &mut Frame, area: Rect) {
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::LEFT)
            .border_style(Style::default().fg(theme::BORDER));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines = vec![Line::from(Span::styled("TODAY", theme::muted()))];
        match self.today_minutes {
            Some(minutes) => {
                lines.push(Line::from(Span::styled(
                    fmt_minutes(minutes),
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled("logged today", theme::muted())));
            }
            None => lines.push(Line::from(Span::styled("…", theme::muted()))),
        }

        let pad = inner.height.saturating_sub(lines.len() as u16) / 2;
        let mut padded: Vec<Line<'static>> = (0..pad).map(|_| Line::from("")).collect();
        padded.extend(lines);
        frame.render_widget(
            Paragraph::new(padded).alignment(ratatui::layout::Alignment::Center),
            inner,
        );
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
            Stage::Live => self.render_watch_face(frame, area),
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
                    ("SPC", "resume")
                } else {
                    ("SPC", "pause")
                };
                if bound {
                    widgets::footer_hints(&[
                        pp,
                        ("i", "instruments"),
                        ("s", "end & save"),
                        ("h", "home"),
                    ])
                } else {
                    widgets::footer_hints(&[
                        ("/", "bind"),
                        pp,
                        ("i", "instruments"),
                        ("d", "discard"),
                        ("h", "home"),
                    ])
                }
            }
        }
    }
}

/// The watch-face digit font: 5 rows tall, `█`-on-space, one column of gap
/// between glyphs (the kit's block-digit idiom — weight and colour only, one
/// font size).
fn big_glyph(c: char) -> [&'static str; 5] {
    match c {
        '0' => ["█████", "█   █", "█   █", "█   █", "█████"],
        '1' => ["    █", "    █", "    █", "    █", "    █"],
        '2' => ["█████", "    █", "█████", "█    ", "█████"],
        '3' => ["█████", "    █", "█████", "    █", "█████"],
        '4' => ["█   █", "█   █", "█████", "    █", "    █"],
        '5' => ["█████", "█    ", "█████", "    █", "█████"],
        '6' => ["█████", "█    ", "█████", "█   █", "█████"],
        '7' => ["█████", "    █", "    █", "    █", "    █"],
        '8' => ["█████", "█   █", "█████", "█   █", "█████"],
        '9' => ["█████", "█   █", "█████", "    █", "█████"],
        ':' => [" ", "█", " ", "█", " "],
        _ => [" ", " ", " ", " ", " "],
    }
}

fn big_time_lines(time: &str, style: Style) -> Vec<Line<'static>> {
    (0..5)
        .map(|row| {
            let text = time
                .chars()
                .map(|c| big_glyph(c)[row])
                .collect::<Vec<_>>()
                .join(" ");
            Line::from(Span::styled(text, style))
        })
        .collect()
}

fn fmt_minutes(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// Timer ops that resolve to a fresh snapshot (`TimerLoaded`).
enum TimerOp {
    Pause,
    Resume,
}

/// Today's logged minutes for the rail — the same today-window read the Home
/// screen uses, reduced to one number.
fn spawn_today(api: &ApiClient, tx: &UnboundedSender<Action>) {
    use jiff::{civil::Date, ToSpan, Zoned};

    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let now = Zoned::now();
        let date: Date = now.date();
        let start = date
            .to_zoned(now.time_zone().clone())
            .ok()
            .map(|z| z.timestamp());
        let end = start.and_then(|s| s.checked_add(1.day()).ok());
        let filters = crate::api::ActivityFilters {
            started_after: start,
            started_before: end,
            ..Default::default()
        };
        if let Ok(list) = api.list_activities(&filters).await {
            let total: u32 = list.data.iter().filter_map(|a| a.duration_minutes).sum();
            let _ = tx.send(Action::TimerTodayLoaded(total));
        }
    });
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

    #[tokio::test]
    async fn save_key_is_stage_dependent() {
        let (mut s, api, tx) = setup();
        // Live + unbound → the bind-first warning, no stop.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false
            })))),
        )
        .await;
        let warn = s.handle(Action::TimerSave, &api, &tx).await;
        match warn {
            Some((Level::Warning, text)) => assert!(text.contains("bind"), "{text}"),
            other => panic!("expected the bind-first warning, got {other:?}"),
        }

        // Live + bound → accepted (spawns the stop op, no warning).
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true
            })))),
        )
        .await;
        assert!(s.handle(Action::TimerSave, &api, &tx).await.is_none());
    }

    #[tokio::test]
    async fn rail_folds_and_unfolds_with_i() {
        let (mut s, api, tx) = setup();
        assert!(!s.rail_hidden, "cockpit by default");
        feed(&mut s, &api, &tx, Action::TimerToggleRail).await;
        assert!(s.rail_hidden);
        feed(&mut s, &api, &tx, Action::TimerToggleRail).await;
        assert!(!s.rail_hidden);
    }

    #[tokio::test]
    async fn mode_switch_names_the_missing_api() {
        let (mut s, api, tx) = setup();
        let note = s.handle(Action::TimerModeSwitch, &api, &tx).await;
        match note {
            Some((Level::Info, text)) => assert!(text.contains("focus API"), "{text}"),
            other => panic!("expected the focus-API note, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn today_total_lands_in_the_rail() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, Action::TimerTodayLoaded(227)).await;
        assert_eq!(s.today_minutes, Some(227));
    }

    #[tokio::test]
    async fn space_pauses_only_on_the_live_face() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut s, api, tx) = setup();
        // Absent: Space stays with the global leader.
        assert!(s
            .intercept_key(KeyEvent::from(KeyCode::Char(' ')))
            .is_none());

        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true
            })))),
        )
        .await;
        assert!(matches!(
            s.intercept_key(KeyEvent::from(KeyCode::Char(' '))),
            Some(Action::TimerPauseResume)
        ));
    }

    #[tokio::test]
    async fn paused_face_is_frozen_and_labelled() {
        use ratatui::{backend::TestBackend, Terminal};

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "paused": true,
                "label": "Read DDIA ch.7", "elapsed_seconds": 3134
            })))),
        )
        .await;

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| s.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("PAUSED — NOT COUNTING"), "paused label");
        assert!(content.contains("never goes idle"), "idle-guard caption");
        assert!(content.contains('█'), "big digits render");
        assert!(content.contains("TODAY"), "rail shows the today instrument");
    }

    #[test]
    fn big_time_lines_render_every_clock_char() {
        let lines = big_time_lines("10:59", Style::default());
        assert_eq!(lines.len(), 5);
        // Every row spans the same five glyphs joined by single gaps.
        let row0 = &lines[0].spans[0].content;
        assert!(row0.contains("█████"), "{row0}");
    }

    #[test]
    fn fmt_minutes_reads_like_the_design() {
        assert_eq!(fmt_minutes(227), "3h 47m");
        assert_eq!(fmt_minutes(45), "45m");
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
