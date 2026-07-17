//! Progress screen — weekly targets rendered as `engineer pace` meters
//! (progress.html §F). Read-only: one meter row per target (behind-first), the
//! week header line, a behind-total footer, and the "where it went" fold — a
//! muted glance `Tab`-cycles through by kind → by domain → by intent (§Where it
//! went). Step weeks with `[` / `]`; `t` returns to the current week.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::ToSpan;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{
    codes, ApiClient, ApiError, Domain, PaceState, Progress as ProgressData, ProgressReading,
    TargetCreate, TargetScope,
};
use crate::app::action::Action;
use crate::queue::WriteOutcome;
use crate::ui::notify::Level;
use crate::ui::picker::{Picker, PickerItem};
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

/// The shared "the server moved on — re-read" copy for a target write that hits
/// a `target-version-closed` conflict live (ADR 0026): a soft re-fetch, never a
/// hard error. The Inbox screen speaks the same idiom for a stale draft.
const TARGET_MOVED_ON: &str = "this target moved on — re-fetching the live pace";

/// Meter bar width in cells (matches the design mock's ten-block bar).
const BAR_WIDTH: usize = 10;

/// The activity kinds and intents a target can scope to — mirrors engineer's
/// `Activity.kinds` / `Activity.intents` enums (Target reuses them). Domains are
/// fetched; these are fixed, so the declare picker offers them without a call.
const KINDS: &[&str] = &[
    "deep_work",
    "reading",
    "coding",
    "lecture",
    "review",
    "pairing",
    "other",
];
const INTENTS: &[&str] = &["implement", "challenge", "follow", "study"];

/// The "where it went" fold's facet (§Where it went) — the shipped kind-mix line
/// grown into a `Tab`-cycled glance. Only `Kind` has data in the pace read; the
/// server carries no by-domain / by-intent rollup, so those two render as absent
/// (the backend-gap rule — no client-derived second ledger; see #122).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Facet {
    #[default]
    Kind,
    Domain,
    Intent,
}

impl Facet {
    /// Advance the fold one step: kind → domain → intent → kind.
    fn next(self) -> Self {
        match self {
            Self::Kind => Self::Domain,
            Self::Domain => Self::Intent,
            Self::Intent => Self::Kind,
        }
    }

    /// The active-facet label, mirroring the panel's axis tabs.
    fn label(self) -> &'static str {
        match self {
            Self::Kind => "by kind",
            Self::Domain => "by domain",
            Self::Intent => "by intent",
        }
    }
}

/// The `n`-to-declare flow: fetch domains, fuzzy-pick any scope, then hours.
enum Declare {
    /// Fetching domains before the scope picker can open.
    Loading,
    /// Fuzzy-picking the scope — any domain, kind, or intent — in one list.
    Scope(Picker<TargetScope>),
    /// Entering the weekly hours for the chosen scope. Keeps the scope picker so
    /// `Esc` steps *back* to it — query and all — instead of cancelling the whole
    /// flow (§Declare · hours: "⏎ declare · Esc back").
    Hours {
        scope: TargetScope,
        label: String,
        buf: String,
        picker: Picker<TargetScope>,
    },
}

#[derive(Default)]
pub struct Progress {
    data: Option<ProgressData>,
    /// Weeks relative to the current week: 0 = this week, -1 = last week.
    offset: i32,
    loading: bool,
    error: Option<String>,
    /// Cursor over `data.targets` — the row `e` (adjust) / `x` (retire) act on.
    selected: usize,
    /// `Some` while the inline hours editor is open for the selected target.
    edit: Option<String>,
    /// The target id armed for retire; a second `x` on the same row confirms.
    retire_armed: Option<i64>,
    /// `Some` while the `n`-declare flow (scope pick → hours) is open.
    declare: Option<Declare>,
    /// Which facet the "where it went" fold shows, cycled by `Tab`. Derived
    /// presentation only — the rollup is recomputed from the read, never stored.
    fold: Facet,
    /// Targets declared offline this session — rendered as provisional `◔ …
    /// queued` lines under the meters until the create replays and a live
    /// refetch returns the real reading. Only the render of what's pending
    /// (the queue is the ledger); cleared on any authoritative reload or week step.
    provisional: Vec<String>,
    /// Queue + read-cache paths for the write seam (`None` = shared XDG; tests
    /// inject a scratch dir so a spawned write never touches the real queue).
    queue_paths: QueuePaths,
}

