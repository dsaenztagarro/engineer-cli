//! Timer screen — the watch face and every timer interaction; the header cell is the chrome's.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{
    ApiClient, ApiError, ReclaimVerb, Reclaimed, Timer as TimerSnapshot, TimerCandidate,
    TimerStopped,
};
use crate::app::action::Action;
use crate::messages;
use crate::queue::{
    Intent, IntentKind, IntentState, QueueStore, Resolution, Resolved, WriteOutcome,
};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

/// The `:timer` sub-verbs, defined beside the actions they drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerVerb {
    Start,
    Pause,
    Resume,
    Stop,
}

impl TimerVerb {
    /// Help/completion order; the `:` grammar table reads its argument set here.
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

/// Runs against the app-owned snapshot rather than the screen, so it works from
/// any screen and never races the screen's own load.
pub(crate) fn palette_dispatch(
    verb: TimerVerb,
    snap: Option<&TimerSnapshot>,
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
) -> Option<(Level, String)> {
    let running = snap.is_some_and(|s| s.running);
    let paused = snap.is_some_and(|s| s.paused);
    let bound = snap.is_some_and(|s| s.bound);
    match verb {
        TimerVerb::Start => {
            if running {
                Some((Level::Warning, "a timer is already running".into()))
            } else {
                spawn_start_blank(api, tx, paths, false);
                None
            }
        }
        TimerVerb::Pause => {
            if !running {
                Some((Level::Warning, "no timer running".into()))
            } else if paused {
                Some((Level::Warning, "timer is already paused".into()))
            } else {
                spawn_op(api, tx, paths, TimerOp::Pause);
                None
            }
        }
        TimerVerb::Resume => {
            if !running {
                Some((Level::Warning, "no timer running".into()))
            } else if !paused {
                Some((Level::Warning, "timer isn't paused".into()))
            } else {
                spawn_op(api, tx, paths, TimerOp::Resume);
                None
            }
        }
        TimerVerb::Stop => {
            if !running {
                Some((Level::Warning, "no timer to stop".into()))
            } else if bound {
                spawn_stop(api, tx, paths);
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

/// Shared with the header cell so both tick in step.
pub(crate) fn live_elapsed(snap: &TimerSnapshot, base: Option<Instant>) -> i64 {
    let age = base.map(|b| b.elapsed().as_secs() as i64).unwrap_or(0);
    crate::timer_clock::elapsed_with_snapshot_age(snap, jiff::Timestamp::now(), age)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    Break { long: bool },
    BackToWork,
}

/// Phase time ignores mid-phase pauses, so an offer can arrive early after one;
/// the server still validates the transition.
pub(crate) fn offer_for(
    snap: &TimerSnapshot,
    settings: &crate::api::TimerSettings,
    now: jiff::Timestamp,
) -> Option<Offer> {
    if !snap.running
        || snap.paused
        || snap.idle == Some(true)
        || snap.mode.as_deref() != Some("focus")
    {
        return None;
    }
    let started = snap.phase_started_at?;
    let phase_secs = (now.as_second() - started.as_second()).max(0);
    let every = settings.focus_long_break_every;
    match snap.phase.as_deref() {
        Some("work") => {
            let target = settings.focus_work_minutes as i64 * 60;
            (phase_secs >= target).then(|| {
                let next_break = snap.intervals_completed.unwrap_or(0) + 1;
                Offer::Break {
                    long: every != 0 && next_break.is_multiple_of(every),
                }
            })
        }
        Some("break") => {
            let banked = snap.intervals_completed.unwrap_or(0);
            let long = every != 0 && banked != 0 && banked.is_multiple_of(every);
            let minutes = if long {
                settings.focus_long_break_minutes
            } else {
                settings.focus_short_break_minutes
            };
            (phase_secs >= minutes as i64 * 60).then_some(Offer::BackToWork)
        }
        _ => None,
    }
}

pub(crate) const DISCARD_CONFIRM_SECS: i64 = 120;

enum BindTarget {
    Existing(i64),
    Create(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Stopwatch,
    Focus,
}

/// The open panel, which owns the keys.
enum Panel {
    Bind {
        save_on_bind: bool,
        froze: bool,
    },
    Start {
        mode: PickerMode,
        confirm: Option<TimerCandidate>,
    },
    Reclaim {
        selected: usize,
    },
    Reconcile {
        /// Boxed: the stored problem payload is large next to the other panels.
        intent: Box<Intent>,
        /// 0 = local, 1 = server.
        selected: usize,
        confirm_drop: bool,
    },
}

fn rejected_write(intent: &Intent) -> bool {
    matches!(
        intent.kind,
        IntentKind::SegmentCreate { .. } | IntentKind::ActivityCreate { .. }
    )
}

/// The reclaim list rows, in display order: trim · keep · stop · discard.
const RECLAIM_ROWS: usize = 4;

enum StartTarget {
    Candidate(TimerCandidate),
    Create(String),
    JustStart,
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
    /// Monotonic baseline for ticking the displayed elapsed between polls.
    base: Option<Instant>,
    rail_hidden: bool,
    today_minutes: Option<u32>,
    week: Vec<crate::api::DayMinutes>,
    panel: Option<Panel>,
    discard_armed: bool,
    provisional: bool,
    /// `None` is the shared XDG queue and cache; tests inject a scratch dir.
    queue_paths: QueuePaths,
    settings: Option<crate::api::TimerSettings>,
    query: String,
    candidates: Vec<TimerCandidate>,
    cand_state: ListState,
}

impl Timer {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.stage = Stage::Loading;
        spawn_load(api, tx);
        spawn_today(api, tx);
        spawn_week(api, tx);
        spawn_settings(api, tx);
        spawn_diverged_check(tx, self.queue_paths.clone());
    }

    fn focus_phase(&self) -> Option<&str> {
        let snap = self.snapshot.as_ref()?;
        (snap.running && snap.mode.as_deref() == Some("focus"))
            .then(|| snap.phase.as_deref().unwrap_or("work"))
    }

    fn current_offer(&self) -> Option<Offer> {
        let snap = self.snapshot.as_ref()?;
        let settings = self.settings.as_ref()?;
        offer_for(snap, settings, jiff::Timestamp::now())
    }

    fn default_reclaim_row(&self) -> usize {
        match self
            .settings
            .as_ref()
            .map(|s| s.idle_default_reclaim.as_str())
        {
            Some("keep") => 1,
            Some("stop") => 2,
            _ => 0,
        }
    }

    /// On the live face `Space` is pause ⇄ resume, which shadows the `<Space>`
    /// leader while a clock runs here — navigation still has `h`, `t` and `:`.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(panel) = self.panel.as_ref() else {
            if key.code == KeyCode::Char(' ') && matches!(self.stage, Stage::Live) {
                return Some(Action::TimerPauseResume);
            }
            return None;
        };
        if matches!(panel, Panel::Reclaim { .. }) {
            return match key.code {
                KeyCode::Esc => Some(Action::TimerBindCancel),
                KeyCode::Enter => Some(Action::TimerBindSubmit),
                KeyCode::Char('j') | KeyCode::Down => Some(Action::TimerBindMove(1)),
                KeyCode::Char('k') | KeyCode::Up => Some(Action::TimerBindMove(-1)),
                _ => None,
            };
        }
        if let Panel::Reconcile { intent, .. } = panel {
            if rejected_write(intent) {
                return match key.code {
                    KeyCode::Esc => Some(Action::TimerBindCancel),
                    KeyCode::Char('e') => Some(Action::TimerReconcileEdit),
                    KeyCode::Char('x') => Some(Action::TimerReconcileDrop),
                    KeyCode::Char('s') => Some(Action::TimerReconcileSkip),
                    _ => None,
                };
            }
            return match key.code {
                KeyCode::Esc => Some(Action::TimerBindCancel),
                KeyCode::Enter => Some(Action::TimerBindSubmit),
                KeyCode::Char('j') | KeyCode::Down => Some(Action::TimerBindMove(1)),
                KeyCode::Char('k') | KeyCode::Up => Some(Action::TimerBindMove(-1)),
                KeyCode::Char('b') => Some(Action::TimerReconcileBoth),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Esc => Some(Action::TimerBindCancel),
            KeyCode::Enter => Some(Action::TimerBindSubmit),
            KeyCode::Tab if matches!(panel, Panel::Start { .. }) => {
                Some(Action::TimerPickerToggleMode)
            }
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
        if self.discard_armed && !matches!(action, Action::TimerDiscard) {
            self.discard_armed = false;
        }
        if !matches!(action, Action::TimerReconcileDrop) {
            if let Some(Panel::Reconcile { confirm_drop, .. }) = self.panel.as_mut() {
                *confirm_drop = false;
            }
        }
        match action {
            Action::TimerLoaded(t) => {
                // The reconcile panel follows the queue file this way rather
                // than running its own polling loop.
                spawn_diverged_check(tx, self.queue_paths.clone());
                if matches!(self.stage, Stage::Stopped { .. }) {
                    return None;
                }
                let t = *t;
                self.stage = if t.running {
                    Stage::Live
                } else {
                    Stage::Absent
                };
                if matches!(self.panel, Some(Panel::Bind { .. })) && (t.bound || !t.running) {
                    self.close_panel();
                }
                match (t.idle == Some(true) && t.running, &self.panel) {
                    (true, None) => {
                        self.panel = Some(Panel::Reclaim {
                            selected: self.default_reclaim_row(),
                        });
                    }
                    (false, Some(Panel::Reclaim { .. })) => self.close_panel(),
                    _ => {}
                }
                self.base = Some(Instant::now());
                self.snapshot = Some(t);
                self.provisional = false;
            }
            Action::TimerProvisional(t) => {
                if matches!(self.stage, Stage::Stopped { .. }) {
                    return None;
                }
                let t = *t;
                self.stage = if t.running {
                    Stage::Live
                } else {
                    Stage::Absent
                };
                if matches!(self.panel, Some(Panel::Bind { .. })) && (t.bound || !t.running) {
                    self.close_panel();
                }
                self.base = Some(Instant::now());
                self.snapshot = Some(t);
                self.provisional = true;
            }
            Action::TimerDivergedLoaded(found) => match (found, &mut self.panel) {
                (Some(intent), None) => {
                    self.panel = Some(Panel::Reconcile {
                        intent,
                        selected: 0,
                        confirm_drop: false,
                    });
                }
                (Some(intent), Some(Panel::Reconcile { intent: cur, .. })) => *cur = intent,
                (None, Some(Panel::Reconcile { .. })) => self.close_panel(),
                _ => {}
            },
            Action::TimerReconcileBoth => {
                if let Some(Panel::Reconcile { intent, .. }) = self.panel.as_ref() {
                    if matches!(intent.kind, IntentKind::TimerStop { .. }) {
                        return Some((
                            Level::Warning,
                            "a diverged stop has one segment at stake — ⏎ on a side instead".into(),
                        ));
                    }
                    let id = intent.id;
                    self.close_panel();
                    spawn_resolve(api, tx, self.queue_paths.clone(), id, Resolution::KeepBoth);
                }
            }
            Action::TimerReconcileEdit => {
                if let Some(Panel::Reconcile { intent, .. }) = self.panel.as_ref() {
                    match crate::queue::edit_seed(intent) {
                        Some(seed) => {
                            let _ = tx.send(Action::QueueIntentEdit {
                                intent_id: intent.id,
                                seed,
                            });
                        }
                        None => {
                            return Some((
                                Level::Warning,
                                "this divergence has nothing editable — pick a side instead".into(),
                            ));
                        }
                    }
                }
            }
            Action::TimerReconcileEditApply { intent_id, buffer } => {
                self.close_panel();
                spawn_edit_apply(api, tx, self.queue_paths.clone(), intent_id, buffer);
            }
            Action::TimerReconcileDrop => {
                if let Some(Panel::Reconcile {
                    intent,
                    confirm_drop,
                    ..
                }) = self.panel.as_mut()
                {
                    if !rejected_write(intent) {
                        return None;
                    }
                    if !*confirm_drop {
                        *confirm_drop = true;
                        return Some((
                            Level::Warning,
                            "drop this queued write? `x` again to confirm — it will never be written".into(),
                        ));
                    }
                    let id = intent.id;
                    self.close_panel();
                    spawn_reject_gesture(
                        api,
                        tx,
                        self.queue_paths.clone(),
                        id,
                        RejectGesture::Drop,
                    );
                }
            }
            Action::TimerReconcileSkip => {
                if let Some(Panel::Reconcile { intent, .. }) = self.panel.as_ref() {
                    if !rejected_write(intent) {
                        return None;
                    }
                    let id = intent.id;
                    self.close_panel();
                    spawn_reject_gesture(
                        api,
                        tx,
                        self.queue_paths.clone(),
                        id,
                        RejectGesture::Skip,
                    );
                }
            }
            Action::TimerReload => spawn_load(api, tx),
            Action::TimerSave => match self.stage {
                Stage::Absent => self.open_start_panel(api, tx),
                Stage::Live => {
                    if self.snapshot.as_ref().is_some_and(|s| s.bound) {
                        spawn_stop(api, tx, self.queue_paths.clone());
                    } else {
                        self.open_bind_at_stop(api, tx);
                    }
                }
                _ => {}
            },
            Action::TimerToggleRail => self.rail_hidden = !self.rail_hidden,
            Action::TimerModeSwitch => match self.snapshot.as_ref() {
                Some(snap) if snap.running => {
                    let target = if snap.mode.as_deref() == Some("focus") {
                        "stopwatch"
                    } else {
                        "focus"
                    };
                    spawn_mode(api, tx, target);
                }
                _ => {
                    return Some((
                        Level::Warning,
                        "no running timer to switch — mode is picked at start".into(),
                    ));
                }
            },
            Action::TimerSkipInterval => match self.focus_phase() {
                Some("work") => spawn_skip_interval(api, tx),
                Some("break") => spawn_phase(api, tx, "work"),
                _ => {}
            },
            Action::TimerBreak => match self.focus_phase() {
                Some("work") => spawn_phase(api, tx, "break"),
                Some("break") => spawn_phase(api, tx, "work"),
                _ => {
                    return Box::pin(self.handle(Action::TimerBindBegin, api, tx)).await;
                }
            },
            Action::TimerTodayLoaded(minutes) => self.today_minutes = Some(minutes),
            Action::TimerWeekLoaded(days) => self.week = days,
            Action::SettingsLoaded(s) => self.settings = Some(*s),
            Action::TimerPauseResume => {
                if matches!(self.stage, Stage::Live) {
                    if self.snapshot.as_ref().is_some_and(|s| s.paused) {
                        spawn_op(api, tx, self.queue_paths.clone(), TimerOp::Resume);
                    } else {
                        spawn_op(api, tx, self.queue_paths.clone(), TimerOp::Pause);
                    }
                }
            }
            Action::TimerStop => {
                if !matches!(self.stage, Stage::Live) {
                    return None;
                }
                if self.snapshot.as_ref().is_some_and(|s| s.bound) {
                    spawn_stop(api, tx, self.queue_paths.clone());
                } else {
                    self.open_bind_at_stop(api, tx);
                }
            }
            Action::TimerUndo => {
                if let Stage::Stopped { result, .. } = &self.stage {
                    if result.segment_id >= 0 {
                        spawn_undo(api, tx, result.activity_id, result.segment_id);
                    }
                }
            }
            Action::TimerUndone => {
                self.stage = Stage::Loading;
                spawn_load(api, tx);
            }
            Action::TimerStopped(result) => {
                let label = self.snapshot.as_ref().and_then(|s| s.label.clone());
                self.close_panel();
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
                if !matches!(self.stage, Stage::Live) {
                    return None;
                }
                let elapsed = self
                    .snapshot
                    .as_ref()
                    .map(|s| live_elapsed(s, self.base))
                    .unwrap_or(0);
                if elapsed > DISCARD_CONFIRM_SECS && !self.discard_armed {
                    self.discard_armed = true;
                    return Some((
                        Level::Warning,
                        format!(
                            "discard {} of work? `d` again to confirm",
                            widgets::fmt_elapsed(elapsed)
                        ),
                    ));
                }
                self.discard_armed = false;
                spawn_discard(api, tx, self.queue_paths.clone());
            }
            Action::TimerBindBegin => match self.stage {
                Stage::Live => {
                    if self.snapshot.as_ref().is_some_and(|s| !s.bound) {
                        self.open_panel(
                            Panel::Bind {
                                save_on_bind: false,
                                froze: false,
                            },
                            api,
                            tx,
                        );
                    } else {
                        self.open_start_panel(api, tx);
                    }
                }
                Stage::Absent => self.open_start_panel(api, tx),
                _ => {}
            },
            Action::TimerBindCancel => match self.panel.as_mut() {
                Some(Panel::Start { confirm, .. }) if confirm.is_some() => {
                    *confirm = None;
                    return None;
                }
                Some(Panel::Bind {
                    save_on_bind: true,
                    froze,
                }) => {
                    if *froze {
                        spawn_op(api, tx, self.queue_paths.clone(), TimerOp::Resume);
                    }
                    self.close_panel();
                }
                _ => self.close_panel(),
            },
            Action::TimerPickerToggleMode => {
                if let Some(Panel::Start { mode, .. }) = self.panel.as_mut() {
                    *mode = match mode {
                        PickerMode::Stopwatch => PickerMode::Focus,
                        PickerMode::Focus => PickerMode::Stopwatch,
                    };
                }
            }
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
            Action::TimerBindMove(delta) => match self.panel.as_mut() {
                Some(Panel::Reclaim { selected }) => {
                    let next = (*selected as i32 + delta).clamp(0, RECLAIM_ROWS as i32 - 1);
                    *selected = next as usize;
                }
                Some(Panel::Reconcile { selected, .. }) => {
                    *selected = (*selected as i32 + delta).clamp(0, 1) as usize;
                }
                _ => self.move_selection(delta),
            },
            Action::TimerCandidatesLoaded(list) => {
                self.candidates = list;
                let len = self.bind_rows_len();
                if len == 0 {
                    self.cand_state.select(None);
                } else if self.cand_state.selected().unwrap_or(0) >= len {
                    self.cand_state.select(Some(len - 1));
                }
            }
            Action::TimerBindSubmit => return self.submit_panel(api, tx),
            _ => {}
        }
        None
    }

    fn submit_panel(
        &mut self,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match self.panel.as_mut() {
            Some(Panel::Bind { save_on_bind, .. }) => {
                let save = *save_on_bind;
                if let Some(target) = self.bind_target() {
                    self.close_panel();
                    if save {
                        spawn_bind_then_stop(api, tx, self.queue_paths.clone(), target);
                    } else {
                        spawn_bind(api, tx, self.queue_paths.clone(), target);
                    }
                }
                None
            }
            Some(Panel::Start { mode, confirm }) => {
                let focus = *mode == PickerMode::Focus;
                if let Some(picked) = confirm.take() {
                    self.close_panel();
                    spawn_start_switch(api, tx, self.queue_paths.clone(), picked.id, focus);
                    return None;
                }
                let running = self.snapshot.as_ref().is_some_and(|s| s.running);
                match self.start_target() {
                    Some(StartTarget::Candidate(picked)) if running => {
                        if let Some(Panel::Start { confirm, .. }) = self.panel.as_mut() {
                            *confirm = Some(picked);
                        }
                    }
                    Some(StartTarget::Candidate(picked)) => {
                        self.close_panel();
                        spawn_start_bound(api, tx, self.queue_paths.clone(), picked.id, focus);
                    }
                    Some(StartTarget::Create(title)) => {
                        self.close_panel();
                        spawn_create_and_start(api, tx, self.queue_paths.clone(), title, focus);
                    }
                    Some(StartTarget::JustStart) => {
                        self.close_panel();
                        spawn_start_blank(api, tx, self.queue_paths.clone(), focus);
                    }
                    None => {}
                }
                None
            }
            Some(Panel::Reclaim { selected }) => {
                let selected = *selected;
                self.close_panel();
                match selected {
                    0 => spawn_reclaim(api, tx, ReclaimVerb::Trim),
                    1 => spawn_reclaim(api, tx, ReclaimVerb::Keep),
                    2 => spawn_reclaim(api, tx, ReclaimVerb::Stop),
                    _ => {
                        let _ = tx.send(Action::TimerDiscard);
                    }
                }
                None
            }
            Some(Panel::Reconcile {
                intent, selected, ..
            }) => {
                if rejected_write(intent) {
                    return None;
                }
                let resolution = if *selected == 0 {
                    Resolution::KeepLocal
                } else {
                    Resolution::TakeServer
                };
                let id = intent.id;
                self.close_panel();
                spawn_resolve(api, tx, self.queue_paths.clone(), id, resolution);
                None
            }
            None => None,
        }
    }

    fn open_panel(&mut self, panel: Panel, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.panel = Some(panel);
        self.query.clear();
        self.candidates.clear();
        self.cand_state.select(Some(0));
        spawn_candidates(api, tx, String::new());
    }

    fn open_start_panel(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.open_panel(
            Panel::Start {
                mode: PickerMode::Stopwatch,
                confirm: None,
            },
            api,
            tx,
        );
    }

    fn open_bind_at_stop(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let already_paused = self.snapshot.as_ref().is_some_and(|s| s.paused);
        if !already_paused {
            spawn_op(api, tx, self.queue_paths.clone(), TimerOp::Pause);
        }
        self.open_panel(
            Panel::Bind {
                save_on_bind: true,
                froze: !already_paused,
            },
            api,
            tx,
        );
    }

    fn close_panel(&mut self) {
        self.panel = None;
        self.query.clear();
        self.candidates.clear();
    }

    /// The synthetic `(create, just start)` rows after the candidates.
    fn extra_rows(&self) -> (bool, bool) {
        let has_query = !self.query.trim().is_empty();
        let running = self.snapshot.as_ref().is_some_and(|s| s.running);
        match self.panel {
            Some(Panel::Bind { .. }) => (has_query, false),
            Some(Panel::Start { .. }) if !running => (has_query, true),
            Some(Panel::Start { .. }) | Some(Panel::Reclaim { .. }) => (false, false),
            Some(Panel::Reconcile { .. }) | None => (false, false),
        }
    }

    fn bind_rows_len(&self) -> usize {
        let (create, just_start) = self.extra_rows();
        self.candidates.len() + usize::from(create) + usize::from(just_start)
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

    fn start_target(&self) -> Option<StartTarget> {
        let sel = self.cand_state.selected()?;
        let (create, just_start) = self.extra_rows();
        if sel < self.candidates.len() {
            return Some(StartTarget::Candidate(self.candidates[sel].clone()));
        }
        let mut next = self.candidates.len();
        if create {
            if sel == next {
                return Some(StartTarget::Create(self.query.trim().to_string()));
            }
            next += 1;
        }
        (just_start && sel == next).then_some(StartTarget::JustStart)
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

    fn render_watch_face(&self, frame: &mut Frame, area: Rect) {
        let block = bordered("Timer");
        let inner = block.inner(area);
        frame.render_widget(block, area);

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
        let focus = snap.mode.as_deref() == Some("focus");
        let on_break = focus && snap.phase.as_deref() == Some("break");
        // No "of N": the round length has no API to read it from.
        let interval_now = snap.intervals_completed.unwrap_or(0) + 1;

        let idle = snap.idle == Some(true);
        let offer = self.current_offer();

        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.provisional {
            lines.push(Line::from(Span::styled(
                "◔  QUEUED — will sync when you reconnect",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(if idle {
            Line::from(Span::styled(
                "◐  IDLE — RECLAIM PENDING",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if paused {
            Line::from(Span::styled(
                "‖  PAUSED — NOT COUNTING",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if offer == Some(Offer::BackToWork) {
            Line::from(Span::styled(
                "○  BREAK'S OVER",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if matches!(offer, Some(Offer::Break { .. })) {
            Line::from(Span::styled(
                format!(
                    "◆  INTERVAL {} COMPLETE",
                    snap.intervals_completed.unwrap_or(0) + 1
                ),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if on_break {
            Line::from(Span::styled(
                "○  BREAK — NOT COUNTING",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if snap.over {
            Line::from(Span::styled(
                "●  PAST THE PLAN — STILL COUNTING",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ))
        } else if focus {
            Line::from(Span::styled(
                format!("◆  WORK · INTERVAL {interval_now}"),
                Style::default()
                    .fg(theme::ACCENT)
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

        let digit_style = if paused || on_break {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        } else if snap.over {
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        };
        lines.extend(big_time_lines(&widgets::fmt_elapsed(secs), digit_style));
        lines.push(Line::from(""));

        if paused {
            lines.push(Line::from(Span::styled(
                "frozen · the paused gap is excluded from the total",
                theme::muted(),
            )));
            lines.push(Line::from(Span::styled(
                "a paused timer never goes idle",
                theme::muted(),
            )));
        } else if let Some(Offer::Break { long }) = offer {
            let (mins, kind) = self
                .settings
                .as_ref()
                .map(|s| {
                    if long {
                        (s.focus_long_break_minutes, "long break")
                    } else {
                        (s.focus_short_break_minutes, "break")
                    }
                })
                .unwrap_or((0, "break"));
            lines.push(Line::from(Span::styled(
                format!("b start {mins}m {kind} · n skip — arm interval {interval_now}"),
                theme::muted(),
            )));
            lines.push(Line::from(Span::styled(
                "nothing fires on its own — the clock waits for you",
                theme::muted(),
            )));
        } else if offer == Some(Offer::BackToWork) {
            lines.push(Line::from(Span::styled(
                format!("b back to work — interval {interval_now} arms only when you say so"),
                theme::muted(),
            )));
        } else if on_break {
            lines.push(Line::from(Span::styled(
                "a break is never a segment — a rhythm, not logged data",
                theme::muted(),
            )));
        } else if snap.over {
            let planned = snap.planned_minutes.unwrap_or(0);
            let logged = snap.logged_minutes.unwrap_or(0);
            lines.push(Line::from(Span::styled(
                format!(
                    "planned {} · logged {} — earlier segments + this timer",
                    fmt_minutes(planned),
                    fmt_minutes(logged)
                ),
                theme::muted(),
            )));
            lines.push(Line::from(Span::styled(
                "s wrap up & save · SPC pause — it never stops anything for you",
                theme::muted(),
            )));
        } else if let Some(since) = snap.started_at.map(|ts| {
            let local = ts.to_zoned(jiff::tz::TimeZone::system());
            format!("since {}", local.strftime("%H:%M"))
        }) {
            lines.push(Line::from(Span::styled(since, theme::muted())));
        }
        lines.push(Line::from(""));

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

    fn render_rail(&self, frame: &mut Frame, area: Rect) {
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::LEFT)
            .border_style(Style::default().fg(theme::BORDER));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // No empty remainder dots: the round length has no API to read it from.
        if let Some(snap) = self
            .snapshot
            .as_ref()
            .filter(|s| s.mode.as_deref() == Some("focus"))
        {
            let done = snap.intervals_completed.unwrap_or(0) as usize;
            let on_break = snap.phase.as_deref() == Some("break");
            lines.push(Line::from(Span::styled("POMODORO", theme::muted())));
            lines.push(Line::from(vec![
                Span::styled("● ".repeat(done), Style::default().fg(theme::SUCCESS)),
                if on_break {
                    Span::styled("○", Style::default().fg(theme::MUTED))
                } else {
                    Span::styled("●", Style::default().fg(theme::ACCENT))
                },
            ]));
            lines.push(Line::from(Span::styled(
                format!(
                    "{done} done · {} · break excluded",
                    if on_break { "on break" } else { "1 now" }
                ),
                theme::muted(),
            )));
            lines.push(Line::from(""));
        }

        if !self.week.is_empty() {
            let today = jiff::Zoned::now().date();
            let max = self
                .week
                .iter()
                .map(|d| d.minutes)
                .max()
                .unwrap_or(0)
                .max(1);
            const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
            let mut spans: Vec<Span<'static>> = vec![];
            for day in &self.week {
                let idx = if day.minutes == 0 {
                    0
                } else {
                    (((day.minutes as usize) * (BARS.len() - 1)) / max as usize).max(1)
                };
                let style = if day.date == today {
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::SUCCESS)
                };
                spans.push(Span::styled(BARS[idx].to_string(), style));
            }
            lines.push(Line::from(Span::styled("THIS WEEK", theme::muted())));
            lines.push(Line::from(spans));
            lines.push(Line::from(Span::styled("mon → sun", theme::muted())));
            lines.push(Line::from(""));
        }

        lines.push(Line::from(Span::styled("TODAY", theme::muted())));
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
        if matches!(self.panel, Some(Panel::Reconcile { .. })) {
            self.render_reconcile_panel(frame, area);
            return;
        }
        if matches!(self.panel, Some(Panel::Start { .. })) {
            self.render_start_panel(frame, area);
            return;
        }
        if matches!(self.panel, Some(Panel::Reclaim { .. })) {
            self.render_reclaim_panel(frame, area);
            return;
        }
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
                        Span::raw(" to start — pick an activity, or just start and name it later."),
                    ]),
                ];
                frame.render_widget(Paragraph::new(lines).block(bordered("Timer")), area);
            }
            Stage::Stopped { result, label } => {
                let activity = label
                    .clone()
                    .unwrap_or_else(|| format!("activity #{}", result.activity_id));
                let queued = result.segment_id < 0;
                let heading = if queued {
                    Line::from(Span::styled(
                        "◔ segment queued",
                        Style::default()
                            .fg(theme::WARN)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(Span::styled(
                        "✓ segment written",
                        Style::default()
                            .fg(theme::SUCCESS)
                            .add_modifier(Modifier::BOLD),
                    ))
                };
                let detail = if queued {
                    Line::from(Span::styled("will sync when you reconnect", theme::muted()))
                } else {
                    Line::from(Span::styled(
                        format!("segment #{}", result.segment_id),
                        theme::muted(),
                    ))
                };
                let footer = if queued {
                    Line::from(vec![
                        Span::raw("Press "),
                        Span::styled("↵", theme::focused()),
                        Span::raw(" to dismiss — it syncs on its own."),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("Press "),
                        Span::styled("↵", theme::focused()),
                        Span::raw(" to dismiss, or "),
                        Span::styled("u", theme::focused()),
                        Span::raw(" to undo — deletes this segment."),
                    ])
                };
                let lines = vec![
                    Line::from(""),
                    heading,
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!("{} min", result.minutes),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  →  ", theme::muted()),
                        Span::raw(activity),
                    ]),
                    detail,
                    Line::from(""),
                    footer,
                ];
                let title = if queued {
                    "Timer · stopped (queued)"
                } else {
                    "Timer · stopped"
                };
                frame.render_widget(Paragraph::new(lines).block(bordered(title)), area);
            }
            Stage::Live if matches!(self.panel, Some(Panel::Bind { .. })) => {
                self.render_bind_panel(frame, area)
            }
            Stage::Live => self.render_watch_face(frame, area),
        }
    }

    fn render_start_panel(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Start a timer");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (mode, confirm) = match self.panel.as_ref() {
            Some(Panel::Start { mode, confirm }) => (*mode, confirm.clone()),
            _ => return,
        };

        if let Some(picked) = confirm {
            let current = self
                .snapshot
                .as_ref()
                .and_then(|s| s.label.clone())
                .unwrap_or_else(|| "untitled".into());
            let elapsed = self
                .snapshot
                .as_ref()
                .map(|s| widgets::fmt_elapsed(live_elapsed(s, self.base)))
                .unwrap_or_default();
            let lines = vec![
                Line::from(Span::styled(
                    "⚠ One timer at a time",
                    Style::default()
                        .fg(theme::WARN)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("already tracking  ", theme::muted()),
                    Span::styled(current, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  · {elapsed}"), theme::muted()),
                ]),
                Line::from(vec![
                    Span::styled("you picked       ", theme::muted()),
                    Span::styled(picked.title, Style::default().add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "⏎ stops & saves the current timer, then starts the new one",
                    theme::muted(),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(0)])
            .split(inner);

        let (sw_style, focus_style) = match mode {
            PickerMode::Stopwatch => (theme::selection(), theme::muted()),
            PickerMode::Focus => (theme::muted(), theme::selection()),
        };
        let running = self.snapshot.as_ref().is_some_and(|s| s.running);
        let context = if running {
            Line::from(Span::styled(
                "a timer is running — picking a row offers stop & switch",
                Style::default().fg(theme::WARN),
            ))
        } else {
            Line::from(Span::styled("nothing running", theme::muted()))
        };
        let focus_note = self
            .settings
            .as_ref()
            .map(|s| {
                format!(
                    " · focus = your {}m work · {}m break · ×{}",
                    s.focus_work_minutes, s.focus_short_break_minutes, s.focus_long_break_every
                )
            })
            .unwrap_or_default();
        let header = vec![
            Line::from(vec![
                Span::styled(" ● Stopwatch ", sw_style),
                Span::raw("  "),
                Span::styled(" ○ Focus ", focus_style),
                Span::styled(format!("   Tab switches mode{focus_note}"), theme::muted()),
            ]),
            context,
            Line::from(""),
            Line::from(vec![
                Span::raw("bind to  "),
                Span::styled(format!("{}_", self.query), theme::focused()),
            ]),
        ];
        frame.render_widget(Paragraph::new(header), rows[0]);

        let (create, just_start) = self.extra_rows();
        let mut items: Vec<ListItem> = self
            .candidates
            .iter()
            .map(|c| ListItem::new(Line::from(c.title.clone())))
            .collect();
        if create {
            items.push(ListItem::new(Line::from(Span::styled(
                format!(
                    "＋ new activity: \"{}\" — create & start",
                    self.query.trim()
                ),
                Style::default().fg(theme::ACCENT),
            ))));
        }
        if just_start {
            items.push(ListItem::new(Line::from(Span::styled(
                "▶ just start — no activity · untitled, bind when you stop",
                Style::default().fg(theme::SUCCESS),
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

    fn render_reclaim_panel(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Welcome back — the clock went quiet");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(Panel::Reclaim { selected }) = self.panel.as_ref() else {
            return;
        };
        let selected = *selected;
        let Some(snap) = self.snapshot.as_ref() else {
            return;
        };

        let elapsed = live_elapsed(snap, self.base);
        let idle_secs = snap
            .last_interacted_at
            .map(|mark| (jiff::Timestamp::now().as_second() - mark.as_second()).max(0))
            .unwrap_or(0);
        let worked = (elapsed - idle_secs).max(0);
        let last_input = snap
            .last_interacted_at
            .map(|ts| {
                ts.to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "—".into());

        let fmt = widgets::fmt_elapsed;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("running ", theme::muted()),
                Span::styled(fmt(elapsed), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(" · last input {last_input} · idle "),
                    theme::muted(),
                ),
                Span::styled(
                    fmt(idle_secs),
                    Style::default()
                        .fg(theme::WARN)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" → worked ", theme::muted()),
                Span::styled(
                    fmt(worked),
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                snap.label.clone().unwrap_or_else(|| "untitled".into()),
                theme::muted(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "What was the idle tail worth?  (each row = one server verb)",
                theme::muted(),
            )),
        ];

        let rows: [(String, String, bool); RECLAIM_ROWS] = [
            (
                "✓ Trim — idle becomes paused time  (safe pick)".into(),
                format!("keeps {} · timer keeps running", fmt(worked)),
                false,
            ),
            (
                "▸ Keep — the tail counts".into(),
                format!("I really was working · {}", fmt(elapsed)),
                false,
            ),
            (
                "■ Stop at last input".into(),
                format!("saves {} · ends {last_input} · timer ends", fmt(worked)),
                false,
            ),
            (
                "✗ Discard the timer".into(),
                format!("nothing written · −{} · confirms", fmt(elapsed)),
                true,
            ),
        ];
        for (i, (label, caption, danger)) in rows.iter().enumerate() {
            let style = if i == selected {
                theme::selection()
            } else if *danger {
                Style::default().fg(theme::DANGER)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if i == selected { "▌ " } else { "  " }.to_string(), style),
                Span::styled(label.clone(), style.add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("   {caption}"),
                    if i == selected { style } else { theme::muted() },
                ),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Nothing is written until you choose; Esc defers. Idle time is never logged as a segment.",
            theme::muted(),
        )));
        lines.push(Line::from(Span::styled(
            "Attribution follows the start and the 4 AM boundary — reclaim runs before attribution.",
            theme::muted(),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_reconcile_panel(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Panel::Reconcile {
            intent,
            selected,
            confirm_drop,
        }) = self.panel.as_ref()
        else {
            return;
        };
        let IntentState::Diverged {
            status,
            title,
            detail,
            code,
            conflict,
            ..
        } = &intent.state
        else {
            return;
        };
        if rejected_write(intent) {
            return render_rejected_write(
                frame,
                area,
                intent,
                *status,
                title,
                detail,
                *confirm_drop,
            );
        }
        let is_stop = matches!(intent.kind, IntentKind::TimerStop { .. });
        let already_running = code.as_deref() == Some(crate::api::codes::TIMER_ALREADY_RUNNING);
        let gone = code.as_deref() == Some(crate::api::codes::NO_LIVE_TIMER);

        let danger = Style::default()
            .fg(theme::DANGER)
            .add_modifier(Modifier::BOLD);
        let heading = if gone {
            "The session is gone server-side"
        } else if is_stop {
            "The server refused this save"
        } else {
            "Two sessions — which is real?"
        };
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(theme::DANGER))
            .title(Span::styled(format!(" {heading} "), danger));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let local_clock = match &intent.kind {
            IntentKind::TimerStop {
                local_elapsed_s, ..
            } => widgets::fmt_elapsed(*local_elapsed_s),
            _ => self
                .snapshot
                .as_ref()
                .map(|s| widgets::fmt_elapsed(live_elapsed(s, self.base)))
                .unwrap_or_else(|| "—".into()),
        };
        let local_label = self
            .snapshot
            .as_ref()
            .and_then(|s| s.label.clone())
            .unwrap_or_else(|| "untitled".into());
        let age_s = (jiff::Timestamp::now().as_second() - intent.queued_at.as_second()).max(0);
        let identity = format!("{} · queued {} ago", intent.kind.word(), fmt_age(age_s));

        let side = |i: usize, text: String, caption: String| -> Line<'static> {
            let style = if i == *selected {
                theme::selection()
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(if i == *selected { "▌ " } else { "  " }.to_string(), style),
                Span::styled(text, style.add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("   {caption}"),
                    if i == *selected {
                        style
                    } else {
                        theme::muted()
                    },
                ),
            ])
        };

        let objection = format!("{status} {title}");
        let (server_text, server_caption) = if gone {
            (
                "server   —   no live session".to_string(),
                if detail.is_empty() {
                    "stopped or discarded elsewhere — the server has nothing running".to_string()
                } else {
                    detail.clone()
                },
            )
        } else if let Some(current) = conflict.current.as_ref().filter(|_| already_running) {
            let server_label = current.label.clone().unwrap_or_else(|| "untitled".into());
            let since = current
                .started_at
                .map(|ts| {
                    ts.to_zoned(jiff::tz::TimeZone::system())
                        .strftime("%H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "—".into());
            if current.paused {
                // The conflict snapshot carries no paused spans, so there is
                // no honest elapsed to show.
                (
                    format!("server   ‖ paused   {server_label}"),
                    format!("paused on the server · started {since}"),
                )
            } else {
                let server_elapsed = current
                    .started_at
                    .map(|ts| {
                        widgets::fmt_elapsed(
                            (jiff::Timestamp::now().as_second() - ts.as_second()).max(0),
                        )
                    })
                    .unwrap_or_else(|| "—".into());
                (
                    format!("server   {server_elapsed}   {server_label}"),
                    format!("running on the server · started {since}"),
                )
            }
        } else {
            (
                format!("server   {objection}"),
                if detail.is_empty() {
                    "the server's version stands".into()
                } else {
                    detail.clone()
                },
            )
        };

        let mut lines = vec![
            Line::from(Span::styled(
                "the server moved on while this was queued — pick a side; nothing is dropped for you",
                theme::muted(),
            )),
            Line::from(""),
            side(
                0,
                format!("local    {local_clock}   {local_label}"),
                identity,
            ),
            side(1, server_text, server_caption),
            Line::from(""),
        ];
        if already_running && !conflict.resolutions.is_empty() {
            let mapped: Vec<String> = conflict
                .resolutions
                .iter()
                .map(|r| match r.as_str() {
                    "switch" => "switch = keep local".to_string(),
                    "keep-remote" => "keep-remote = take server".to_string(),
                    other => other.to_string(),
                })
                .collect();
            lines.push(Line::from(Span::styled(
                format!("the server offers: {}", mapped.join(" · ")),
                theme::muted(),
            )));
        }
        lines.push(Line::from(Span::styled(
            if gone {
                "keep local writes your minutes as a segment; take server parks your intents for review — nothing is written or dropped behind your back.".to_string()
            } else if is_stop {
                "keep local writes your minutes as a segment; take server parks the stop for review — nothing is written or dropped behind your back.".to_string()
            } else {
                "keeping local stops & saves the server session and yours takes over; taking server parks your intents for review — never deletes them.".to_string()
            },
            theme::muted(),
        )));
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
            inner,
        );
    }

    fn render_bind_panel(&mut self, frame: &mut Frame, area: Rect) {
        let save_on_bind = matches!(
            self.panel,
            Some(Panel::Bind {
                save_on_bind: true,
                ..
            })
        );
        let block = bordered(if save_on_bind {
            "Timer · name it to save it"
        } else {
            "Timer · bind"
        });
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(0)])
            .split(inner);

        let context = if save_on_bind {
            Line::from(Span::styled(
                "clock frozen — an unbound timer can't be saved: bind it, or Esc to keep running",
                Style::default().fg(theme::WARN),
            ))
        } else {
            Line::from("")
        };
        let header = vec![
            self.elapsed_line(),
            context,
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
        match self.panel.as_ref() {
            Some(Panel::Start { confirm, .. }) if confirm.is_some() => {
                return widgets::footer_hints(&[("⏎", "stop & switch"), ("Esc", "keep running")]);
            }
            Some(Panel::Start { .. }) => {
                return Line::from(Span::styled(
                    "type to search · Tab mode · ↑/↓ pick · ⏎ start · Esc cancel",
                    theme::muted(),
                ));
            }
            Some(Panel::Reclaim { .. }) => {
                return widgets::footer_hints(&[
                    ("j/k", "choose"),
                    ("⏎", "apply"),
                    ("Esc", "decide later"),
                ]);
            }
            Some(Panel::Reconcile { intent, .. }) => {
                if rejected_write(intent) {
                    return widgets::footer_hints(&[
                        ("e", "edit times ($EDITOR)"),
                        ("x", "drop it"),
                        ("s", "skip & keep queued"),
                        ("Esc", "decide later"),
                    ]);
                }
                return if matches!(intent.kind, IntentKind::TimerStop { .. }) {
                    widgets::footer_hints(&[
                        ("j/k", "choose"),
                        ("⏎", "keep this side"),
                        ("Esc", "decide later"),
                    ])
                } else {
                    widgets::footer_hints(&[
                        ("j/k", "choose"),
                        ("⏎", "keep this side"),
                        ("b", "keep both, review"),
                        ("Esc", "decide later"),
                    ])
                };
            }
            _ => {}
        }
        match &self.stage {
            Stage::Loading => widgets::footer_hints(&[("h", "home")]),
            Stage::Absent => widgets::footer_hints(&[("s", "start"), ("h", "home")]),
            Stage::Stopped { result, .. } if result.segment_id < 0 => {
                widgets::footer_hints(&[("↵", "dismiss"), ("h", "home")])
            }
            Stage::Stopped { .. } => {
                widgets::footer_hints(&[("u", "undo"), ("↵", "dismiss"), ("h", "home")])
            }
            Stage::Live
                if matches!(
                    self.panel,
                    Some(Panel::Bind {
                        save_on_bind: true,
                        ..
                    })
                ) =>
            {
                Line::from(Span::styled(
                    "type to search · ↑/↓ pick · ⏎ bind & save · Esc keep running",
                    theme::muted(),
                ))
            }
            Stage::Live if matches!(self.panel, Some(Panel::Bind { .. })) => {
                Line::from(Span::styled(
                    "type to search · ↑/↓ pick · ↵ bind/create · Esc cancel",
                    theme::muted(),
                ))
            }
            Stage::Live => {
                let snap = self.snapshot.as_ref();
                let paused = snap.is_some_and(|s| s.paused);
                let bound = snap.is_some_and(|s| s.bound);
                let pp = if paused {
                    ("SPC", "resume")
                } else {
                    ("SPC", "pause")
                };
                let in_focus = snap.is_some_and(|s| s.mode.as_deref() == Some("focus"));
                if bound && in_focus {
                    widgets::footer_hints(&[
                        pp,
                        ("b", "break"),
                        ("n", "skip"),
                        ("m", "mode"),
                        ("s", "end & save"),
                        ("h", "home"),
                    ])
                } else if bound {
                    widgets::footer_hints(&[
                        pp,
                        ("i", "instruments"),
                        ("m", "mode"),
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

fn render_rejected_write(
    frame: &mut Frame,
    area: Rect,
    intent: &Intent,
    status: u16,
    title: &str,
    detail: &str,
    confirm_drop: bool,
) {
    let danger = Style::default()
        .fg(theme::DANGER)
        .add_modifier(Modifier::BOLD);
    let heading = match intent.kind {
        IntentKind::SegmentCreate { .. } => "Server refused this segment",
        _ => "Server refused this log",
    };
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme::DANGER))
        .title(Span::styled(format!(" {heading} "), danger));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let hhmm = |ts: jiff::Timestamp| {
        ts.to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M")
            .to_string()
    };
    let (what, caption) = match &intent.kind {
        IntentKind::SegmentCreate {
            activity_id,
            started_at,
            minutes,
        } => {
            let end = jiff::Timestamp::from_second(started_at.as_second() + *minutes as i64 * 60)
                .unwrap_or(*started_at);
            let target = if *activity_id < 0 {
                "a queued activity".to_string()
            } else {
                format!("activity #{activity_id}")
            };
            (
                format!("segment {}–{}", hhmm(*started_at), hhmm(end)),
                format!("· {minutes}m · {target}"),
            )
        }
        IntentKind::ActivityCreate { body } => {
            let mut caption = body
                .duration_minutes
                .map(|m| format!("· {m}m"))
                .unwrap_or_default();
            if let Some(day) = body.planned_on {
                caption.push_str(&format!(" · planned {day}"));
            }
            (format!("log \"{}\"", body.title), caption)
        }
        other => (other.word().to_string(), String::new()),
    };
    let age_s = (jiff::Timestamp::now().as_second() - intent.queued_at.as_second()).max(0);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(what, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {caption}"), theme::muted()),
            Span::styled(format!("   queued {} ago", fmt_age(age_s)), theme::muted()),
        ]),
        Line::from(vec![
            Span::styled(format!("{status}"), danger),
            Span::styled(
                if detail.is_empty() {
                    format!(" {title}")
                } else {
                    format!(" {title} — {detail}")
                },
                theme::muted(),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "the offending minutes stay in the queue until you decide — nothing is written or dropped behind your back.",
            theme::muted(),
        )),
    ];
    if confirm_drop {
        lines.push(Line::from(Span::styled(
            "x again to drop it — it will never be written",
            danger,
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

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

fn fmt_age(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

enum TimerOp {
    Pause,
    Resume,
}

fn forward_write(
    tx: &UnboundedSender<Action>,
    result: Result<WriteOutcome<TimerSnapshot>, ApiError>,
    context: &str,
) {
    match result {
        Ok(WriteOutcome::Confirmed(t)) => {
            let _ = tx.send(Action::TimerLoaded(Box::new(t)));
        }
        Ok(WriteOutcome::Provisional(t)) => {
            let _ = tx.send(Action::TimerProvisional(Box::new(t)));
        }
        Err(e) => {
            let _ = tx.send(Action::Notify {
                level: Level::Error,
                text: format!("{context}: {e}"),
            });
        }
    }
}

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
        match api.list_activities(&filters).await {
            Ok(list) => {
                let total: u32 = list.data.iter().filter_map(|a| a.duration_minutes).sum();
                let _ = tx.send(Action::TimerTodayLoaded(total));
            }
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(_) => {}
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
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: messages::tile_load_failed(
                        "timer",
                        &messages::fail_reason(api.host(), &e),
                    ),
                });
            }
        }
    });
}

/// `start_timer` takes no mode, so a focus start is a start plus a mode hop.
async fn into_mode(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    started: TimerSnapshot,
    focus: bool,
) -> TimerSnapshot {
    if !focus {
        return started;
    }
    match api.timer_mode("focus").await {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(Action::Notify {
                level: Level::Warning,
                text: format!("started, but focus mode refused: {e}"),
            });
            started
        }
    }
}

fn spawn_start_blank(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "start failed", e),
        };
        match queued.start_timer(None, false).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let t = into_mode(&api, &tx, t, focus).await;
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            // The mode hop is live-only; a queued start stays a stopwatch.
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
            }
            Err(e) => notify_seam_error(&tx, "start failed", e),
        }
    });
}

/// No `switch`: a running timer goes through the conflict banner first, and a
/// racing 409 still surfaces.
fn spawn_start_bound(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    activity_id: i64,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "start failed", e),
        };
        match queued.start_timer(Some(activity_id), false).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let t = into_mode(&api, &tx, t, focus).await;
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
            }
            Err(e) => notify_seam_error(&tx, "start failed", e),
        }
    });
}

fn spawn_start_switch(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    activity_id: i64,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "stop & switch failed", e),
        };
        match queued.start_timer(Some(activity_id), true).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let t = into_mode(&api, &tx, t, focus).await;
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
            }
            Err(e) => notify_seam_error(&tx, "stop & switch failed", e),
        }
    });
}

/// A blank start, then a bind-with-title, so the server mints the activity.
fn spawn_create_and_start(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    title: String,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "start failed", e),
        };
        match queued.start_timer(None, false).await {
            Ok(WriteOutcome::Confirmed(_)) => match queued.bind_timer(None, Some(title)).await {
                Ok(out) => {
                    let t = into_mode(&api, &tx, out.into_value(), focus).await;
                    let _ = tx.send(Action::TimerLoaded(Box::new(t)));
                }
                Err(e) => {
                    let _ = tx.send(Action::TimerReload);
                    let _ = tx.send(Action::Notify {
                        level: Level::Warning,
                        text: format!("started, but naming it failed: {e} — bind with `/`"),
                    });
                }
            },
            Ok(WriteOutcome::Provisional(started)) => {
                match queued.bind_timer(None, Some(title)).await {
                    Ok(out) => {
                        let _ = tx.send(Action::TimerProvisional(Box::new(out.into_value())));
                    }
                    Err(_) => {
                        let _ = tx.send(Action::TimerProvisional(Box::new(started)));
                    }
                }
            }
            Err(e) => notify_seam_error(&tx, "start failed", e),
        }
    });
}

fn spawn_op(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths, op: TimerOp) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "timer op failed", e),
        };
        let result = match op {
            TimerOp::Pause => queued.pause_timer().await,
            TimerOp::Resume => queued.resume_timer().await,
        };
        forward_write(&tx, result, "timer op failed");
    });
}

fn spawn_stop(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "stop failed", e),
        };
        match queued.stop_timer().await {
            // A queued stop carries a negative `segment_id`; the confirmation
            // view keys on it.
            Ok(out) => {
                let _ = tx.send(Action::TimerStopped(Box::new(out.into_value())));
                // `TimerCleared` is app-only: it clears the header cell without
                // disturbing the screen's confirmation view.
                let _ = tx.send(Action::TimerCleared);
            }
            Err(e) => notify_seam_error(&tx, "stop failed", e),
        }
    });
}

fn spawn_discard(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "discard failed", e),
        };
        match queued.discard_timer().await {
            Ok(WriteOutcome::Confirmed(_)) => match api.timer().await {
                Ok(t) => {
                    let _ = tx.send(Action::TimerLoaded(Box::new(t)));
                }
                Err(_) => {
                    let _ = tx.send(Action::TimerCleared);
                }
            },
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
                let _ = tx.send(Action::TimerCleared);
            }
            Err(e) => notify_seam_error(&tx, "discard failed", e),
        }
    });
}

