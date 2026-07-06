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

use crate::api::{
    ApiClient, ReclaimVerb, Reclaimed, Timer as TimerSnapshot, TimerCandidate, TimerStopped,
};
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
                spawn_start_blank(api, tx, false);
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

/// Which offer the focus rhythm is holding open, if any (§Focus offers).
/// Transitions never fire on their own — a finished phase waits for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// The work interval is complete: offer the break (long every Nth).
    Break { long: bool },
    /// The break is done: offer the next work interval.
    BackToWork,
}

/// A finished focus phase, judged from `phase_started_at` against the
/// configured durations. Paused and idle clocks never hold an offer open
/// (their moments come first). Phase time ignores mid-phase pauses — the
/// offer may arrive early after one; the server still validates.
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
                    long: every != 0 && next_break % every == 0,
                }
            })
        }
        Some("break") => {
            let banked = snap.intervals_completed.unwrap_or(0);
            let long = every != 0 && banked != 0 && banked % every == 0;
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

/// Past this much elapsed work, discarding asks twice — in the TUI a second
/// `d`, headless `--force`. Under it, a mis-tap discards instantly.
pub(crate) const DISCARD_CONFIRM_SECS: i64 = 120;

/// What a bind submission would act on, resolved from the current selection.
enum BindTarget {
    Existing(i64),
    Create(String),
}

/// The start picker's stopwatch ⇄ focus toggle (`Tab`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Stopwatch,
    Focus,
}

/// Which live-search panel owns the keys. `Bind` names the running unnamed
/// timer in place; `Start` is the §Start-a-timer picker — one list, every way
/// in (bound / new activity / just start), plus the stop-&-switch confirm
/// when a timer is already running.
enum Panel {
    Bind {
        /// Bind-at-stop (§Bind at stop): a successful bind immediately saves —
        /// the server's bound-only stop, with the picker in between.
        save_on_bind: bool,
        /// Whether opening the panel paused the clock (the design's frozen
        /// moment). Esc resumes only what this panel froze — a manually
        /// paused timer stays paused.
        froze: bool,
    },
    Start {
        mode: PickerMode,
        confirm: Option<TimerCandidate>,
    },
    /// §Idle reclaim: the clock went quiet — one row per server verb plus the
    /// discard escape. Nothing is written until a row is applied; Esc defers.
    Reclaim { selected: usize },
}

/// The reclaim list rows, in display order: trim · keep · stop · discard.
const RECLAIM_ROWS: usize = 4;