impl Progress {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    /// While the inline hours editor is open it owns every relevant key, so a
    /// digit edits the buffer rather than firing the global keymap.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        // The declare flow (scope picker / hours input) is modal — while open it
        // owns every key so a typed letter filters rather than firing the keymap.
        if self.declare.is_some() {
            return Some(Action::ProgressDeclareKey(key));
        }
        // The inline hours editor owns digits/./Enter/Esc while open.
        self.edit.as_ref()?;
        match key.code {
            KeyCode::Esc => Some(Action::ProgressAdjustCancel),
            KeyCode::Enter => Some(Action::ProgressAdjustSubmit),
            KeyCode::Backspace => Some(Action::ProgressAdjustBackspace),
            KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                Some(Action::ProgressAdjustInput(c))
            }
            _ => None,
        }
    }

    /// Build the one-list scope picker: every domain, then the kind and intent
    /// enums — each labeled by axis, valued as the `TargetScope` to create.
    fn scope_picker(domains: &[Domain]) -> Picker<TargetScope> {
        let mut items = Vec::new();
        for d in domains {
            items.push(PickerItem::new(
                format!("domain · {}", d.name),
                TargetScope::Domain(d.id),
            ));
        }
        for k in KINDS {
            items.push(PickerItem::new(
                format!("kind · {k}"),
                TargetScope::Kind((*k).to_string()),
            ));
        }
        for i in INTENTS {
            items.push(PickerItem::new(
                format!("intent · {i}"),
                TargetScope::Intent((*i).to_string()),
            ));
        }
        Picker::new("declare a target — pick a scope", items)
    }

    fn selected_target(&self) -> Option<&ProgressReading> {
        self.data.as_ref()?.targets.get(self.selected)
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let api = api.clone();
        let tx = tx.clone();
        let week = super::week_param(self.offset);
        tokio::spawn(async move {
            match api.get_progress(week.as_deref()).await {
                Ok(progress) => {
                    let _ = tx.send(Action::ProgressLoaded(Box::new(progress)));
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("progress load failed: {e}"),
                    });
                    let _ = tx.send(Action::ProgressLoadFailed(e.to_string()));
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
            Action::ProgressLoaded(progress) => {
                self.data = Some(*progress);
                self.loading = false;
                self.error = None;
                // Keep the cursor in range as the target set changes week to week.
                let n = self.data.as_ref().map_or(0, |d| d.targets.len());
                self.selected = self.selected.min(n.saturating_sub(1));
                self.retire_armed = None;
                // An authoritative reading supersedes any queued-declare stand-ins.
                self.provisional.clear();
            }
            Action::ProgressLoadFailed(e) => {
                self.loading = false;
                self.error = Some(e);
            }
            Action::ProgressWeekStep(delta) => {
                self.offset += delta;
                self.loading = true;
                // A different week's readings are coming — drop this week's queued
                // stand-ins so they never bleed across the step.
                self.provisional.clear();
                self.fetch(api, tx);
            }
            Action::ProgressWeekReset => {
                if self.offset != 0 {
                    self.offset = 0;
                    self.loading = true;
                    self.provisional.clear();
                    self.fetch(api, tx);
                }
            }
            Action::RefreshProgress => {
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::ProgressFoldCycle => self.fold = self.fold.next(),
            Action::ProgressSelectMove(delta) => {
                if let Some(data) = &self.data {
                    let n = data.targets.len() as i32;
                    if n > 0 {
                        self.selected = (self.selected as i32 + delta).clamp(0, n - 1) as usize;
                    }
                }
                self.retire_armed = None;
            }
            Action::ProgressAdjustBegin => {
                // Prefill with the current hours so the edit starts from the truth.
                if let Some(r) = self.selected_target() {
                    self.edit = Some(fmt_hours(r.target.hours_per_week));
                }
                self.retire_armed = None;
            }
            Action::ProgressAdjustInput(c) => {
                if let Some(b) = self.edit.as_mut() {
                    b.push(c);
                }
            }
            Action::ProgressAdjustBackspace => {
                if let Some(b) = self.edit.as_mut() {
                    b.pop();
                }
            }
            Action::ProgressAdjustCancel => self.edit = None,
            Action::ProgressAdjustSubmit => {
                let parsed = self
                    .edit
                    .as_deref()
                    .and_then(|b| b.trim().parse::<f64>().ok());
                let id = self.selected_target().map(|r| r.target.id);
                self.edit = None;
                match (id, parsed) {
                    (Some(id), Some(hours)) if hours > 0.0 => {
                        spawn_adjust_target(api, tx, self.queue_paths.clone(), id, hours);
                    }
                    (Some(_), _) => {
                        return Some((Level::Warning, "enter a positive number of hours".into()))
                    }
                    _ => {}
                }
            }
            Action::ProgressRetire => {
                let id = self.selected_target().map(|r| r.target.id)?;
                if self.retire_armed == Some(id) {
                    // Second press on the same row — confirm.
                    self.retire_armed = None;
                    spawn_retire_target(api, tx, self.queue_paths.clone(), id);
                } else {
                    self.retire_armed = Some(id);
                    return Some((
                        Level::Warning,
                        "press x again to retire this target (history is kept)".into(),
                    ));
                }
            }
            Action::ProgressDeclareBegin => {
                if self.declare.is_none() && self.edit.is_none() {
                    self.declare = Some(Declare::Loading);
                    self.retire_armed = None;
                    let api = api.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        // Domains failing shouldn't block declaring a kind/intent
                        // target — fall back to an empty domain list.
                        let domains = api.list_domains().await.unwrap_or_default();
                        let _ = tx.send(Action::ProgressDeclareReady(domains));
                    });
                }
            }
            Action::ProgressDeclareReady(domains) => {
                // Only open the picker if the user is still in the flow.
                if matches!(self.declare, Some(Declare::Loading)) {
                    self.declare = Some(Declare::Scope(Self::scope_picker(&domains)));
                }
            }
            Action::ProgressDeclareKey(key) => self.declare_key(key, api, tx),
            Action::ProgressDeclareQueued(desc) => self.provisional.push(desc),
            _ => {}
        }
        None
    }

    /// Route a key while the declare flow is open. Matches the taken state by
    /// value so a stage transition can move the picker into (and back out of)
    /// the hours step; every continuing arm puts the state back.
    fn declare_key(&mut self, key: KeyEvent, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(state) = self.declare.take() else {
            return;
        };
        match state {
            Declare::Loading => {
                if key.code != KeyCode::Esc {
                    self.declare = Some(Declare::Loading); // Esc: taken → cancelled
                }
            }
            Declare::Scope(mut picker) => match key.code {
                KeyCode::Esc => {} // taken → cancelled
                KeyCode::Enter => {
                    let choice = picker
                        .selected()
                        .cloned()
                        .zip(picker.selected_label().map(str::to_string));
                    // The picker rides into the hours step so Esc can walk back.
                    self.declare = Some(match choice {
                        Some((scope, label)) => Declare::Hours {
                            scope,
                            label,
                            buf: String::new(),
                            picker,
                        },
                        // Everything filtered out — nothing to pick; stay put.
                        None => Declare::Scope(picker),
                    });
                }
                code => {
                    match code {
                        KeyCode::Backspace => picker.backspace(),
                        KeyCode::Down => picker.move_cursor(1),
                        KeyCode::Up => picker.move_cursor(-1),
                        KeyCode::Char('n') if ctrl => picker.move_cursor(1),
                        KeyCode::Char('p') if ctrl => picker.move_cursor(-1),
                        KeyCode::Char(c) if !ctrl => picker.input(c),
                        _ => {}
                    }
                    self.declare = Some(Declare::Scope(picker));
                }
            },
            Declare::Hours {
                scope,
                label,
                mut buf,
                picker,
            } => match key.code {
                // Back to the scope picker, query and cursor intact — the
                // design's hours footer is "⏎ declare · Esc back".
                KeyCode::Esc => self.declare = Some(Declare::Scope(picker)),
                KeyCode::Enter if matches!(buf.trim().parse::<f64>(), Ok(h) if h > 0.0) => {
                    let hours = buf.trim().parse::<f64>().expect("guard just parsed it");
                    let create = TargetCreate {
                        scope,
                        hours_per_week: hours,
                    };
                    // The picker label is `axis · scope`; the queued line
                    // shows the scope + hours, lowercased like the meters.
                    let scope_name = label.rsplit(" · ").next().unwrap_or(&label).to_lowercase();
                    let desc = format!("{scope_name} · {}h/wk", fmt_hours(hours));
                    spawn_declare_target(api, tx, self.queue_paths.clone(), create, desc);
                    // done → closed
                }
                code => {
                    match code {
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => buf.push(c),
                        // Invalid hours on Enter: keep the prompt open.
                        _ => {}
                    }
                    self.declare = Some(Declare::Hours {
                        scope,
                        label,
                        buf,
                        picker,
                    });
                }
            },
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Progress · engineer pace");

        let Some(data) = &self.data else {
            let body = if let Some(err) = &self.error {
                Paragraph::new(Line::from(Span::styled(
                    format!("could not load progress: {err}"),
                    Style::default().fg(theme::DANGER),
                )))
            } else {
                Paragraph::new("loading…")
            };
            frame.render_widget(body.block(block), area);
            return;
        };

        let mut lines: Vec<Line> = Vec::new();
        lines.push(week_header(data));
        lines.push(Line::from(""));

        if data.targets.is_empty() {
            // The teaching empty state points at the keystroke first, the honest
            // headless verb second (§Progress · empty; the design drew `d`, the
            // shipped binding is `n` — the "new" mnemonic the footer advertises).
            lines.push(Line::from(Span::styled(
                "No weekly targets yet.",
                theme::muted(),
            )));
            lines.push(Line::from(vec![
                Span::styled("Press ", theme::muted()),
                Span::styled("n", Style::default().fg(theme::ACCENT)),
                Span::styled(
                    " to declare a weekly intent — a promise you keep from the terminal.",
                    theme::muted(),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "or headless: engineer target declare systems --hours 6",
                theme::muted(),
            )));
        } else {
            let label_w = data
                .targets
                .iter()
                .map(|r| r.target.scope.name().chars().count())
                .max()
                .unwrap_or(6)
                .clamp(6, 20);
            for (i, reading) in data.targets.iter().enumerate() {
                let is_sel = i == self.selected;
                lines.push(meter_line(reading, label_w, is_sel));
                if is_sel {
                    if let Some(buf) = &self.edit {
                        lines.push(edit_line(reading, buf));
                    } else if self.retire_armed == Some(reading.target.id) {
                        lines.push(Line::from(Span::styled(
                            "  retire this target? press x again — history is kept",
                            Style::default().fg(theme::WARN),
                        )));
                    }
                }
            }
        }

        // Offline declares this session — a `◔ … queued` stand-in per target,
        // until the create replays and a live refetch returns the real reading.
        for desc in &self.provisional {
            lines.push(Line::from(vec![
                Span::styled("  ◔ ", Style::default().fg(theme::ACCENT)),
                Span::raw(desc.clone()),
                Span::styled("  queued", theme::muted()),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(behind_footer(data));

        // The "where it went" fold sits with the meters: shown whenever the week
        // has targets or logged time, cycled by `Tab`. A truly empty week keeps
        // its teaching state uncluttered.
        if !data.targets.is_empty() || !data.kind_mix.is_empty() {
            lines.push(fold_line(data, self.fold));
        }
        if data.totals.thin {
            lines.push(Line::from(Span::styled(
                "week is thin (< 3 activities) — too sparse to read a trend",
                theme::muted(),
            )));
        }

        frame.render_widget(Paragraph::new(lines).block(block), area);

        // The declare flow draws over the meters when open.
        match &self.declare {
            Some(Declare::Loading) => declare_overlay(
                frame,
                area,
                "declare a target",
                Span::styled("loading domains…", theme::muted()),
            ),
            Some(Declare::Scope(picker)) => picker.render(frame, area),
            Some(Declare::Hours { label, buf, .. }) => declare_overlay(
                frame,
                area,
                "declare a target — hours",
                Span::from(format!("{label}  →  {buf}█ h/wk   (⏎ declare · Esc back)")),
            ),
            None => {}
        }
    }

    pub fn hints(&self) -> Line<'static> {
        if let Some(d) = &self.declare {
            return match d {
                Declare::Scope(_) => Line::from(Span::styled(
                    "type to filter · ↑/↓ or ^n/^p move · ⏎ pick · Esc cancel",
                    theme::muted(),
                )),
                _ => Line::from(Span::styled(
                    "enter weekly hours · ⏎ declare · Esc back",
                    theme::muted(),
                )),
            };
        }
        if self.edit.is_some() {
            return widgets::footer_hints(&[("⏎", "save"), ("Esc", "cancel")]);
        }
        widgets::footer_hints(&[
            ("j/k", "select"),
            ("n", "new"),
            ("e", "adjust"),
            ("x", "retire"),
            ("⇥", "where it went"),
            ("[", "prev wk"),
            ("]", "next wk"),
            ("t", "this wk"),
            ("a", "audit"),
            ("h", "home"),
        ])
    }
}

/// Declare a weekly target through the queue seam (`n` → scope → hours). A
/// confirmed create refetches the pace (the server now has the row); an offline
/// one queues and the screen renders `desc` as a `◔ … queued` line until it
/// replays.
fn spawn_declare_target(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    create: TargetCreate,
    desc: String,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "declare failed", e),
        };
        match queued.create_target(&create).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!(
                        "declared {} · {}h/wk",
                        t.scope.name(),
                        fmt_hours(t.hours_per_week)
                    ),
                });
                let _ = tx.send(Action::RefreshProgress);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::ProgressDeclareQueued(desc));
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "declared · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_target_write_error(&tx, "declare failed", e),
        }
    });
}