fn spawn_bind(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    target: BindTarget,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "bind failed", e),
        };
        let result = match target {
            BindTarget::Existing(id) => queued.bind_timer(Some(id), None).await,
            BindTarget::Create(title) => queued.bind_timer(None, Some(title)).await,
        };
        forward_write(&tx, result, "bind failed");
    });
}

/// The bind result is forwarded before the stop so the confirmation can name
/// the activity.
fn spawn_bind_then_stop(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    target: BindTarget,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "bind failed", e),
        };
        let bound = match target {
            BindTarget::Existing(id) => queued.bind_timer(Some(id), None).await,
            BindTarget::Create(title) => queued.bind_timer(None, Some(title)).await,
        };
        match bound {
            Ok(WriteOutcome::Confirmed(t)) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Ok(WriteOutcome::Provisional(t)) => {
                let _ = tx.send(Action::TimerProvisional(Box::new(t)));
            }
            Err(e) => return notify_seam_error(&tx, "bind failed", e),
        }
        match queued.stop_timer().await {
            Ok(out) => {
                let _ = tx.send(Action::TimerStopped(Box::new(out.into_value())));
                let _ = tx.send(Action::TimerCleared);
            }
            Err(e) => {
                let _ = tx.send(Action::TimerReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("bound, but the save failed: {e}"),
                });
            }
        }
    });
}