/// What a start-picker submission would do, resolved from the selection.
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
    /// Monotonic baseline for ticking the displayed elapsed between snapshots.
    base: Option<Instant>,
    /// `i` folds the instrument rail away; the number recenters into the calm
    /// watch face. Default is the cockpit (rail shown).
    rail_hidden: bool,
    /// Today's logged minutes (summed from today's activities) for the rail.
    today_minutes: Option<u32>,
    /// The week's per-day minutes (mon→sun) for the rail's sparkline; empty
    /// until the progress read lands (or on servers without `by_day`).
    week: Vec<crate::api::DayMinutes>,
    /// The open live-search panel (bind or start picker), if any. The panel
    /// owns the keys while open; the clock underneath runs untouched.
    panel: Option<Panel>,
    /// A `d` past the confirm fence arms this; only the very next `d`
    /// confirms the discard.
    discard_armed: bool,
    /// The per-user knobs — the reclaim default and the focus copy read them.
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
    }

    /// The live focus phase (`work`/`break`), or `None` outside focus.
    fn focus_phase(&self) -> Option<&str> {
        let snap = self.snapshot.as_ref()?;
        (snap.running && snap.mode.as_deref() == Some("focus"))
            .then(|| snap.phase.as_deref().unwrap_or("work"))
    }

    /// The offer the face is holding open, if settings have arrived.
    fn current_offer(&self) -> Option<Offer> {
        let snap = self.snapshot.as_ref()?;
        let settings = self.settings.as_ref()?;
        offer_for(snap, settings, jiff::Timestamp::now())
    }

    /// The reclaim list's preselected row, from the `idle_default_reclaim`
    /// knob. Trim (the safe pick) when settings haven't arrived.
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

    /// While the bind panel is open, the screen owns every key (a live search):
    /// characters filter, arrows pick, Enter binds/creates, Esc closes. This
    /// runs before the global keymap, so the timer keeps running untouched.
    ///
    /// On the live face the screen also claims `Space` as pause ⇄ resume (the
    /// design's `SPC`), so the leader is unavailable only while a clock runs on
    /// this screen — navigation still has `h`, `t`, and the `:` verbs.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(panel) = self.panel.as_ref() else {
            if key.code == KeyCode::Char(' ') && matches!(self.stage, Stage::Live) {
                return Some(Action::TimerPauseResume);
            }
            return None;
        };
        // The reclaim list is a plain chooser, not a live search: j/k move,
        // ⏎ applies, Esc defers — typing does nothing.
        if matches!(panel, Panel::Reclaim { .. }) {
            return match key.code {
                KeyCode::Esc => Some(Action::TimerBindCancel),
                KeyCode::Enter => Some(Action::TimerBindSubmit),
                KeyCode::Char('j') | KeyCode::Down => Some(Action::TimerBindMove(1)),
                KeyCode::Char('k') | KeyCode::Up => Some(Action::TimerBindMove(-1)),
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
        // A discard confirm is strictly two consecutive `d`s — anything else
        // disarms it.
        if self.discard_armed && !matches!(action, Action::TimerDiscard) {
            self.discard_armed = false;
        }
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
                // A background poll must not close the start picker mid-browse;
                // the bind panel closes once its job is done (bound or gone).
                if matches!(self.panel, Some(Panel::Bind { .. })) && (t.bound || !t.running) {
                    self.close_panel();
                }
                // The idle guard: a quiet clock opens the reclaim list (the
                // default verb from settings preselected); a read that is no
                // longer idle closes it — the decision landed elsewhere.
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
            }
            Action::TimerReload => spawn_load(api, tx),
            // `s` — stage-dependent primary: open the start picker when
            // absent, end & save when bound, and the bind-first warning when
            // unbound (the full bind-at-stop flow is its own ticket).
            Action::TimerSave => match self.stage {
                Stage::Absent => self.open_start_panel(api, tx),
                Stage::Live => {
                    if self.snapshot.as_ref().is_some_and(|s| s.bound) {
                        spawn_stop(api, tx);
                    } else {
                        // §Bind at stop: freeze the clock and name it to save it.
                        self.open_bind_at_stop(api, tx);
                    }
                }
                _ => {}
            },
            Action::TimerToggleRail => self.rail_hidden = !self.rail_hidden,
            // `m` — stopwatch ⇄ focus in place; elapsed is preserved.
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
            // `n` — bank the interval and arm the next: work → break → work
            // (interval credit is the work→break edge). On a break it simply
            // returns to work early.
            Action::TimerSkipInterval => match self.focus_phase() {
                Some("work") => spawn_skip_interval(api, tx),
                Some("break") => spawn_phase(api, tx, "work"),
                _ => {}
            },
            // `b` — the phase toggle in focus (break now / back to work); in
            // stopwatch it keeps its bind meaning: the bind panel when
            // unbound, the start picker when bound.
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
                    self.open_bind_at_stop(api, tx);
                }
            }
            Action::TimerUndo => {
                if let Stage::Stopped { result, .. } = &self.stage {
                    spawn_undo(api, tx, result.activity_id, result.segment_id);
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
                // Past ~2 minutes real work is at stake: ask twice (§Saved &
                // undo). A mis-tap discards instantly.
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
                spawn_discard(api, tx);
            }
            Action::TimerBindBegin => match self.stage {
                // Unbound: name the running timer in place. Bound: open the
                // start picker in switch context (§Start conflict). Absent:
                // the same picker `s` opens.
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
            Action::TimerBindCancel => {
                // Esc steps back one level: a pending switch-confirm first,
                // then the panel itself. Leaving bind-at-stop resumes only
                // what the panel froze.
                match self.panel.as_mut() {
                    Some(Panel::Start { confirm, .. }) if confirm.is_some() => {
                        *confirm = None;
                        return None;
                    }
                    Some(Panel::Bind {
                        save_on_bind: true,
                        froze,
                    }) => {
                        if *froze {
                            spawn_op(api, tx, TimerOp::Resume);
                        }
                        self.close_panel();
                    }
                    _ => self.close_panel(),
                }
            }
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
            Action::TimerBindMove(delta) => {
                if let Some(Panel::Reclaim { selected }) = self.panel.as_mut() {
                    let next = (*selected as i32 + delta).clamp(0, RECLAIM_ROWS as i32 - 1);
                    *selected = next as usize;
                } else {
                    self.move_selection(delta);
                }
            }
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
                        spawn_bind_then_stop(api, tx, target);
                    } else {
                        spawn_bind(api, tx, target);
                    }
                }
                None
            }
            Some(Panel::Start { mode, confirm }) => {
                let focus = *mode == PickerMode::Focus;
                // Second ⏎ on the conflict banner: stop & save, then start.
                if let Some(picked) = confirm.take() {
                    self.close_panel();
                    spawn_start_switch(api, tx, picked.id, focus);
                    return None;
                }
                let running = self.snapshot.as_ref().is_some_and(|s| s.running);
                match self.start_target() {
                    Some(StartTarget::Candidate(picked)) if running => {
                        // One timer at a time — surface the conflict banner.
                        if let Some(Panel::Start { confirm, .. }) = self.panel.as_mut() {
                            *confirm = Some(picked);
                        }
                    }
                    Some(StartTarget::Candidate(picked)) => {
                        self.close_panel();
                        spawn_start_bound(api, tx, picked.id, focus);
                    }
                    Some(StartTarget::Create(title)) => {
                        self.close_panel();
                        spawn_create_and_start(api, tx, title, focus);
                    }
                    Some(StartTarget::JustStart) => {
                        self.close_panel();
                        spawn_start_blank(api, tx, focus);
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
                    // The discard escape rides the normal discard flow —
                    // including its two-press confirm past the fence.
                    _ => {
                        let _ = tx.send(Action::TimerDiscard);
                    }
                }
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

    /// §Bind at stop: freeze the clock (pause, unless already paused) and open
    /// the bind picker with save-on-bind armed. Esc resumes what was frozen.
    fn open_bind_at_stop(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let already_paused = self.snapshot.as_ref().is_some_and(|s| s.paused);
        if !already_paused {
            spawn_op(api, tx, TimerOp::Pause);
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

    /// Extra synthetic rows after the candidates, by panel: the bind panel
    /// offers "create" when a title is typed; the start picker adds "create"
    /// and "just start" only while nothing runs (in switch context the list
    /// is candidates-only).
    fn extra_rows(&self) -> (bool, bool) {
        let has_query = !self.query.trim().is_empty();
        let running = self.snapshot.as_ref().is_some_and(|s| s.running);
        match self.panel {
            Some(Panel::Bind { .. }) => (has_query, false),
            Some(Panel::Start { .. }) if !running => (has_query, true),
            Some(Panel::Start { .. }) | Some(Panel::Reclaim { .. }) => (false, false),
            None => (false, false),
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
        let focus = snap.mode.as_deref() == Some("focus");
        let on_break = focus && snap.phase.as_deref() == Some("break");
        // 1-based: the interval being worked now. The round length is a
        // settings knob with no API, so no "of N" is claimed.
        let interval_now = snap.intervals_completed.unwrap_or(0) + 1;

        let idle = snap.idle == Some(true);
        let offer = self.current_offer();

        let mut lines: Vec<Line<'static>> = Vec::new();
        // State label above the number.
        lines.push(if idle {
            // Reclaim was deferred with Esc — the face says the guard is
            // still waiting (the list reopens on the next poll).
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

        // The big number — muted while not counting, amber past the plan,
        // accent while counting.
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

        let mut lines: Vec<Line<'static>> = Vec::new();

        // POMODORO — focus only: banked intervals green, the live one accent.
        // No empty remainder dots: the round length is a settings knob with no
        // API to read it from.
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

        // THIS WEEK — the mon→sun sparkline over the progress by_day series;
        // absent on servers without it (the block degrades to TODAY below).
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
        // The start picker overlays whichever stage it was opened from
        // (Absent, or Live in switch context).
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
                        Span::raw(" to dismiss, or "),
                        Span::styled("u", theme::focused()),
                        Span::raw(" to undo — deletes this segment."),
                    ]),
                ];
                frame.render_widget(
                    Paragraph::new(lines).block(bordered("Timer · stopped")),
                    area,
                );
            }
            Stage::Live if matches!(self.panel, Some(Panel::Bind { .. })) => {
                self.render_bind_panel(frame, area)
            }
            Stage::Live => self.render_watch_face(frame, area),
        }
    }

    /// The §Start-a-timer picker: mode toggle, live search, the synthetic
    /// create / just-start rows — and the §Start-conflict banner when a timer
    /// is already running.
    fn render_start_panel(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Start a timer");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (mode, confirm) = match self.panel.as_ref() {
            Some(Panel::Start { mode, confirm }) => (*mode, confirm.clone()),
            _ => return,
        };

        // The conflict banner replaces the list: one decision, two keys.
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

    /// §Idle reclaim: what was the idle tail worth? One row per server verb,
    /// captions computed from the read (`last_interacted_at` anchors the
    /// span). Nothing is written until ⏎.
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
            _ => {}
        }
        match &self.stage {
            Stage::Loading => widgets::footer_hints(&[("h", "home")]),
            Stage::Absent => widgets::footer_hints(&[("s", "start"), ("h", "home")]),
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

/// The picker's Focus choice: `start_timer` has no mode param, so a focus
/// start is start + mode switch. A refused hop keeps the stopwatch start and
/// says so.
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

fn spawn_start_blank(api: &ApiClient, tx: &UnboundedSender<Action>, focus: bool) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.start_timer(None, false).await {
            Ok(t) => {
                let t = into_mode(&api, &tx, t, focus).await;
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

/// Start bound to an existing activity (no switch — a running timer should
/// have routed through the conflict banner first; a racing 409 still surfaces).
fn spawn_start_bound(api: &ApiClient, tx: &UnboundedSender<Action>, activity_id: i64, focus: bool) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.start_timer(Some(activity_id), false).await {
            Ok(t) => {
                let t = into_mode(&api, &tx, t, focus).await;
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

/// The conflict banner's second ⏎: stop & save the running timer server-side,
/// then start the picked one.
fn spawn_start_switch(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    activity_id: i64,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.start_timer(Some(activity_id), true).await {
            Ok(t) => {
                let t = into_mode(&api, &tx, t, focus).await;
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("stop & switch failed: {e}"),
                });
            }
        }
    });
}

/// The "＋ new activity" row: a blank start followed by a bind-with-title —
/// the same call pair the bind panel uses, so the server mints the activity.
fn spawn_create_and_start(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    title: String,
    focus: bool,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Err(e) = api.start_timer(None, false).await {
            let _ = tx.send(Action::Notify {
                level: Level::Error,
                text: format!("start failed: {e}"),
            });
            return;
        }
        match api.bind_timer(None, Some(title)).await {
            Ok(t) => {
                let t = into_mode(&api, &tx, t, focus).await;
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                // The clock is running but unnamed — say so instead of hiding it.
                let _ = tx.send(Action::TimerReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Warning,
                    text: format!("started, but naming it failed: {e} — bind with `/`"),
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

/// §Bind at stop's ⏎: bind (existing or minted-from-title), then stop — the
/// server's bound-only save with the picker in between. The bind result is
/// forwarded first so the stop confirmation can name the activity.
fn spawn_bind_then_stop(api: &ApiClient, tx: &UnboundedSender<Action>, target: BindTarget) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let bound = match target {
            BindTarget::Existing(id) => api.bind_timer(Some(id), None).await,
            BindTarget::Create(title) => api.bind_timer(None, Some(title)).await,
        };
        match bound {
            Ok(t) => {
                let _ = tx.send(Action::TimerLoaded(Box::new(t)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("bind failed: {e}"),
                });
                return;
            }
        }
        match api.stop_timer().await {
            Ok(stopped) => {
                let _ = tx.send(Action::TimerStopped(Box::new(stopped)));
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

/// Apply a reclaim verb. Trim/keep resolve to a fresh running snapshot; stop
/// resolves to the written segment (the same confirmation + undo as a normal
/// stop).
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

/// Drive a focus phase transition; the fresh snapshot lands as TimerLoaded.
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

/// `n` mid-work: bank the interval (work → break credits it) and immediately
/// arm the next work phase — the skip is the pair, not a server verb.
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

/// Switch the running timer's mode in place (elapsed preserved).
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

/// The week's per-day minutes for the rail's sparkline — the current week's
/// progress read, reduced to `by_day`.
fn spawn_week(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Ok(progress) = api.get_progress(None).await {
            let _ = tx.send(Action::TimerWeekLoaded(progress.by_day));
        }
    });
}

/// The per-user knobs for this screen's copy and the reclaim default.
fn spawn_settings(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Ok(s) = api.timer_settings().await {
            let _ = tx.send(Action::SettingsLoaded(Box::new(s)));
        }
    });
}

/// `u` on the stop confirmation: delete the just-written segment — the exact
/// inverse of the save, while the confirmation still shows.
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
        // Submitting in focus mode starts for real now (start + mode hop) —
        // the picker closes like any accepted start.
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