/// Adjust the selected target's weekly hours through the queue seam (`e`). A
/// confirmed adjust refetches — the returned LIVE row's id may differ (a same-day
/// edit mints a successor), so the screen re-reads rather than patch in place;
/// an offline adjust queues.
fn spawn_adjust_target(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    id: i64,
    hours: f64,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "adjust failed", e),
        };
        match queued.adjust_target(id, hours).await {
            Ok(WriteOutcome::Confirmed(t)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("target → {}h/wk", fmt_hours(t.hours_per_week)),
                });
                let _ = tx.send(Action::RefreshProgress);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "adjusted · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_target_write_error(&tx, "adjust failed", e),
        }
    });
}

/// Retire the selected target through the queue seam (`x`, second press).
/// Confirmed refetches; offline queues. Retire closes the lineage — history is
/// kept, never deleted.
fn spawn_retire_target(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths, id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "retire failed", e),
        };
        match queued.retire_target(id).await {
            Ok(WriteOutcome::Confirmed(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: "target retired — history kept".into(),
                });
                let _ = tx.send(Action::RefreshProgress);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "retired · queued (offline) — will sync".into(),
                });
            }
            Err(e) => notify_target_write_error(&tx, "retire failed", e),
        }
    });
}

/// Route a live target-write failure: a `target-version-closed` conflict (ADR
/// 0026 — the version you addressed was superseded) is not an error but a soft
/// re-read — render the shared "moved on" copy and refetch, so the live lineage's
/// current pace comes back (the user re-adjusts the fresh row). Every other
/// failure is the loud seam error.
fn notify_target_write_error(tx: &UnboundedSender<Action>, context: &str, e: ApiError) {
    if e.code() == Some(codes::TARGET_VERSION_CLOSED) {
        let _ = tx.send(Action::Notify {
            level: Level::Warning,
            text: TARGET_MOVED_ON.into(),
        });
        let _ = tx.send(Action::RefreshProgress);
    } else {
        notify_seam_error(tx, context, e);
    }
}