fn spawn_reclaim(api: &ApiClient, tx: &UnboundedSender<Action>, verb: ReclaimVerb) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.reclaim_timer(verb).await {
            Ok(Reclaimed::Running(t)) => {
                let _ = tx.send(Action::TimerLoaded(t));
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: match verb {
                        ReclaimVerb::Trim => "trimmed — idle moved to paused time".into(),
                        _ => "kept — the tail counts".into(),
                    },
                });
            }
            Ok(Reclaimed::Stopped(stopped)) => {
                let _ = tx.send(Action::TimerStopped(Box::new(stopped)));
                let _ = tx.send(Action::TimerCleared);
            }
            Err(e) => {
                let _ = tx.send(Action::TimerReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("reclaim failed: {e}"),
                });
            }
        }
    });
}

fn spawn_phase(api: &ApiClient, tx: &UnboundedSender<Action>, to: &'static str) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.timer_phase(to).await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Warning,
                    text: format!("phase change refused: {e}"),
                });
            }
        }
    });
}

fn spawn_skip_interval(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Err(e) = api.timer_phase("break").await {
            let _ = tx.send(Action::Notify {
                level: Level::Warning,
                text: format!("skip refused: {e}"),
            });
            return;
        }
        match api.timer_phase("work").await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: "interval banked — next one armed".into(),
                });
            }
            Err(e) => {
                let _ = tx.send(Action::TimerReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Warning,
                    text: format!("interval banked, but re-arming refused: {e}"),
                });
            }
        }
    });
}

