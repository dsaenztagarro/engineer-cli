//! Progress screen — weekly targets rendered as `engineer pace` meters
//! (progress.html §F). Read-only: one meter row per target (behind-first), the
//! week header line, a behind-total footer, and a compact kind-mix line. Step
//! weeks with `[` / `]`; `t` returns to the current week.

use jiff::{ToSpan, Zoned};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, PaceState, Progress as ProgressData, ProgressReading};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Meter bar width in cells (matches the design mock's ten-block bar).
const BAR_WIDTH: usize = 10;

#[derive(Default)]
pub struct Progress {
    data: Option<ProgressData>,
    /// Weeks relative to the current week: 0 = this week, -1 = last week.
    offset: i32,
    loading: bool,
    error: Option<String>,
}

impl Progress {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
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
            _ => {}
        }
        None
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
                "No targets yet — declare a weekly intent in the web app.",
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
            for reading in &data.targets {
                lines.push(meter_line(reading, label_w));
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
    }

    pub fn hints(&self) -> Line<'static> {
        widgets::footer_hints(&[
            ("[", "prev wk"),
            ("]", "next wk"),
            ("t", "this wk"),
            ("r", "refresh"),
            ("h", "home"),
        ])
    }
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

/// One meter row: `systems     █████·╎···  2.2/6h   -2.1h behind`.
fn meter_line(reading: &ProgressReading, label_w: usize) -> Line<'static> {
    let name = reading.target.scope.name().to_lowercase();
    let label = pad_or_truncate(&name, label_w);
    let color = state_color(reading.state);

    let mut spans: Vec<Span<'static>> = vec![Span::raw(format!("{label}  "))];
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
        let behind = spans_text(&meter_line(&data.targets[0], 18));
        assert!(behind.contains("distributed sys"), "{behind}");
        assert!(behind.contains("2.2/6h"), "{behind}");
        assert!(behind.contains("behind"), "{behind}");

        let met = spans_text(&meter_line(&data.targets[1], 18));
        assert!(met.contains("2.0/2h"), "{met}");
        assert!(met.contains("met"), "{met}");
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