/// A small centered box for the declare flow's non-picker stages (loading, hours).
fn declare_overlay(frame: &mut Frame, area: Rect, title: &str, body: Span<'static>) {
    let width = area.width.saturating_sub(6).clamp(24, 64);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height / 2,
        width,
        height: 3,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::from(body)).block(bordered(title.to_string())),
        rect,
    );
}

/// `2026-W27 · sat · day 5 of 7 · now = 57%` — the week frame and now-tick.
fn week_header(data: &ProgressData) -> Line<'static> {
    let week = &data.week;
    // The current day being lived; clamp so a closed week reads "day 7 of 7".
    let day_offset = week.elapsed_days.min(6);
    let weekday = week
        .monday
        .checked_add((day_offset as i64).days())
        .map(|d| d.strftime("%a").to_string().to_lowercase())
        .unwrap_or_default();
    let pct = (week.now_fraction * 100.0).round() as i64;
    Line::from(vec![
        Span::styled(week.id.clone(), theme::header()),
        Span::styled(
            format!(" · {weekday} · day {} of 7 · now = {pct}%", day_offset + 1),
            theme::muted(),
        ),
    ])
}

/// One meter row: `▌ systems     █████·╎···  2.2/6h   -2.1h behind`. A `▌`
/// marker (accent) flags the selected row that `e`/`x` act on.
fn meter_line(reading: &ProgressReading, label_w: usize, selected: bool) -> Line<'static> {
    let name = reading.target.scope.name().to_lowercase();
    let label = super::pad_or_truncate(&name, label_w);
    let color = state_color(reading.state);

    let marker = if selected { "▌ " } else { "  " };
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(marker.to_string(), Style::default().fg(theme::ACCENT)),
        Span::raw(format!("{label}  ")),
    ];
    spans.extend(widgets::pace_bar(
        reading.progress_fraction(),
        // The now-tick marks where the week expects you to be (expected/target).
        // Skipped on met rows, whose bar is already full.
        reading.now_tick_fraction(),
        BAR_WIDTH,
        color,
        reading.state != PaceState::Met,
    ));

    let nums = format!(
        "{:.1}/{}h",
        reading.actual_hours(),
        fmt_hours(reading.target.hours_per_week)
    );
    spans.push(Span::raw(format!("  {nums:<8}  ")));

    match reading.state {
        PaceState::Met => spans.push(Span::styled("met", theme::muted())),
        _ => spans.push(Span::styled(
            format!("{:+.1}h {}", reading.delta_hours(), reading.state.word()),
            Style::default().fg(color),
        )),
    }
    Line::from(spans)
}