fn spawn_mode(api: &ApiClient, tx: &UnboundedSender<Action>, mode: &'static str) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.timer_mode(mode).await {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Warning,
                    text: format!("mode switch refused: {e}"),
                });
            }
        }
    });
}

/// Spawned so the reducer never blocks on the store. An unreadable queue reads
/// as no divergence here; `engineer queue` is where that is loud.
fn spawn_diverged_check(tx: &UnboundedSender<Action>, paths: QueuePaths) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let store = match &paths {
            Some((queue, _)) => QueueStore::at(queue.clone()),
            None => match QueueStore::open_default() {
                Ok(store) => store,
                Err(_) => return,
            },
        };
        let found = store
            .intents()
            .ok()
            .and_then(|intents| intents.into_iter().find(Intent::is_diverged));
        let _ = tx.send(Action::TimerDivergedLoaded(found.map(Box::new)));
    });
}

/// Through the same `queue::resolve` engine `engineer queue resolve` calls, so
/// the gesture and the flag cannot drift.
fn spawn_resolve(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    intent_id: u64,
    resolution: Resolution,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "reconcile failed", e),
        };
        match queued.resolve_divergence(intent_id, resolution).await {
            Ok(resolved) => {
                let text = match resolved {
                    Resolved::SwitchedToLocal => {
                        "kept local — the server stopped & saved its session; yours took over"
                            .to_string()
                    }
                    Resolved::SegmentWritten {
                        segment_id,
                        minutes,
                        ..
                    } => format!("kept — {minutes}m written (segment {segment_id}); nothing lost"),
                    Resolved::Parked { count } => format!(
                        "took server — {count} intent{} parked for review, nothing deleted",
                        if count == 1 { "" } else { "s" }
                    ),
                };
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text,
                });
                let tx2 = tx.clone();
                if let Some(report) = queued
                    .drain_reporting(|intent| {
                        let _ = tx2.send(Action::ReplayProgress {
                            word: intent.kind.word().to_string(),
                        });
                    })
                    .await
                {
                    let _ = tx.send(Action::ReplayFinished(report));
                }
                let _ = tx.send(Action::TimerReload);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("reconcile failed: {e} — the intent stays diverged"),
                });
                let _ = tx.send(Action::TimerReload);
            }
        }
    });
}

