//! Progress screen — weekly targets rendered as `engineer pace` meters
//! (progress.html §F). Read-only: one meter row per target (behind-first), the
//! week header line, a behind-total footer, and a compact kind-mix line. Step
//! weeks with `[` / `]`; `t` returns to the current week.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{ToSpan, Zoned};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{
    ApiClient, Domain, PaceState, Progress as ProgressData, ProgressReading, TargetCreate,
    TargetScope,
};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::picker::{Picker, PickerItem};
use crate::ui::{layout::bordered, theme, widgets};

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

/// The `n`-to-declare flow: fetch domains, fuzzy-pick any scope, then hours.
enum Declare {
    /// Fetching domains before the scope picker can open.
    Loading,
    /// Fuzzy-picking the scope — any domain, kind, or intent — in one list.
    Scope(Picker<TargetScope>),
    /// Entering the weekly hours for the chosen scope.
    Hours {
        scope: TargetScope,
        label: String,
        buf: String,
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
        let week = week_param(self.offset);
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
            }
            Action::ProgressLoadFailed(e) => {
                self.loading = false;
                self.error = Some(e);
            }
            Action::ProgressWeekStep(delta) => {
                self.offset += delta;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::ProgressWeekReset => {
                if self.offset != 0 {
                    self.offset = 0;
                    self.loading = true;
                    self.fetch(api, tx);
                }
            }
            Action::RefreshProgress => {
                self.loading = true;
                self.fetch(api, tx);
            }
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
                        let api = api.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            match api.update_target(id, hours).await {
                                Ok(t) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Success,
                                        text: format!(
                                            "target → {}h/wk",
                                            fmt_hours(t.hours_per_week)
                                        ),
                                    });
                                    let _ = tx.send(Action::RefreshProgress);
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Error,
                                        text: format!("adjust failed: {e}"),
                                    });
                                }
                            }
                        });
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
                    let api = api.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        match api.retire_target(id).await {
                            Ok(_) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: "target retired — history kept".into(),
                                });
                                let _ = tx.send(Action::RefreshProgress);
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("retire failed: {e}"),
                                });
                            }
                        }
                    });
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
            _ => {}
        }
        None
    }

    /// Route a key while the declare flow is open. Uses take-then-replace so a
    /// stage transition can reassign `self.declare` without a borrow conflict.
    fn declare_key(&mut self, key: KeyEvent, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(mut state) = self.declare.take() else {
            return;
        };
        match &mut state {
            Declare::Loading => {
                if key.code == KeyCode::Esc {
                    return; // taken → cancelled
                }
            }
            Declare::Scope(picker) => match key.code {
                KeyCode::Esc => return,
                KeyCode::Enter => {
                    if let (Some(scope), Some(label)) =
                        (picker.selected().cloned(), picker.selected_label())
                    {
                        let label = label.to_string();
                        self.declare = Some(Declare::Hours {
                            scope,
                            label,
                            buf: String::new(),
                        });
                    }
                    return;
                }
                KeyCode::Backspace => picker.backspace(),
                KeyCode::Down => picker.move_cursor(1),
                KeyCode::Up => picker.move_cursor(-1),
                KeyCode::Char('n') if ctrl => picker.move_cursor(1),
                KeyCode::Char('p') if ctrl => picker.move_cursor(-1),
                KeyCode::Char(c) if !ctrl => picker.input(c),
                _ => {}
            },
            Declare::Hours { scope, buf, .. } => match key.code {
                KeyCode::Esc => return,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => buf.push(c),
                KeyCode::Enter => match buf.trim().parse::<f64>() {
                    Ok(hours) if hours > 0.0 => {
                        let create = TargetCreate {
                            scope: scope.clone(),
                            hours_per_week: hours,
                        };
                        let api = api.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            match api.create_target(&create).await {
                                Ok(t) => {
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
                                Err(e) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Error,
                                        text: format!("declare failed: {e}"),
                                    });
                                }
                            }
                        });
                        return; // done → closed
                    }
                    // Invalid hours: keep the prompt open (fall through, put back).
                    _ => {}
                },
                _ => {}
            },
        }
        // Stages that continue (picker filtering, hours typing) keep the state.
        self.declare = Some(state);
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
            lines.push(Line::from(Span::styled(
                "No targets yet — declare one with `engineer target declare` (e.g. --kind coding --hours 4).",
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

        lines.push(Line::from(""));
        lines.push(behind_footer(data));

        if !data.kind_mix.is_empty() {
            lines.push(kind_mix_line(data));
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
                Span::from(format!("{label}  →  {buf}█ h/wk   (⏎ save · Esc cancel)")),
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
                    "enter weekly hours · ⏎ declare · Esc cancel",
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
            ("[", "prev wk"),
            ("]", "next wk"),
            ("t", "this wk"),
            ("a", "audit"),
            ("h", "home"),
        ])
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
    let label = pad_or_truncate(&name, label_w);
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

/// `kind mix  coding 3.0h · reading 2.5h` — the week's time-by-kind split.
fn kind_mix_line(data: &ProgressData) -> Line<'static> {
    let parts: Vec<String> = data
        .kind_mix
        .iter()
        .map(|k| format!("{} {:.1}h", k.kind, k.minutes as f64 / 60.0))
        .collect();
    Line::from(Span::styled(
        format!("kind mix  {}", parts.join(" · ")),
        theme::muted(),
    ))
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

/// The ISO week id (`YYYY-Www`) `offset` weeks from the current week, or `None`
/// for the current week so the server picks its own default.
fn week_param(offset: i32) -> Option<String> {
    if offset == 0 {
        return None;
    }
    let target = Zoned::now()
        .date()
        .checked_add((offset as i64 * 7).days())
        .ok()?;
    let iso = target.iso_week_date();
    Some(format!("{:04}-W{:02}", iso.year(), iso.week()))
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ctx() -> (ApiClient, UnboundedSender<Action>) {
        let api =
            ApiClient::with_token(url::Url::parse("http://127.0.0.1:9/").unwrap(), "t".into());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        (api, tx)
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
    async fn declare_scope_pick_moves_to_hours_then_esc_cancels() {
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
            Some(Declare::Hours { scope, label, buf }) => {
                assert!(matches!(scope, TargetScope::Domain(7)));
                assert!(label.contains("Systems"), "{label}");
                assert!(buf.is_empty());
            }
            other => panic!("expected Hours, got {:?}", other.is_some()),
        }
        // Type hours, then Esc cancels the whole flow.
        p.handle(
            Action::ProgressDeclareKey(key(KeyCode::Char('6'))),
            &api,
            &tx,
        )
        .await;
        assert!(matches!(&p.declare, Some(Declare::Hours { buf, .. }) if buf == "6"));
        p.handle(Action::ProgressDeclareKey(key(KeyCode::Esc)), &api, &tx)
            .await;
        assert!(p.declare.is_none());
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
        assert!(week_param(0).is_none());
        assert!(week_param(-1).unwrap().contains("-W"));
    }

    fn spans_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }
}