/// The inline hours editor shown under the selected row while adjusting:
/// `  adjust systems → 6█ h/wk  (⏎ save · Esc cancel)`.
fn edit_line(reading: &ProgressReading, buf: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  adjust ".to_string(), theme::muted()),
        Span::raw(reading.target.scope.name().to_lowercase()),
        Span::styled(" → ".to_string(), theme::muted()),
        Span::styled(format!("{buf}█"), Style::default().fg(theme::ACCENT)),
        Span::styled(" h/wk  (⏎ save · Esc cancel)".to_string(), theme::muted()),
    ])
}

/// `behind 3.3h total · largest gap "systems"` — or a quiet on-pace confirmation.
fn behind_footer(data: &ProgressData) -> Line<'static> {
    let behind: Vec<&ProgressReading> = data
        .targets
        .iter()
        .filter(|r| r.state == PaceState::Behind)
        .collect();

    if behind.is_empty() {
        return Line::from(Span::styled(
            "all targets on pace ✓",
            Style::default().fg(theme::SUCCESS),
        ));
    }

    let total: f64 = behind.iter().map(|r| r.delta_hours().abs()).sum();
    // Readings arrive largest-gap-first, so the first behind row is the worst.
    let worst = behind[0].target.scope.name().to_lowercase();
    Line::from(Span::styled(
        format!("behind {total:.1}h total · largest gap \"{worst}\""),
        Style::default().fg(theme::WARN),
    ))
}

/// The "where it went" fold (§Where it went): one muted line whose active facet
/// (`Tab`-cycled) reads `where it went · by kind   coding 3.0h · reading 2.5h`.
/// Only the kind facet has data in the pace read — the server carries no
/// by-domain / by-intent rollup, so those render as an honest absent note rather
/// than a client-derived second ledger (the backend-gap rule; #122).
fn fold_line(data: &ProgressData, facet: Facet) -> Line<'static> {
    let mut spans = vec![
        Span::styled("where it went · ", theme::muted()),
        Span::styled(facet.label(), Style::default().fg(theme::ACCENT)),
        Span::raw("  "),
    ];
    let body: Span<'static> = match facet {
        Facet::Kind if data.kind_mix.is_empty() => {
            Span::styled("nothing logged yet", theme::muted())
        }
        Facet::Kind => {
            let parts: Vec<String> = data
                .kind_mix
                .iter()
                .map(|k| format!("{} {:.1}h", k.kind, k.minutes as f64 / 60.0))
                .collect();
            Span::styled(parts.join(" · "), theme::muted())
        }
        Facet::Domain => Span::styled(
            "the pace read doesn't roll up by domain yet",
            theme::muted(),
        ),
        Facet::Intent => Span::styled(
            "the pace read doesn't roll up by intent yet",
            theme::muted(),
        ),
    };
    spans.push(body);
    Line::from(spans)
}