#[derive(Clone, Copy)]
enum RejectGesture {
    Drop,
    Skip,
}

/// Through the same `queue` functions `engineer queue resolve --drop/--skip`
/// calls.
fn spawn_reject_gesture(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    intent_id: u64,
    gesture: RejectGesture,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "reconcile failed", e),
        };
        let store = match queue_store(&paths) {
            Ok(store) => store,
            Err(e) => return notify_seam_error(&tx, "reconcile failed", e),
        };
        let outcome = match gesture {
            RejectGesture::Drop => crate::queue::drop_intent(&store, intent_id).map(|dropped| {
                format!(
                    "dropped — the queued {} left the queue; nothing was written",
                    dropped.kind.word()
                )
            }),
            RejectGesture::Skip => crate::queue::skip_intent(&store, intent_id).map(|skipped| {
                format!(
                    "skipped — the {} stays in the queue (parked), nothing lost",
                    skipped.kind.word()
                )
            }),
        };
        match outcome {
            Ok(text) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text,
                });
                drain_behind(&queued, &tx).await;
                let _ = tx.send(Action::TimerReload);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("reconcile failed: {e} — the intent stays as it was"),
                });
                let _ = tx.send(Action::TimerReload);
            }
        }
    });
}

fn spawn_edit_apply(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    intent_id: u64,
    buffer: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "edit failed", e),
        };
        let store = match queue_store(&paths) {
            Ok(store) => store,
            Err(e) => return notify_seam_error(&tx, "edit failed", e),
        };
        match crate::queue::apply_edit(&store, intent_id, &buffer) {
            Ok(updated) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("edited — retrying the queued {}", updated.kind.word()),
                });
                drain_behind(&queued, &tx).await;
                let _ = tx.send(Action::TimerReload);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("edit refused: {e} — the intent stays diverged"),
                });
                let _ = tx.send(Action::TimerReload);
            }
        }
    });
}

fn queue_store(paths: &QueuePaths) -> Result<QueueStore, crate::queue::QueueError> {
    match paths {
        Some((queue, _)) => Ok(QueueStore::at(queue.clone())),
        None => QueueStore::open_default(),
    }
}

/// Continue the drain behind a resolved choice, streaming the reconnect
/// transcript.
async fn drain_behind(queued: &crate::queue::QueuedClient, tx: &UnboundedSender<Action>) {
    let tx2 = tx.clone();
    if let Some(report) = queued
        .drain_reporting(|intent| {
            let _ = tx2.send(Action::ReplayProgress {
                word: intent.kind.word().to_string(),
            });
        })
        .await
    {
        let _ = tx.send(Action::ReplayFinished(report));
    }
}

fn spawn_week(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.get_progress(None).await {
            Ok(progress) => {
                let _ = tx.send(Action::TimerWeekLoaded(progress.by_day));
            }
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(_) => {}
        }
    });
}

fn spawn_settings(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Ok(s) = api.timer_settings().await {
            let _ = tx.send(Action::SettingsLoaded(Box::new(s)));
        }
    });
}

