//! Week board — the planned-vs-done readout for one ISO week
//! (week-planning.dc.html §Week · board). Read-only: one row per plan item with
//! a derived state pill (` done ` / ` live ` / ` hold ` / ` untouched `), a
//! logged-vs-planned meter, the summary line, and the retro band that reads the
//! stored week note. Step weeks with `[` / `]`; `t` returns to this week. The
//! TUI twin of the shipped `engineer week` readout — the plan and the actuals
//! stay one ledger (`GET /api/v1/weeks/:iso_week`), nothing here is stored.
//!
//! The plan-write gestures (declare/adjust/drop an intent, start the timer on a
//! planned item, the `$EDITOR` reflection) land in the follow-on tickets; this
//! screen ships the read plus the full-row `▌` cursor they steer.

use jiff::civil::Date;
use jiff::{ToSpan, Zoned};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, PlanItem, PlanState, Week as WeekData};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Meter bar width in cells (matches the Progress screen's ten-block bar).
const BAR_WIDTH: usize = 10;

#[derive(Default)]
pub struct Week {
    data: Option<WeekData>,
    /// Weeks relative to the current study week: 0 = this week, -1 = last week.
    offset: i32,
    loading: bool,
    error: Option<String>,
    /// Full-row `▌` cursor over the plan rows — the row the plan-write gestures
    /// (declare/start/drop) will act on once they land.
    selected: usize,
}

impl Week {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
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
                self.fetch(api, tx);
            }
            Action::WeekReset => {
                if self.offset != 0 {
                    self.offset = 0;
                    self.loading = true;
                    self.fetch(api, tx);
                }
            }
            Action::RefreshWeek => {
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::WeekSelectMove(delta) => {
                let n = self.item_count() as i32;
                if n > 0 {
                    self.selected = (self.selected as i32 + delta).clamp(0, n - 1) as usize;
                }
            }
            _ => {}
        }
        None
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

        if items.is_empty() {
            lines.extend(empty_lines());
        } else {
            let title_w = items
                .iter()
                .map(|i| i.title.chars().count())
                .max()
                .unwrap_or(16)
                .clamp(16, 32);
            for (i, item) in items.iter().enumerate() {
                lines.push(plan_row(item, title_w, i == self.selected));
            }
            lines.push(Line::from(""));
            lines.push(summary_line(data));
        }

        lines.extend(retro_lines(data));

        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    pub fn hints(&self) -> Line<'static> {
        widgets::footer_hints(&[
            ("j/k", "select"),
            ("[", "prev wk"),
            ("]", "next wk"),
            ("t", "this wk"),
            ("h", "home"),
        ])
    }
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
/// state, the state pill, and the time. `▌` (accent) flags the selected row.
fn plan_row(item: &PlanItem, title_w: usize, selected: bool) -> Line<'static> {
    let state = item.retro_state();
    let color = state_color(state);
    let logged = item.logged_minutes.unwrap_or(0);
    let planned = item.size_minutes.unwrap_or(0);
    let fraction = if planned > 0 {
        logged as f64 / planned as f64
    } else if logged > 0 {
        1.0
    } else {
        0.0
    };

    let marker = if selected { "▌ " } else { "  " };
    let kind = item.kind.as_deref().unwrap_or("");
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(marker.to_string(), Style::default().fg(theme::ACCENT)),
        Span::styled(super::pad_or_truncate(kind, 8), theme::muted()),
        Span::raw(format!(
            " {}  ",
            super::pad_or_truncate(&item.title, title_w)
        )),
    ];
    // A tick-free pace bar coloured by the derived state; empty cells read as the
    // dim `··········` the design shows for an untouched intent.
    spans.extend(widgets::pace_bar(fraction, 0.0, BAR_WIDTH, color, false));
    spans.push(Span::raw("  "));
    spans.push(plan_pill(state));
    spans.push(Span::raw(format!(
        "  {} / {}",
        fmt_hm(logged),
        fmt_hm(planned)
    )));
    Line::from(spans)
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

/// The retro band — the one stored line. Reads `note.body`; an unwritten week
/// shows the calm empty state. The `$EDITOR` reflection *write* is a later slice,
/// so this is read-and-display only.
fn retro_lines(data: &WeekData) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "reflection · this week's note",
            theme::muted(),
        )),
    ];
    let body = data.note.body.trim();
    if body.is_empty() {
        lines.push(Line::from(Span::styled(
            "No reflection yet.",
            theme::muted(),
        )));
    } else {
        for line in body.lines() {
            lines.push(Line::from(Span::raw(line.to_string())));
        }
    }
    lines
}

/// §Week · nothing planned — the calm invitation. Points at the shipped
/// `engineer plan add` one-liner, and advertises the in-app `a` gesture the
/// plan-write slice adds next (the design shows both).
fn empty_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Nothing planned for this week yet.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "Say what it's for — plan an intent with `engineer plan add` from the shell.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "(in-app `a` to add an intent is coming)",
            Style::default().fg(theme::BORDER),
        )),
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

        let done = spans_text(&plan_row(items[0], 24, false));
        assert!(done.contains("SICP"), "{done}");
        assert!(done.contains("done"), "{done}");
        // 190 logged / 180 planned → 3h10 / 3h.
        assert!(done.contains("3h10 / 3h"), "{done}");

        let hold = spans_text(&plan_row(items[1], 24, false));
        assert!(hold.contains("hold"), "{hold}");
        assert!(hold.contains("1h55 / 3h"), "{hold}");

        let untouched = spans_text(&plan_row(items[2], 24, false));
        assert!(untouched.contains("untouched"), "{untouched}");
        assert!(untouched.contains("0 / 1h"), "{untouched}");
    }

    #[test]
    fn plan_row_marks_the_selected_row() {
        let data = sample();
        let item = data.items().next().unwrap();
        assert!(spans_text(&plan_row(item, 24, true)).starts_with('▌'));
        assert!(!spans_text(&plan_row(item, 24, false)).starts_with('▌'));
    }

    #[test]
    fn summary_line_reads_planned_done_logged() {
        // 305 logged minutes → 5.1h.
        assert_eq!(
            spans_text(&summary_line(&sample())),
            "planned 3 · done 1 · 5.1h logged"
        );
    }

    #[test]
    fn retro_band_shows_the_note_then_the_empty_state() {
        let written: String = retro_lines(&sample())
            .iter()
            .map(|l| spans_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            written.contains("reflection · this week's note"),
            "{written}"
        );
        assert!(written.contains("Read the paper first"), "{written}");

        let blank: String = retro_lines(&empty())
            .iter()
            .map(|l| spans_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blank.contains("No reflection yet."), "{blank}");
    }

    #[test]
    fn nothing_planned_teaches_the_plan_add_verb() {
        let text: String = empty_lines()
            .iter()
            .map(|l| spans_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Nothing planned"), "{text}");
        assert!(text.contains("engineer plan add"), "{text}");
        // The design shows the in-app `a` gesture, advertised as coming.
        assert!(text.contains("`a`"), "{text}");
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
        assert!(text.contains("Nothing planned"), "{text}");
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
}