fn state_color(state: PaceState) -> Color {
    match state {
        PaceState::Behind => theme::WARN,
        PaceState::OnPace => theme::SUCCESS,
        PaceState::Met => theme::ACCENT,
    }
}

/// Format target hours without a trailing `.0`: `6h`, but `2.5h` when fractional.
fn fmt_hours(hours: f64) -> String {
    if (hours.fract()).abs() < 1e-9 {
        format!("{hours:.0}")
    } else {
        format!("{hours:.1}")
    }
}

impl ProgressReading {
    /// Where the now-tick sits on the bar: the week's elapsed fraction, derived
    /// per-reading as `expected / target` (equal to the week's `now_fraction`).
    fn now_tick_fraction(&self) -> f64 {
        let target_minutes = self.hours_per_week * 60.0;
        if target_minutes <= 0.0 {
            return 0.0;
        }
        (self.expected_minutes as f64 / target_minutes).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProgressData {
        serde_json::from_value(serde_json::json!({
            "week": {
                "id": "2026-W27", "monday": "2026-06-29", "sunday": "2026-07-05",
                "elapsed_days": 4, "now_fraction": 0.5714
            },
            "targets": [
                {
                    "target": {
                        "id": 42, "axis": "domain",
                        "scope": { "axis": "domain", "value": 7, "domain": { "id": 7, "name": "Distributed Systems" } },
                        "hours_per_week": 6.0, "active": true, "retired": false
                    },
                    "hours_per_week": 6.0, "actual_minutes": 132, "expected_minutes": 257,
                    "delta_minutes": -125, "state": "behind"
                },
                {
                    "target": {
                        "id": 51, "axis": "kind",
                        "scope": { "axis": "kind", "value": "coding" },
                        "hours_per_week": 2.0, "active": true, "retired": false
                    },
                    "hours_per_week": 2.0, "actual_minutes": 120, "expected_minutes": 86,
                    "delta_minutes": 34, "state": "met"
                }
            ],
            "kind_mix": [ { "kind": "coding", "minutes": 120 } ],
            "bloom": [],
            "totals": { "actual_minutes": 252, "activity_count": 5, "thin": false }
        }))
        .unwrap()
    }

    #[test]
    fn week_header_shows_id_weekday_and_now_pct() {
        let text = spans_text(&week_header(&sample()));
        // 2026-06-29 is a Monday; elapsed_days=4 lands on Friday, day 5 of 7.
        assert!(text.contains("2026-W27"), "{text}");
        assert!(text.contains("fri"), "{text}");
        assert!(text.contains("day 5 of 7"), "{text}");
        assert!(text.contains("now = 57%"), "{text}");
    }

    #[test]
    fn behind_footer_sums_gaps_and_names_worst() {
        let text = spans_text(&behind_footer(&sample()));
        // Only the domain target is behind: |−125min| ≈ 2.1h.
        assert!(text.contains("behind 2.1h total"), "{text}");
        assert!(text.contains("distributed systems"), "{text}");
    }

    #[test]
    fn behind_footer_quiet_when_all_on_pace() {
        let mut data = sample();
        data.targets.retain(|r| r.state != PaceState::Behind);
        assert!(spans_text(&behind_footer(&data)).contains("on pace"));
    }

    #[test]
    fn meter_line_renders_nums_and_state_word() {
        let data = sample();
        let behind = spans_text(&meter_line(&data.targets[0], 18, false));
        assert!(behind.contains("distributed sys"), "{behind}");
        assert!(behind.contains("2.2/6h"), "{behind}");
        assert!(behind.contains("behind"), "{behind}");

        let met = spans_text(&meter_line(&data.targets[1], 18, false));
        assert!(met.contains("2.0/2h"), "{met}");
        assert!(met.contains("met"), "{met}");
    }

    #[test]
    fn meter_line_marks_the_selected_row() {
        let data = sample();
        assert!(spans_text(&meter_line(&data.targets[0], 18, true)).starts_with('▌'));
        assert!(!spans_text(&meter_line(&data.targets[0], 18, false)).starts_with('▌'));
    }

    #[tokio::test]
    async fn fold_cycles_kind_domain_intent_and_wraps() {
        let (api, tx) = ctx();
        let mut p = loaded();
        assert_eq!(p.fold, Facet::Kind, "the fold opens on kind");
        p.handle(Action::ProgressFoldCycle, &api, &tx).await;
        assert_eq!(p.fold, Facet::Domain);
        p.handle(Action::ProgressFoldCycle, &api, &tx).await;
        assert_eq!(p.fold, Facet::Intent);
        p.handle(Action::ProgressFoldCycle, &api, &tx).await;
        assert_eq!(p.fold, Facet::Kind, "the fold wraps back to kind");
    }

    #[test]
    fn fold_line_shows_kind_data_and_absent_domain_intent() {
        let data = sample();
        let kind = spans_text(&fold_line(&data, Facet::Kind));
        assert!(kind.contains("where it went · by kind"), "{kind}");
        assert!(kind.contains("coding 2.0h"), "{kind}");
        // The pace read carries no by-domain / by-intent rollup — the fold says
        // so rather than deriving a second ledger (the backend-gap rule).
        let domain = spans_text(&fold_line(&data, Facet::Domain));
        assert!(domain.contains("by domain"), "{domain}");
        assert!(domain.contains("doesn't roll up by domain"), "{domain}");
        let intent = spans_text(&fold_line(&data, Facet::Intent));
        assert!(intent.contains("by intent"), "{intent}");
        assert!(intent.contains("doesn't roll up by intent"), "{intent}");
    }

    #[test]
    fn fold_kind_facet_is_calm_when_nothing_logged() {
        let mut data = sample();
        data.kind_mix.clear();
        let text = spans_text(&fold_line(&data, Facet::Kind));
        assert!(text.contains("nothing logged yet"), "{text}");
    }

    #[test]
    fn fold_renders_on_screen_and_the_toggle_is_advertised() {
        let mut p = loaded();
        let text = render_text(&mut p);
        assert!(text.contains("where it went"), "the fold renders: {text}");
        // The footer advertises the toggle so it's discoverable (quiet, not loud).
        let hint = spans_text(&p.hints());
        assert!(
            hint.contains("where it went"),
            "footer advertises it: {hint}"
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ctx() -> (ApiClient, UnboundedSender<Action>) {
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:9/").unwrap(), "t".into());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        (api, tx)
    }

    /// A dead port refuses fast — the offline arm of the write seam.
    fn dead_api() -> ApiClient {
        ApiClient::with_token(url::Url::parse("http://127.0.0.1:1/").unwrap(), "t".into())
    }

    fn scratch_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-progress-screen-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("queue.json"), dir.join("timer-cache.json"))
    }

    fn render_text(p: &mut Progress) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(96, 24)).unwrap();
        terminal.draw(|f| p.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

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

    fn loaded() -> Progress {
        // two targets: id 42 (6h), id 51 (2h)
        Progress {
            data: Some(sample()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn select_move_clamps_within_targets() {
        let (api, tx) = ctx();
        let mut p = loaded();
        p.handle(Action::ProgressSelectMove(1), &api, &tx).await;
        assert_eq!(p.selected, 1);
        p.handle(Action::ProgressSelectMove(5), &api, &tx).await;
        assert_eq!(p.selected, 1, "clamped at the last row");
        p.handle(Action::ProgressSelectMove(-9), &api, &tx).await;
        assert_eq!(p.selected, 0, "clamped at the first row");
    }

    #[tokio::test]
    async fn adjust_prefills_current_hours_and_edits_buffer() {
        let (api, tx) = ctx();
        let mut p = loaded();
        p.handle(Action::ProgressAdjustBegin, &api, &tx).await;
        assert_eq!(
            p.edit.as_deref(),
            Some("6"),
            "prefilled from the current 6h"
        );
        p.handle(Action::ProgressAdjustBackspace, &api, &tx).await;
        p.handle(Action::ProgressAdjustInput('8'), &api, &tx).await;
        assert_eq!(p.edit.as_deref(), Some("8"));
        p.handle(Action::ProgressAdjustCancel, &api, &tx).await;
        assert!(p.edit.is_none());
    }

    #[tokio::test]
    async fn retire_arms_then_disarms_on_move() {
        let (api, tx) = ctx();
        let mut p = loaded();
        let note = p.handle(Action::ProgressRetire, &api, &tx).await;
        assert!(note.is_some(), "first press asks for confirmation");
        assert_eq!(p.retire_armed, Some(42));
        p.handle(Action::ProgressSelectMove(1), &api, &tx).await;
        assert_eq!(p.retire_armed, None, "moving the cursor disarms retire");
    }

    #[tokio::test]
    async fn declare_begin_loads_then_ready_opens_the_scope_picker() {
        let (api, tx) = ctx();
        let mut p = Progress::default();
        p.handle(Action::ProgressDeclareBegin, &api, &tx).await;
        assert!(matches!(p.declare, Some(Declare::Loading)));
        p.handle(
            Action::ProgressDeclareReady(vec![Domain {
                id: 7,
                name: "Systems".into(),
            }]),
            &api,
            &tx,
        )
        .await;
        assert!(matches!(p.declare, Some(Declare::Scope(_))));
    }

    #[tokio::test]
    async fn declare_scope_pick_moves_to_hours_then_esc_steps_back() {
        let (api, tx) = ctx();
        let mut p = Progress {
            declare: Some(Declare::Scope(Progress::scope_picker(&[Domain {
                id: 7,
                name: "Systems".into(),
            }]))),
            ..Default::default()
        };
        // Filter to the one domain, then pick it.
        for c in "sys".chars() {
            p.handle(Action::ProgressDeclareKey(key(KeyCode::Char(c))), &api, &tx)
                .await;
        }
        p.handle(Action::ProgressDeclareKey(key(KeyCode::Enter)), &api, &tx)
            .await;
        match &p.declare {
            Some(Declare::Hours {
                scope, label, buf, ..
            }) => {
                assert!(matches!(scope, TargetScope::Domain(7)));
                assert!(label.contains("Systems"), "{label}");
                assert!(buf.is_empty());
            }
            other => panic!("expected Hours, got {:?}", other.is_some()),
        }
        // Type hours, then Esc steps *back* to the scope picker (§Declare ·
        // hours: "Esc back"), filter intact — the same pick lands again.
        p.handle(
            Action::ProgressDeclareKey(key(KeyCode::Char('6'))),
            &api,
            &tx,
        )
        .await;
        assert!(matches!(&p.declare, Some(Declare::Hours { buf, .. }) if buf == "6"));
        p.handle(Action::ProgressDeclareKey(key(KeyCode::Esc)), &api, &tx)
            .await;
        assert!(
            matches!(&p.declare, Some(Declare::Scope(picker)) if picker.selected_label().is_some_and(|l| l.contains("Systems"))),
            "Esc from hours returns to the picker with the query kept"
        );
        // Esc again from the picker cancels the whole flow.
        p.handle(Action::ProgressDeclareKey(key(KeyCode::Esc)), &api, &tx)
            .await;
        assert!(p.declare.is_none());
    }

    #[test]
    fn empty_state_teaches_n_first_then_the_headless_verb() {
        let mut data = sample();
        data.targets.clear();
        let mut p = Progress {
            data: Some(data),
            ..Default::default()
        };
        let text = render_text(&mut p);
        assert!(text.contains("No weekly targets yet."), "{text}");
        assert!(
            text.contains("Press") && text.contains("declare a weekly intent"),
            "the keystroke is taught first: {text}"
        );
        assert!(
            text.contains("engineer target declare"),
            "the headless verb is taught second: {text}"
        );
    }

    #[tokio::test]
    async fn offline_declare_enqueues_and_renders_the_provisional_line() {
        // The full wiring, offline: the hours `⏎` → the declare helper →
        // `QueuedClient` → the persisted queue, plus the `◔ … queued` line the
        // screen renders until the create replays. A dead port forces offline.
        use crate::queue::{IntentKind, QueueStore};

        let (queue_path, cache_path) = scratch_paths();
        let api = dead_api();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut p = Progress {
            data: Some(sample()),
            declare: Some(Declare::Hours {
                scope: TargetScope::Kind("coding".into()),
                label: "kind · coding".into(),
                buf: String::new(),
                picker: Progress::scope_picker(&[]),
            }),
            queue_paths: Some((queue_path.clone(), cache_path)),
            ..Default::default()
        };

        p.handle(
            Action::ProgressDeclareKey(key(KeyCode::Char('4'))),
            &api,
            &tx,
        )
        .await;
        p.handle(Action::ProgressDeclareKey(key(KeyCode::Enter)), &api, &tx)
            .await;
        assert!(p.declare.is_none(), "the flow closed on submit");

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
        assert_eq!(pending[0].kind.word(), "declare");
        assert_eq!(pending[0].stream, "target");
        match &pending[0].kind {
            IntentKind::TargetCreate { body } => {
                assert_eq!(body.scope, TargetScope::Kind("coding".into()));
                assert!((body.hours_per_week - 4.0).abs() < 1e-9);
            }
            other => panic!("expected a TargetCreate intent, got {other:?}"),
        }

        // The spawn streamed the provisional line back; feed it and render it.
        assert!(
            wait_for(&mut rx, |a| {
                matches!(a, Action::ProgressDeclareQueued(d) if d == "coding · 4h/wk")
            })
            .await,
            "the queued declare streams a provisional line back"
        );
        p.handle(
            Action::ProgressDeclareQueued("coding · 4h/wk".into()),
            &api,
            &tx,
        )
        .await;
        let text = render_text(&mut p);
        assert!(text.contains("◔"), "the queued marker renders: {text}");
        assert!(text.contains("coding · 4h/wk"), "{text}");

        // An authoritative reload clears the stand-in.
        p.handle(Action::ProgressLoaded(Box::new(sample())), &api, &tx)
            .await;
        assert!(p.provisional.is_empty(), "a fresh reading supersedes it");
    }

    #[test]
    fn intercept_only_captures_keys_while_editing() {
        let mut p = Progress::default();
        // Not editing → keys fall through to the global keymap.
        assert!(p.intercept_key(key(KeyCode::Char('8'))).is_none());
        p.edit = Some(String::new());
        assert!(matches!(
            p.intercept_key(key(KeyCode::Char('8'))),
            Some(Action::ProgressAdjustInput('8'))
        ));
        assert!(matches!(
            p.intercept_key(key(KeyCode::Char('.'))),
            Some(Action::ProgressAdjustInput('.'))
        ));
        assert!(matches!(
            p.intercept_key(key(KeyCode::Enter)),
            Some(Action::ProgressAdjustSubmit)
        ));
        assert!(matches!(
            p.intercept_key(key(KeyCode::Esc)),
            Some(Action::ProgressAdjustCancel)
        ));
        // A non-hours character is not captured.
        assert!(p.intercept_key(key(KeyCode::Char('q'))).is_none());
    }

    #[test]
    fn week_param_none_for_current_week() {
        assert!(super::super::week_param(0).is_none());
        assert!(super::super::week_param(-1).unwrap().contains("-W"));
    }

    fn spans_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }
}