fn spawn_undo(api: &ApiClient, tx: &UnboundedSender<Action>, activity_id: i64, segment_id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.delete_segment(activity_id, segment_id).await {
            Ok(()) => {
                let _ = tx.send(Action::TimerUndone);
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: "segment removed — nothing written".into(),
                });
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("undo failed: {e}"),
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
        match api.timer_candidates(q).await {
            Ok(list) => {
                let _ = tx.send(Action::TimerCandidatesLoaded(list));
            }
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(_) => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    /// A per-test scratch (queue.json, cache) so a spawned offline write lands
    /// in a throwaway dir, never the shared XDG queue.
    fn scratch_paths() -> (PathBuf, PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-timer-screen-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("queue.json"), dir.join("timer-cache.json"))
    }

    /// A screen plus a live api/tx. The receiver is leaked so the background
    /// tasks spawned by intent actions still have a live sender (their results,
    /// which would hit a non-existent dev server, are irrelevant to state). The
    /// screen's queue seam points at a scratch dir, so those tasks never touch
    /// the shared XDG queue.
    fn setup() -> (Timer, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        let screen = Timer {
            queue_paths: Some(scratch_paths()),
            ..Timer::default()
        };
        (screen, api, tx)
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
    async fn a_401_from_the_timer_read_routes_to_reauth() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_load(&api, &tx);

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a message arrives")
            .expect("channel open");
        assert!(matches!(got, Action::SessionExpired), "got {got:?}");
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
        assert!(matches!(s.panel, Some(Panel::Bind { .. })));

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
        assert!(s.panel.is_none());
        // Still Live — cancelling the panel never touches the clock.
        assert!(matches!(s.stage, Stage::Live));
    }

    #[tokio::test]
    async fn save_on_absent_opens_the_start_picker() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSave).await;
        assert!(matches!(
            s.panel,
            Some(Panel::Start {
                mode: PickerMode::Stopwatch,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn tab_toggles_picker_mode_and_focus_submit_names_the_gap() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSave).await;
        feed(&mut s, &api, &tx, Action::TimerPickerToggleMode).await;
        assert!(matches!(
            s.panel,
            Some(Panel::Start {
                mode: PickerMode::Focus,
                ..
            })
        ));
        // A focus start (start + mode hop) closes the picker like any other.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerCandidatesLoaded(vec![TimerCandidate {
                id: 7,
                title: "SICP reading".into(),
            }]),
        )
        .await;
        assert!(s.handle(Action::TimerBindSubmit, &api, &tx).await.is_none());
        assert!(s.panel.is_none(), "a focus start closes the picker");
    }

    #[tokio::test]
    async fn just_start_row_starts_unnamed_and_closes_the_picker() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSave).await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerCandidatesLoaded(vec![TimerCandidate {
                id: 7,
                title: "SICP reading".into(),
            }]),
        )
        .await;
        // candidates(1) + just-start (no query → no create row).
        feed(&mut s, &api, &tx, Action::TimerBindMove(1)).await;
        assert!(matches!(s.start_target(), Some(StartTarget::JustStart)));
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        assert!(s.panel.is_none(), "submit closes the picker");
    }

    #[tokio::test]
    async fn switch_flow_requires_a_second_enter_and_esc_steps_back() {
        let (mut s, api, tx) = setup();
        // A bound timer is running; `/` opens the picker in switch context.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Read DDIA ch.7",
                "elapsed_seconds": 3134
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        assert!(matches!(s.panel, Some(Panel::Start { .. })));
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerCandidatesLoaded(vec![TimerCandidate {
                id: 42,
                title: "Implement Raft".into(),
            }]),
        )
        .await;

        // First ⏎ arms the conflict banner instead of switching.
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        match &s.panel {
            Some(Panel::Start {
                confirm: Some(c), ..
            }) => assert_eq!(c.id, 42),
            other => panic!("expected the conflict banner, got {}", other.is_some()),
        }

        // Esc steps back to the list, keeping the current timer running.
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        assert!(matches!(s.panel, Some(Panel::Start { confirm: None, .. })));

        // Re-arm and confirm: the second ⏎ closes the picker (stop & switch).
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        assert!(s.panel.is_none());
    }

    #[tokio::test]
    async fn switch_context_hides_create_and_just_start_rows() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        feed(&mut s, &api, &tx, Action::TimerBindInput('x')).await;
        // Query typed, but switch context offers existing activities only.
        assert_eq!(s.extra_rows(), (false, false));
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

    /// The buffer text of the screen rendered at 100×32 — the offline markers
    /// are read straight from the watch face / confirmation view.
    fn rendered(s: &mut Timer) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal
            .draw(|frame| s.render(frame, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[tokio::test]
    async fn provisional_write_flips_the_marker_and_a_live_read_clears_it() {
        let (mut s, api, tx) = setup();
        // A queued offline pause lands as the provisional twin.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerProvisional(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "paused": true,
                "label": "consensus", "elapsed_seconds": 1800
            })))),
        )
        .await;
        assert!(matches!(s.stage, Stage::Live));
        assert!(s.provisional, "a queued write is provisional");
        assert!(
            rendered(&mut s).contains("QUEUED"),
            "the watch face wears the ◔ queued marker"
        );

        // A live read is server truth — the marker clears.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "paused": true, "label": "consensus"
            })))),
        )
        .await;
        assert!(!s.provisional, "a confirmed read clears it");
        assert!(!rendered(&mut s).contains("QUEUED"));
    }

    #[tokio::test]
    async fn offline_stop_confirmation_reads_queued_not_written() {
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
        // The offline stop's synthesized confirmation: a negative segment id.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerStopped(Box::new(TimerStopped {
                stopped: true,
                activity_id: 9,
                segment_id: -1,
                minutes: 25,
            })),
        )
        .await;
        let text = rendered(&mut s);
        assert!(text.contains("segment queued"), "{text}");
        assert!(text.contains("will sync"), "{text}");
        assert!(!text.contains("segment written"), "not confirmed: {text}");
        // The undo key is gone — there is no server segment to delete yet.
        let hints: String = s
            .hints()
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(!hints.contains("undo"), "{hints}");
        // And `u` is inert while it is queued.
        assert!(s.handle(Action::TimerUndo, &api, &tx).await.is_none());
        assert!(matches!(s.stage, Stage::Stopped { .. }), "still confirming");
    }

    #[tokio::test]
    async fn an_offline_keystroke_lands_in_the_queue() {
        // The full wiring, offline: a keystroke → the spawned write helper →
        // `QueuedClient` → the persisted queue. A dead api forces the offline
        // arm; the seeded read cache is the snapshot the pause freezes.
        use url::Url;
        let (queue_path, cache_path) = scratch_paths();
        crate::timer_cache::store_at(
            &cache_path,
            &snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "consensus", "elapsed_seconds": 1800
            })),
        );
        // reqwest fails before any response on this port — `ApiError::Transport`.
        let api = ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        let mut s = Timer {
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Timer::default()
        };
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "consensus", "elapsed_seconds": 1800
            })))),
        )
        .await;
        // The pause gesture is never refused offline — it queues.
        feed(&mut s, &api, &tx, Action::TimerPauseResume).await;

        // The write is spawned; wait for it to land (the dead port refuses fast).
        let store = QueueStore::at(&queue_path);
        let mut pending = store.pending().unwrap();
        for _ in 0..100 {
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            pending = store.pending().unwrap();
        }
        assert_eq!(pending.len(), 1, "the gesture landed in the queue");
        assert_eq!(pending[0].kind.word(), "pause");
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
        super::palette_dispatch(verb, snap.as_ref(), &api, &tx, Some(scratch_paths()))
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
        // Live + unbound → bind-at-stop: the frozen bind picker, save-armed.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false
            })))),
        )
        .await;
        assert!(s.handle(Action::TimerSave, &api, &tx).await.is_none());
        assert!(matches!(
            s.panel,
            Some(Panel::Bind {
                save_on_bind: true,
                froze: true,
            })
        ));

        // Esc keeps it running: the panel closes (and resume is spawned).
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        assert!(s.panel.is_none());

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
    async fn bind_at_stop_does_not_refreeze_a_paused_clock() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false, "paused": true
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSave).await;
        // Already paused → the panel did not freeze, so Esc must not resume.
        assert!(matches!(
            s.panel,
            Some(Panel::Bind {
                save_on_bind: true,
                froze: false,
            })
        ));
    }

    #[tokio::test]
    async fn discard_past_the_fence_asks_twice() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "elapsed_seconds": 2460
            })))),
        )
        .await;
        // First `d` arms the confirm and warns; nothing is discarded.
        let warn = s.handle(Action::TimerDiscard, &api, &tx).await;
        match warn {
            Some((Level::Warning, text)) => assert!(text.contains("again to confirm"), "{text}"),
            other => panic!("expected the discard confirm, got {other:?}"),
        }
        assert!(s.discard_armed);
        // The second consecutive `d` goes through (no further warning).
        assert!(s.handle(Action::TimerDiscard, &api, &tx).await.is_none());
        assert!(!s.discard_armed);
    }

    #[tokio::test]
    async fn any_other_key_disarms_the_discard_confirm() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "elapsed_seconds": 2460
            })))),
        )
        .await;
        s.handle(Action::TimerDiscard, &api, &tx).await;
        assert!(s.discard_armed);
        feed(&mut s, &api, &tx, Action::TimerToggleRail).await;
        assert!(!s.discard_armed, "another action disarms");
        // The next `d` warns again instead of discarding.
        assert!(s.handle(Action::TimerDiscard, &api, &tx).await.is_some());
    }

    #[tokio::test]
    async fn short_timers_discard_instantly() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false, "elapsed_seconds": 45
            })))),
        )
        .await;
        assert!(s.handle(Action::TimerDiscard, &api, &tx).await.is_none());
    }

    #[tokio::test]
    async fn undo_reloads_into_the_empty_face() {
        let (mut s, api, tx) = setup();
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
        assert!(matches!(s.stage, Stage::Stopped { .. }));
        feed(&mut s, &api, &tx, Action::TimerUndone).await;
        assert!(matches!(s.stage, Stage::Loading));
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
    async fn mode_switch_needs_a_running_timer() {
        let (mut s, api, tx) = setup();
        // Nothing running → the warning; mode is picked at start.
        let warn = s.handle(Action::TimerModeSwitch, &api, &tx).await;
        assert!(matches!(warn, Some((Level::Warning, _))));

        // Running → accepted (dispatches the in-place mode switch).
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true
            })))),
        )
        .await;
        assert!(s.handle(Action::TimerModeSwitch, &api, &tx).await.is_none());
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

    #[tokio::test]
    async fn focus_work_face_names_the_interval_and_dots() {
        use ratatui::{backend::TestBackend, Terminal};

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Implement Raft",
                "mode": "focus", "phase": "work", "intervals_completed": 2,
                "elapsed_seconds": 1928
            })))),
        )
        .await;

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| s.render(frame, frame.area()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("WORK · INTERVAL 3"), "focus label");
        assert!(content.contains("POMODORO"), "rail instrument");
    }

    #[tokio::test]
    async fn break_face_is_muted_and_never_a_segment() {
        use ratatui::{backend::TestBackend, Terminal};

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Implement Raft",
                "mode": "focus", "phase": "break", "intervals_completed": 3,
                "elapsed_seconds": 252
            })))),
        )
        .await;

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| s.render(frame, frame.area()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("BREAK — NOT COUNTING"), "break label");
        assert!(content.contains("never a segment"), "break caption");
    }

    #[tokio::test]
    async fn skip_and_break_route_by_phase() {
        let (mut s, api, tx) = setup();
        // Stopwatch: `n` stays quiet; `b` keeps its bind meaning (unbound →
        // the bind panel).
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": false
            })))),
        )
        .await;
        assert!(s
            .handle(Action::TimerSkipInterval, &api, &tx)
            .await
            .is_none());
        feed(&mut s, &api, &tx, Action::TimerBreak).await;
        assert!(matches!(s.panel, Some(Panel::Bind { .. })));
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;

        // Focus work: `n` and `b` dispatch phase calls (no warnings).
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "mode": "focus", "phase": "work"
            })))),
        )
        .await;
        assert!(s
            .handle(Action::TimerSkipInterval, &api, &tx)
            .await
            .is_none());
        assert!(s.handle(Action::TimerBreak, &api, &tx).await.is_none());
        assert!(
            s.panel.is_none(),
            "b in focus is the phase toggle, not bind"
        );
    }

    #[tokio::test]
    async fn over_face_is_amber_and_names_the_plan() {
        use ratatui::{backend::TestBackend, Terminal};

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Implement Raft",
                "over": true, "planned_minutes": 120, "logged_minutes": 18,
                "elapsed_seconds": 8320
            })))),
        )
        .await;

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| s.render(frame, frame.area()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("PAST THE PLAN"), "over label");
        assert!(content.contains("planned 2h 00m"), "plan context");
        assert!(
            content.contains("never stops anything"),
            "no auto-stop note"
        );
    }

    #[tokio::test]
    async fn rail_sparkline_renders_the_week_and_degrades_without_it() {
        use ratatui::{backend::TestBackend, Terminal};

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Read DDIA",
                "elapsed_seconds": 600
            })))),
        )
        .await;

        // Without by_day the rail shows only the TODAY block.
        let render = |s: &mut Timer| {
            let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
            terminal
                .draw(|frame| s.render(frame, frame.area()))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol().to_string())
                .collect::<String>()
        };
        let content = render(&mut s);
        assert!(!content.contains("THIS WEEK"), "degrades without by_day");
        assert!(content.contains("TODAY"));

        let days: Vec<crate::api::DayMinutes> = serde_json::from_value(serde_json::json!([
            { "date": "2026-07-06", "minutes": 60 },
            { "date": "2026-07-07", "minutes": 227 },
            { "date": "2026-07-08", "minutes": 0 },
            { "date": "2026-07-09", "minutes": 0 },
            { "date": "2026-07-10", "minutes": 0 },
            { "date": "2026-07-11", "minutes": 0 },
            { "date": "2026-07-12", "minutes": 0 }
        ]))
        .unwrap();
        feed(&mut s, &api, &tx, Action::TimerWeekLoaded(days)).await;
        let content = render(&mut s);
        assert!(content.contains("THIS WEEK"), "sparkline block present");
        assert!(content.contains("mon → sun"));
        assert!(content.contains('█'), "the max day renders a full bar");
    }

    #[test]
    fn offer_for_judges_the_finished_phase() {
        let settings = knobs("trim");
        let now = jiff::Timestamp::from_second(10_000).unwrap();
        let at = |secs_ago: i64| {
            jiff::Timestamp::from_second(10_000 - secs_ago)
                .unwrap()
                .to_string()
        };

        // Work interval past 50m → the break offer; the 4th is long.
        let work_done = snapshot(serde_json::json!({
            "running": true, "mode": "focus", "phase": "work",
            "intervals_completed": 2, "phase_started_at": at(50 * 60)
        }));
        assert_eq!(
            offer_for(&work_done, &settings, now),
            Some(Offer::Break { long: false })
        );
        let fourth = snapshot(serde_json::json!({
            "running": true, "mode": "focus", "phase": "work",
            "intervals_completed": 3, "phase_started_at": at(50 * 60)
        }));
        assert_eq!(
            offer_for(&fourth, &settings, now),
            Some(Offer::Break { long: true })
        );

        // Mid-interval → no offer; paused → never an offer.
        let mid = snapshot(serde_json::json!({
            "running": true, "mode": "focus", "phase": "work",
            "phase_started_at": at(10 * 60)
        }));
        assert_eq!(offer_for(&mid, &settings, now), None);
        let paused = snapshot(serde_json::json!({
            "running": true, "paused": true, "mode": "focus", "phase": "work",
            "phase_started_at": at(90 * 60)
        }));
        assert_eq!(offer_for(&paused, &settings, now), None);

        // A short break past 10m → back to work.
        let break_done = snapshot(serde_json::json!({
            "running": true, "mode": "focus", "phase": "break",
            "intervals_completed": 3, "phase_started_at": at(10 * 60)
        }));
        assert_eq!(
            offer_for(&break_done, &settings, now),
            Some(Offer::BackToWork)
        );
        // The 4th (long) break still has 10 of its 20 minutes left.
        let long_break_running = snapshot(serde_json::json!({
            "running": true, "mode": "focus", "phase": "break",
            "intervals_completed": 4, "phase_started_at": at(10 * 60)
        }));
        assert_eq!(offer_for(&long_break_running, &settings, now), None);

        // Stopwatch never offers.
        let stopwatch = snapshot(serde_json::json!({
            "running": true, "phase_started_at": at(90 * 60)
        }));
        assert_eq!(offer_for(&stopwatch, &settings, now), None);
    }

    fn knobs(default_reclaim: &str) -> crate::api::TimerSettings {
        serde_json::from_value(serde_json::json!({
            "timer_mode": "stopwatch",
            "focus_work_minutes": 50,
            "focus_short_break_minutes": 10,
            "focus_long_break_minutes": 20,
            "focus_long_break_every": 4,
            "idle_guard_enabled": true,
            "idle_threshold_minutes": 15,
            "idle_default_reclaim": default_reclaim,
            "audit_long_hours": 6,
            "audit_short_seconds": 60,
            "audit_badge_enabled": true,
            "overrun_ping_enabled": true
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn idle_load_opens_reclaim_with_the_settings_default() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::SettingsLoaded(Box::new(knobs("keep"))),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true,
                "elapsed_seconds": 9660,
                "last_interacted_at": "2026-07-05T15:12:00Z"
            })))),
        )
        .await;
        assert!(
            matches!(s.panel, Some(Panel::Reclaim { selected: 1 })),
            "keep is row 1"
        );
    }

    #[tokio::test]
    async fn reclaim_esc_defers_and_the_next_idle_poll_reopens() {
        let (mut s, api, tx) = setup();
        let idle_load = || {
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true
            }))))
        };
        feed(&mut s, &api, &tx, idle_load()).await;
        assert!(matches!(s.panel, Some(Panel::Reclaim { .. })));

        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        assert!(s.panel.is_none(), "Esc defers");

        feed(&mut s, &api, &tx, idle_load()).await;
        assert!(
            matches!(s.panel, Some(Panel::Reclaim { .. })),
            "the guard returns while the read stays idle"
        );
    }

    #[tokio::test]
    async fn reclaim_selection_moves_and_clamps() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindMove(1)).await;
        feed(&mut s, &api, &tx, Action::TimerBindMove(10)).await;
        assert!(matches!(s.panel, Some(Panel::Reclaim { selected: 3 })));
        feed(&mut s, &api, &tx, Action::TimerBindMove(-10)).await;
        assert!(matches!(s.panel, Some(Panel::Reclaim { selected: 0 })));
    }

    #[tokio::test]
    async fn reclaim_applies_and_a_settled_read_keeps_it_closed() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true
            })))),
        )
        .await;
        // ⏎ on the default row applies the verb and closes the list.
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        assert!(s.panel.is_none());
        // The settled (non-idle) read that follows keeps it closed.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": false
            })))),
        )
        .await;
        assert!(s.panel.is_none());
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
    fn fmt_minutes_reads_hours_and_padded_minutes() {
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

    #[test]
    fn live_elapsed_prefers_the_started_at_arithmetic() {
        let started =
            jiff::Timestamp::from_second(jiff::Timestamp::now().as_second() - 500).unwrap();
        let anchored = snapshot(serde_json::json!({
            "running": true, "paused": false,
            "started_at": started.to_string(), "elapsed_seconds": 100
        }));
        // The server arithmetic wins over the stale snapshot figure, with or
        // without a monotonic base — the clock never depends on poll age.
        let elapsed = live_elapsed(&anchored, None);
        assert!((500..=502).contains(&elapsed), "got {elapsed}");
    }

    // ------------------------------------------------- the reconcile panel

    /// A diverged intent as the queue check would deliver it — the code-less
    /// generic fallback.
    fn diverged_intent(kind: IntentKind) -> Intent {
        Intent {
            id: 7,
            idempotency_key: "key-7".into(),
            stream: kind.stream(),
            queued_at: jiff::Timestamp::now(),
            kind,
            state: IntentState::Diverged {
                status: 409,
                title: "Conflict".into(),
                detail: "a timer is already running".into(),
                type_uri: None,
                errors: vec![],
                code: None,
                conflict: Default::default(),
            },
            attempts: 1,
            last_error: None,
        }
    }

    /// A coded divergence: swap the generic objection for a `code` +
    /// extensions capture.
    fn coded(mut intent: Intent, code: &str, conflict: serde_json::Value) -> Intent {
        if let IntentState::Diverged {
            code: c,
            conflict: cf,
            ..
        } = &mut intent.state
        {
            *c = Some(code.into());
            *cf = serde_json::from_value(conflict).unwrap();
        }
        intent
    }

    fn diverged_start() -> Intent {
        diverged_intent(IntentKind::TimerStart {
            activity_id: Some(9),
            switch: false,
            at: jiff::Timestamp::now(),
        })
    }

    fn diverged_stop() -> Intent {
        diverged_intent(IntentKind::TimerStop {
            at: jiff::Timestamp::now(),
            local_elapsed_s: 2832,
        })
    }

    #[tokio::test]
    async fn a_waiting_divergence_opens_the_reconcile_panel_and_a_cleared_one_closes_it() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        assert!(
            matches!(s.panel, Some(Panel::Reconcile { selected: 0, .. })),
            "the panel opens on the local side"
        );

        // Resolved elsewhere (e.g. `engineer queue resolve`): the next check
        // reports no divergence and the stale panel closes.
        feed(&mut s, &api, &tx, Action::TimerDivergedLoaded(None)).await;
        assert!(s.panel.is_none());
    }

    #[tokio::test]
    async fn the_reconcile_panel_never_steals_an_open_picker() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSave).await; // start picker open
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        assert!(
            matches!(s.panel, Some(Panel::Start { .. })),
            "mid-gesture pickers stand; the panel opens on the next check"
        );
    }

    #[tokio::test]
    async fn reconcile_selection_moves_between_the_two_sides_and_clamps() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindMove(1)).await;
        assert!(matches!(
            s.panel,
            Some(Panel::Reconcile { selected: 1, .. })
        ));
        feed(&mut s, &api, &tx, Action::TimerBindMove(5)).await;
        assert!(
            matches!(s.panel, Some(Panel::Reconcile { selected: 1, .. })),
            "two sides only"
        );
        feed(&mut s, &api, &tx, Action::TimerBindMove(-5)).await;
        assert!(matches!(
            s.panel,
            Some(Panel::Reconcile { selected: 0, .. })
        ));
    }

    #[tokio::test]
    async fn reconcile_keys_route_choose_keep_both_and_defer() {
        use crossterm::event::KeyModifiers;
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('j'))),
            Some(Action::TimerBindMove(1))
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('b'))),
            Some(Action::TimerReconcileBoth)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Enter)),
            Some(Action::TimerBindSubmit)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::TimerBindCancel)
        ));
        // Esc defers: the panel closes; the divergence stands in the queue,
        // so the next poll's check reopens it.
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        assert!(s.panel.is_none());
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        assert!(matches!(s.panel, Some(Panel::Reconcile { .. })));
    }

    #[tokio::test]
    async fn reconcile_submit_dispatches_the_resolution_and_closes() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        // ⏎ on a side hands the choice to the shared resolve engine and the
        // panel closes; the outcome lands as notify + reload (spawned).
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        assert!(s.panel.is_none());
    }

    #[tokio::test]
    async fn keep_both_refuses_on_a_diverged_stop() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_stop()))),
        )
        .await;
        let warned = s.handle(Action::TimerReconcileBoth, &api, &tx).await;
        let (level, text) = warned.expect("a warning is surfaced");
        assert_eq!(level, Level::Warning);
        assert!(text.contains("one segment at stake"), "{text}");
        assert!(
            matches!(s.panel, Some(Panel::Reconcile { .. })),
            "the choice is still open"
        );
    }

    #[tokio::test]
    async fn reconcile_hints_offer_keep_both_only_when_two_sessions_diverged() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;
        let hints = format!("{:?}", s.hints());
        assert!(hints.contains("keep this side"), "{hints}");
        assert!(hints.contains("keep both"), "{hints}");
        assert!(hints.contains("decide later"), "{hints}");

        // A diverged stop has one segment at stake — no `b` gesture.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_stop()))),
        )
        .await;
        let hints = format!("{:?}", s.hints());
        assert!(hints.contains("keep this side"), "{hints}");
        assert!(!hints.contains("keep both"), "{hints}");
    }

    #[tokio::test]
    async fn reconcile_panel_renders_the_objection_and_both_sides() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (mut s, api, tx) = setup();
        // A live local clock so the local side has an elapsed to show.
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Raft leader election",
                "elapsed_seconds": 2832
            })))),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_start()))),
        )
        .await;

        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Two sessions — which is real?"), "{text}");
        assert!(text.contains("local"), "{text}");
        assert!(text.contains("409 Conflict"), "{text}");
        assert!(text.contains("start · queued"), "the intent's identity");
        assert!(text.contains("never deletes them"), "{text}");
    }

    // ------------------------------------------- the rejected write's face

    /// A diverged `SegmentCreate`, exactly as a 422 on replay parks it.
    fn rejected_segment() -> Intent {
        let mut intent = diverged_intent(IntentKind::SegmentCreate {
            activity_id: 9,
            started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
            minutes: 45,
        });
        if let IntentState::Diverged {
            status,
            title,
            detail,
            ..
        } = &mut intent.state
        {
            *status = 422;
            *title = "Segment overlaps".into();
            *detail = "overlaps an existing segment 14:20–15:05 (web)".into();
        }
        intent
    }

    /// Seed the screen's scratch store with a diverged segment and open the
    /// panel on it — the full path a drop/skip gesture mutates.
    fn seeded_rejected_screen() -> (Timer, ApiClient, mpsc::UnboundedSender<Action>, QueueStore) {
        let (mut s, api, tx) = setup();
        let (queue_path, _) = s.queue_paths.clone().unwrap();
        let store = QueueStore::at(&queue_path);
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 422,
                    title: "Segment overlaps".into(),
                    detail: "overlaps an existing segment 14:20–15:05 (web)".into(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Default::default(),
                };
            })
            .unwrap();
        let intent = store.intents().unwrap().remove(0);
        s.panel = Some(Panel::Reconcile {
            intent: Box::new(intent),
            selected: 0,
            confirm_drop: false,
        });
        (s, api, tx, store)
    }

    /// Poll the store until `pred` holds — the spawned gesture lands async.
    async fn wait_for(store: &QueueStore, pred: impl Fn(&[Intent]) -> bool) -> Vec<Intent> {
        for _ in 0..200 {
            let intents = store.intents().unwrap();
            if pred(&intents) {
                return intents;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        store.intents().unwrap()
    }

    #[tokio::test]
    async fn the_rejected_face_routes_e_x_s_and_defers_never_the_sides() {
        use crossterm::event::KeyModifiers;
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(rejected_segment()))),
        )
        .await;
        let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('e'))),
            Some(Action::TimerReconcileEdit)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('x'))),
            Some(Action::TimerReconcileDrop)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('s'))),
            Some(Action::TimerReconcileSkip)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::TimerBindCancel)
        ));
        // No sides on this face: j/k/⏎/b do nothing.
        assert!(s.intercept_key(press(KeyCode::Char('j'))).is_none());
        assert!(s.intercept_key(press(KeyCode::Enter)).is_none());
        assert!(s.intercept_key(press(KeyCode::Char('b'))).is_none());
    }

    #[tokio::test]
    async fn rejected_drop_arms_warns_then_the_second_x_drops_from_the_queue() {
        let (mut s, api, tx, store) = seeded_rejected_screen();

        // First `x`: armed, loud, nothing dropped.
        let warned = s.handle(Action::TimerReconcileDrop, &api, &tx).await;
        let (level, text) = warned.expect("the confirm is surfaced");
        assert_eq!(level, Level::Warning);
        assert!(text.contains("x` again"), "{text}");
        assert!(
            matches!(
                s.panel,
                Some(Panel::Reconcile {
                    confirm_drop: true,
                    ..
                })
            ),
            "armed, still open"
        );
        assert_eq!(store.intents().unwrap().len(), 1, "nothing left the queue");

        // Any other gesture disarms — a stray `x` later must not drop.
        feed(&mut s, &api, &tx, Action::TimerToggleRail).await;
        assert!(matches!(
            s.panel,
            Some(Panel::Reconcile {
                confirm_drop: false,
                ..
            })
        ));

        // Arm again, confirm: the intent leaves the queue — explicitly.
        feed(&mut s, &api, &tx, Action::TimerReconcileDrop).await;
        feed(&mut s, &api, &tx, Action::TimerReconcileDrop).await;
        assert!(s.panel.is_none(), "the choice is made");
        let intents = wait_for(&store, |i| i.is_empty()).await;
        assert!(intents.is_empty(), "the one user-chosen delete");
    }

    #[tokio::test]
    async fn rejected_skip_parks_the_intent_and_keeps_it_stored() {
        let (mut s, api, tx, store) = seeded_rejected_screen();
        feed(&mut s, &api, &tx, Action::TimerReconcileSkip).await;
        assert!(s.panel.is_none());
        let intents = wait_for(&store, |i| i.iter().all(Intent::is_parked)).await;
        assert_eq!(intents.len(), 1, "kept in the queue — never deleted");
        match &intents[0].state {
            IntentState::Parked { reason } => {
                assert!(reason.starts_with("skipped"), "{reason}")
            }
            other => panic!("expected parked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejected_edit_hands_the_seed_to_the_editor_hand_off() {
        // A live receiver this time — the assertion is the dispatched action.
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Timer {
            queue_paths: Some(scratch_paths()),
            ..Timer::default()
        };
        s.handle(
            Action::TimerDivergedLoaded(Some(Box::new(rejected_segment()))),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::TimerReconcileEdit, &api, &tx).await;
        let action = rx.try_recv().expect("the hand-off is dispatched");
        match action {
            Action::QueueIntentEdit { intent_id, seed } => {
                assert_eq!(intent_id, 7);
                assert!(seed.contains("started_at: 2026-07-15T14:02:00Z"), "{seed}");
                assert!(seed.contains("minutes: 45"), "{seed}");
                assert!(seed.contains("422 Segment overlaps"), "{seed}");
            }
            other => panic!("expected the editor hand-off, got {other:?}"),
        }
        assert!(
            matches!(s.panel, Some(Panel::Reconcile { .. })),
            "still diverged until the saved buffer applies"
        );
    }

    #[tokio::test]
    async fn rejected_edit_apply_repends_the_corrected_intent() {
        let (mut s, api, tx, store) = seeded_rejected_screen();
        let id = store.intents().unwrap()[0].id;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerReconcileEditApply {
                intent_id: id,
                buffer: "started_at: 2026-07-15T15:10:00Z\nminutes: 30".into(),
            },
        )
        .await;
        assert!(s.panel.is_none());
        // The retry-drain hits the dev api (dead) — the intent stays *pending*
        // with the corrected payload: re-pended, never lost.
        let intents = wait_for(&store, |i| i.iter().all(Intent::is_pending)).await;
        assert_eq!(intents.len(), 1);
        match &intents[0].kind {
            IntentKind::SegmentCreate {
                started_at,
                minutes,
                ..
            } => {
                assert_eq!(started_at.to_string(), "2026-07-15T15:10:00Z");
                assert_eq!(*minutes, 30);
            }
            other => panic!("expected the corrected segment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_rejected_face_renders_the_refused_write_and_its_three_gestures() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(rejected_segment()))),
        )
        .await;

        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Server refused this segment"), "{text}");
        assert!(text.contains("422 Segment overlaps"), "{text}");
        assert!(text.contains("45m"), "the gestured minutes: {text}");
        assert!(
            text.contains("nothing is written or dropped"),
            "the never-silent line: {text}"
        );

        let hints = format!("{:?}", s.hints());
        assert!(hints.contains("edit times"), "{hints}");
        assert!(hints.contains("drop it"), "{hints}");
        assert!(hints.contains("skip & keep queued"), "{hints}");
        assert!(hints.contains("decide later"), "{hints}");
    }

    // ------------------------------------------------- the coded conflicts

    #[tokio::test]
    async fn reconcile_panel_renders_the_server_session_from_the_coded_conflict() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Raft leader election",
                "elapsed_seconds": 2832
            })))),
        )
        .await;
        // Started an hour ago so the server row has a live elapsed to show.
        let started =
            jiff::Timestamp::from_second(jiff::Timestamp::now().as_second() - 3600).unwrap();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(coded(
                diverged_start(),
                "timer-already-running",
                serde_json::json!({
                    "current": {
                        "id": 114, "activity_id": 258777238, "label": "Ruby OOP Study",
                        "started_at": started.to_string(), "paused": false
                    },
                    "resolutions": ["switch", "keep-remote"]
                }),
            )))),
        )
        .await;

        let text = rendered(&mut s);
        assert!(text.contains("Two sessions — which is real?"), "{text}");
        assert!(
            text.contains("Ruby OOP Study"),
            "the server session's label, not just problem prose: {text}"
        );
        assert!(
            text.contains("1:00:0"),
            "started_at becomes a live elapsed: {text}"
        );
        assert!(text.contains("running on the server"), "{text}");
        assert!(
            !text.contains("409 Conflict"),
            "the informed row replaces the bare objection: {text}"
        );
        // The server's resolution hints, mapped onto the shipped gestures.
        assert!(text.contains("switch = keep local"), "{text}");
        assert!(text.contains("keep-remote = take server"), "{text}");
    }

    #[tokio::test]
    async fn reconcile_panel_marks_a_paused_server_session_instead_of_counting_its_gap() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(coded(
                diverged_start(),
                "timer-already-running",
                serde_json::json!({
                    "current": {
                        "id": 114, "activity_id": 9, "label": "Ruby OOP Study",
                        "started_at": "2026-07-16T08:59:03Z", "paused": true
                    },
                    "resolutions": ["switch", "keep-remote"]
                }),
            )))),
        )
        .await;

        let text = rendered(&mut s);
        assert!(text.contains("‖ paused"), "{text}");
        assert!(text.contains("paused on the server"), "{text}");
    }

    #[tokio::test]
    async fn reconcile_panel_says_the_session_is_gone_on_no_live_timer() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(coded(
                diverged_intent(IntentKind::TimerPause {
                    at: jiff::Timestamp::now(),
                }),
                "no-live-timer",
                serde_json::json!({}),
            )))),
        )
        .await;

        let text = rendered(&mut s);
        assert!(text.contains("The session is gone server-side"), "{text}");
        assert!(text.contains("no live session"), "{text}");
        assert!(
            text.contains("keep local writes your minutes as a segment"),
            "keep-local composes via create_segment: {text}"
        );
        assert!(text.contains("take server parks"), "{text}");
    }

    // ------------------------------------------------ against a live server

    /// A screen whose api points at `server`, with a live receiver for the
    /// actions its spawned work sends back.
    fn served(
        server: &wiremock::MockServer,
    ) -> (
        Timer,
        ApiClient,
        mpsc::UnboundedSender<Action>,
        mpsc::UnboundedReceiver<Action>,
    ) {
        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        let screen = Timer {
            queue_paths: Some(scratch_paths()),
            ..Timer::default()
        };
        (screen, api, tx, rx)
    }

    /// The first action `pred` accepts, skipping the rest.
    async fn recv_matching(
        rx: &mut mpsc::UnboundedReceiver<Action>,
        pred: impl Fn(&Action) -> bool,
    ) -> Action {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let action = rx.recv().await.expect("channel open");
                if pred(&action) {
                    return action;
                }
            }
        })
        .await
        .expect("a matching action arrives")
    }

    /// The paths and JSON bodies `server` has received on `path_prefix`.
    async fn received(
        server: &wiremock::MockServer,
        path_prefix: &str,
    ) -> Vec<(String, serde_json::Value)> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with(path_prefix))
            .map(|r| {
                (
                    r.url.path().to_string(),
                    serde_json::from_slice(&r.body).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect()
    }

    /// Wait (up to two seconds) until `server` holds `n` requests on
    /// `path_prefix` — the spawned writes land asynchronously.
    async fn await_requests(server: &wiremock::MockServer, path_prefix: &str, n: usize) {
        for _ in 0..200 {
            if received(server, path_prefix).await.len() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn ago(secs: i64) -> String {
        jiff::Timestamp::from_second(jiff::Timestamp::now().as_second() - secs)
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn a_finished_phase_offers_the_long_break_from_settings_and_never_fires_it() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/phase"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let (mut s, api, tx, _rx) = served(&server);
        let mut settings = knobs("trim");
        settings.focus_long_break_every = 3;
        settings.focus_long_break_minutes = 35;
        feed(
            &mut s,
            &api,
            &tx,
            Action::SettingsLoaded(Box::new(settings)),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Implement Raft",
                "mode": "focus", "phase": "work", "intervals_completed": 2,
                "phase_started_at": ago(60 * 60), "elapsed_seconds": 3600
            })))),
        )
        .await;

        let text = rendered(&mut s);
        assert!(text.contains("INTERVAL 3 COMPLETE"), "{text}");
        assert!(text.contains("b start 35m long break"), "{text}");
        assert!(text.contains("nothing fires on its own"), "{text}");

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            s.snapshot.as_ref().and_then(|t| t.phase.as_deref()),
            Some("work"),
            "the phase waits for a key"
        );
    }

    #[tokio::test]
    async fn a_paused_timer_opens_no_reclaim_unless_the_read_says_idle() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::SettingsLoaded(Box::new(knobs("trim"))),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "paused": true, "idle": false,
                "label": "Read DDIA ch.7", "elapsed_seconds": 1800,
                "last_interacted_at": ago(3 * 3600)
            })))),
        )
        .await;
        assert!(s.panel.is_none(), "no reclaim list for a paused clock");
        let text = rendered(&mut s);
        assert!(text.contains("PAUSED — NOT COUNTING"), "{text}");
        assert!(!text.contains("IDLE"), "{text}");
    }

    #[tokio::test]
    async fn a_mode_switch_keeps_the_running_clock_and_closes_no_segment() {
        use wiremock::matchers::{body_json, method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/mode"))
            .and(body_json(serde_json::json!({ "mode": "focus" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "bound": true, "mode": "focus", "phase": "work",
                "elapsed_seconds": 1928
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/v1/timer/stop"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(path_regex("/segments"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let (mut s, api, tx, mut rx) = served(&server);
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "elapsed_seconds": 1928
            })))),
        )
        .await;
        assert!(s.handle(Action::TimerModeSwitch, &api, &tx).await.is_none());

        let switched = recv_matching(&mut rx, |a| matches!(a, Action::TimerLoaded(_))).await;
        feed(&mut s, &api, &tx, switched).await;
        assert!(
            matches!(s.stage, Stage::Live),
            "still counting, never stopped"
        );
        let snap = s.snapshot.as_ref().unwrap();
        assert_eq!(snap.mode.as_deref(), Some("focus"));
        assert_eq!(snap.elapsed_seconds, Some(1928));
    }

    #[tokio::test]
    async fn enter_dismisses_the_stop_confirmation_and_reloads() {
        let (mut s, api, tx) = setup();
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
        feed(&mut s, &api, &tx, Action::TimerDismissStopped).await;
        assert!(matches!(s.stage, Stage::Loading));
    }

    #[tokio::test]
    async fn esc_from_bind_at_stop_resumes_only_what_it_froze() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn gestures_after_save_then_esc(paused: bool) -> Vec<String> {
            let server = MockServer::start().await;
            for verb in ["pause", "resume"] {
                Mock::given(method("POST"))
                    .and(path(format!("/api/v1/timer/{verb}")))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "running": true, "bound": false, "paused": verb == "pause"
                    })))
                    .mount(&server)
                    .await;
            }
            let (mut s, api, tx, _rx) = served(&server);
            s.handle(
                Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                    "running": true, "bound": false, "paused": paused
                })))),
                &api,
                &tx,
            )
            .await;
            s.handle(Action::TimerSave, &api, &tx).await;
            let froze = usize::from(!paused);
            await_requests(&server, "/api/v1/timer/pause", froze).await;
            s.handle(Action::TimerBindCancel, &api, &tx).await;
            await_requests(&server, "/api/v1/timer/resume", froze).await;
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            received(&server, "/api/v1/timer/")
                .await
                .into_iter()
                .map(|(p, _)| p)
                .filter(|p| p.ends_with("/pause") || p.ends_with("/resume"))
                .collect()
        }

        assert_eq!(
            gestures_after_save_then_esc(false).await,
            vec!["/api/v1/timer/pause", "/api/v1/timer/resume"],
            "the panel froze a running clock, so Esc resumes it"
        );
        assert!(
            gestures_after_save_then_esc(true).await.is_empty(),
            "a clock the user paused stays paused"
        );
    }

    #[tokio::test]
    async fn the_reclaim_list_preselects_trim_before_settings_arrive() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true
            })))),
        )
        .await;
        assert!(matches!(s.panel, Some(Panel::Reclaim { selected: 0 })));
    }

    #[tokio::test]
    async fn the_reclaim_list_is_a_chooser_not_a_search() {
        use crossterm::event::KeyModifiers;
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true
            })))),
        )
        .await;
        let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(s.intercept_key(press(KeyCode::Char('x'))).is_none());
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('j'))),
            Some(Action::TimerBindMove(1))
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Enter)),
            Some(Action::TimerBindSubmit)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::TimerBindCancel)
        ));
    }

    #[tokio::test]
    async fn every_landed_snapshot_rechecks_the_queue_for_a_divergence() {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (queue_path, cache_path) = scratch_paths();
        let store = QueueStore::at(&queue_path);
        store
            .enqueue(IntentKind::TimerPause {
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 409,
                    title: "Conflict".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: vec![],
                    code: None,
                    conflict: Default::default(),
                };
            })
            .unwrap();
        let mut s = Timer {
            queue_paths: Some((queue_path, cache_path)),
            ..Timer::default()
        };
        s.handle(
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
            &api,
            &tx,
        )
        .await;
        let found = recv_matching(&mut rx, |a| matches!(a, Action::TimerDivergedLoaded(_))).await;
        assert!(matches!(found, Action::TimerDivergedLoaded(Some(_))));
    }

    #[tokio::test]
    async fn a_background_poll_leaves_the_start_picker_open_but_closes_a_finished_bind() {
        let (mut s, api, tx) = setup();
        let absent =
            || Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false }))));
        feed(&mut s, &api, &tx, absent()).await;
        feed(&mut s, &api, &tx, Action::TimerSave).await;
        feed(&mut s, &api, &tx, absent()).await;
        assert!(matches!(s.panel, Some(Panel::Start { .. })), "mid-browse");

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
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true
            })))),
        )
        .await;
        assert!(s.panel.is_none(), "bound — the bind panel's job is done");
    }

    #[tokio::test]
    async fn a_read_that_is_no_longer_idle_closes_the_reclaim_list() {
        let (mut s, api, tx) = setup();
        let load = |idle: bool| {
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": idle
            }))))
        };
        feed(&mut s, &api, &tx, load(true)).await;
        assert!(matches!(s.panel, Some(Panel::Reclaim { .. })));
        feed(&mut s, &api, &tx, load(false)).await;
        assert!(s.panel.is_none(), "the decision landed elsewhere");
    }

    #[tokio::test]
    async fn a_provisional_write_never_clears_a_pending_stop_confirmation() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerStopped(Box::new(TimerStopped {
                stopped: true,
                activity_id: 9,
                segment_id: -1,
                minutes: 25,
            })),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerProvisional(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        assert!(matches!(s.stage, Stage::Stopped { .. }));
    }

    #[tokio::test]
    async fn slash_on_an_empty_face_opens_the_start_picker() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({ "running": false })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        assert!(matches!(s.panel, Some(Panel::Start { .. })));
    }

    #[tokio::test]
    async fn the_reclaim_discard_row_rides_the_normal_discard_flow() {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Timer {
            queue_paths: Some(scratch_paths()),
            ..Timer::default()
        };
        s.handle(
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true, "elapsed_seconds": 9660
            })))),
            &api,
            &tx,
        )
        .await;
        s.handle(Action::TimerBindMove(3), &api, &tx).await;
        s.handle(Action::TimerBindSubmit, &api, &tx).await;
        let sent = recv_matching(&mut rx, |a| matches!(a, Action::TimerDiscard)).await;
        assert!(matches!(sent, Action::TimerDiscard));
    }

    #[tokio::test]
    async fn a_narrow_face_drops_the_rail() {
        use ratatui::{backend::TestBackend, Terminal};
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "consensus", "elapsed_seconds": 600
            })))),
        )
        .await;
        let mut terminal = Terminal::new(TestBackend::new(60, 32)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("TRACKING"), "{text}");
        assert!(!text.contains("TODAY"), "no room for the rail: {text}");
    }

    #[tokio::test]
    async fn a_deferred_reclaim_leaves_the_face_saying_idle() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true, "elapsed_seconds": 9660
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindCancel).await;
        let text = rendered(&mut s);
        assert!(text.contains("IDLE — RECLAIM PENDING"), "{text}");
    }

    #[tokio::test]
    async fn the_big_number_is_muted_while_not_counting_and_amber_past_the_plan() {
        use ratatui::{backend::TestBackend, style::Color, Terminal};

        async fn digit_colour(json: serde_json::Value) -> Color {
            let (mut s, api, tx) = setup();
            feed(
                &mut s,
                &api,
                &tx,
                Action::TimerLoaded(Box::new(snapshot(json))),
            )
            .await;
            let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
            terminal.draw(|f| s.render(f, f.area())).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .find(|c| c.symbol() == "█")
                .expect("the big digits render")
                .fg
        }

        let counting =
            serde_json::json!({ "running": true, "bound": true, "elapsed_seconds": 600 });
        let paused = serde_json::json!({
            "running": true, "bound": true, "paused": true, "elapsed_seconds": 600
        });
        let on_break = serde_json::json!({
            "running": true, "bound": true, "mode": "focus", "phase": "break", "elapsed_seconds": 600
        });
        let over = serde_json::json!({
            "running": true, "bound": true, "over": true, "elapsed_seconds": 600
        });
        assert_eq!(digit_colour(counting).await, theme::ACCENT);
        assert_eq!(digit_colour(paused).await, theme::MUTED);
        assert_eq!(digit_colour(on_break).await, theme::MUTED);
        assert_eq!(digit_colour(over).await, theme::WARN);
    }

    #[tokio::test]
    async fn the_conflict_banner_replaces_the_picker_list() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "Read DDIA ch.7"
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindBegin).await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerCandidatesLoaded(vec![TimerCandidate {
                id: 42,
                title: "Implement Raft".into(),
            }]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerBindSubmit).await;
        let text = rendered(&mut s);
        assert!(text.contains("One timer at a time"), "{text}");
        assert!(text.contains("already tracking  Read DDIA ch.7"), "{text}");
        assert!(
            !text.contains("Tab switches mode"),
            "the list is gone: {text}"
        );
    }

    #[tokio::test]
    async fn reclaim_captions_split_the_run_into_idle_and_worked() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "idle": true, "elapsed_seconds": 5400,
                "last_interacted_at": ago(1800)
            })))),
        )
        .await;
        let text = rendered(&mut s);
        assert!(text.contains("idle 30:0"), "{text}");
        assert!(text.contains("worked 1:00:0"), "{text}");
    }

    #[tokio::test]
    async fn a_diverged_stop_shows_its_own_gestured_elapsed() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "label": "consensus", "elapsed_seconds": 100
            })))),
        )
        .await;
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerDivergedLoaded(Some(Box::new(diverged_stop()))),
        )
        .await;
        let text = rendered(&mut s);
        assert!(text.contains("local    47:12"), "{text}");
    }

    #[test]
    fn fmt_age_reads_the_largest_whole_unit() {
        assert_eq!(fmt_age(42), "42s");
        assert_eq!(fmt_age(7 * 60 + 59), "7m");
        assert_eq!(fmt_age(3 * 3600 + 5), "3h");
        assert_eq!(fmt_age(2 * 86_400 + 7), "2d");
    }

    #[test]
    fn a_confirmed_write_lands_live_and_a_queued_one_provisional() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let t = || snapshot(serde_json::json!({ "running": true }));
        forward_write(&tx, Ok(WriteOutcome::Confirmed(t())), "op");
        forward_write(&tx, Ok(WriteOutcome::Provisional(t())), "op");
        forward_write(&tx, Err(ApiError::Unauthorized), "op");
        assert!(matches!(rx.try_recv(), Ok(Action::TimerLoaded(_))));
        assert!(matches!(rx.try_recv(), Ok(Action::TimerProvisional(_))));
        assert!(matches!(
            rx.try_recv(),
            Ok(Action::Notify {
                level: Level::Error,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn background_reads_route_a_401_to_reauth_and_swallow_other_failures() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn actions_for_status(status: u16) -> Vec<Action> {
            let server = MockServer::start().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
            let (tx, mut rx) = mpsc::unbounded_channel();
            spawn_today(&api, &tx);
            spawn_week(&api, &tx);
            spawn_candidates(&api, &tx, String::new());
            await_requests(&server, "/", 3).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let mut got = Vec::new();
            while let Ok(action) = rx.try_recv() {
                got.push(action);
            }
            got
        }

        let unauthorized = actions_for_status(401).await;
        assert_eq!(unauthorized.len(), 3, "{unauthorized:?}");
        assert!(unauthorized
            .iter()
            .all(|a| matches!(a, Action::SessionExpired)));
        let failed = actions_for_status(500).await;
        assert!(
            failed.is_empty(),
            "no tile for a background read: {failed:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_timer_read_tiles_an_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_load(&api, &tx);
        let got = recv_matching(&mut rx, |_| true).await;
        assert!(
            matches!(
                got,
                Action::Notify {
                    level: Level::Error,
                    ..
                }
            ),
            "got {got:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_focus_hop_keeps_the_stopwatch_start_and_says_so() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "running": true, "bound": false, "mode": "stopwatch"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/mode"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "title": "Unprocessable", "detail": "focus is off"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_start_blank(&api, &tx, Some(scratch_paths()), true);

        let warned = recv_matching(&mut rx, |a| matches!(a, Action::Notify { .. })).await;
        match warned {
            Action::Notify { level, text } => {
                assert_eq!(level, Level::Warning);
                assert!(text.contains("focus mode refused"), "{text}");
            }
            other => panic!("expected the warning, got {other:?}"),
        }
        let started = recv_matching(&mut rx, |a| matches!(a, Action::TimerLoaded(_))).await;
        match started {
            Action::TimerLoaded(t) => assert!(t.running, "the start stands"),
            other => panic!("expected the started timer, got {other:?}"),
        }
    }

    async fn phase_calls_for_skip_from(phase: &str) -> Vec<serde_json::Value> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/phase"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running": true, "mode": "focus", "phase": "work"
            })))
            .mount(&server)
            .await;
        let (mut s, api, tx, _rx) = served(&server);
        feed(
            &mut s,
            &api,
            &tx,
            Action::TimerLoaded(Box::new(snapshot(serde_json::json!({
                "running": true, "bound": true, "mode": "focus", "phase": phase
            })))),
        )
        .await;
        feed(&mut s, &api, &tx, Action::TimerSkipInterval).await;
        await_requests(&server, "/api/v1/timer/phase", 2).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        received(&server, "/api/v1/timer/phase")
            .await
            .into_iter()
            .map(|(_, body)| body)
            .collect()
    }

    #[tokio::test]
    async fn skip_mid_work_banks_the_interval_then_rearms_work() {
        assert_eq!(
            phase_calls_for_skip_from("work").await,
            vec![
                serde_json::json!({ "to": "break" }),
                serde_json::json!({ "to": "work" })
            ]
        );
    }

    #[tokio::test]
    async fn skip_on_a_break_just_returns_to_work() {
        assert_eq!(
            phase_calls_for_skip_from("break").await,
            vec![serde_json::json!({ "to": "work" })]
        );
    }

    #[tokio::test]
    async fn an_unparseable_edit_is_refused_loudly_and_stays_diverged() {
        let (mut s, api, _, store) = seeded_rejected_screen();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let id = store.intents().unwrap()[0].id;
        s.handle(
            Action::TimerReconcileEditApply {
                intent_id: id,
                buffer: "minutes: 0".into(),
            },
            &api,
            &tx,
        )
        .await;
        let refused = recv_matching(&mut rx, |a| matches!(a, Action::Notify { .. })).await;
        match refused {
            Action::Notify { level, text } => {
                assert_eq!(level, Level::Error);
                assert!(text.contains("edit refused"), "{text}");
            }
            other => panic!("expected the refusal, got {other:?}"),
        }
        assert!(store.intents().unwrap()[0].is_diverged());
    }
}
