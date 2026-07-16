//! Home — the daily loop's opening question, served from one `GET /api/v1/today`
//! read. It leads with the two ambient reads that decide the next move — the
//! running timer (the header's `timer_cell` atom, drawn larger as a band) and
//! this week's pace fold — over today's plan, the logged/review strip, and the
//! books mid-chapter. Home owns no write; its blocks link out (Timer / Progress
//! / Review).

use std::time::Instant;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, BookStatus, Today};
use crate::app::action::Action;
use crate::app::screens::timer::live_elapsed;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

#[derive(Default)]
pub struct Home {
    today: Option<Today>,
    /// Monotonic instant the current `today` snapshot arrived — the base for
    /// ticking the lead band's elapsed between refreshes, exactly as the header
    /// cell ticks (both share `timer::live_elapsed`).
    loaded_at: Option<Instant>,
    loading: bool,
    /// The ambient pending-drafts count (§Inbox · the ambient count). Loaded by
    /// a light `list_pending_tasks()` fetch beside the `today()` load, since the
    /// `/today` aggregate carries no drafts count. `0` renders nothing — quiet by
    /// default; `inbox_expiring` escalates the chip to amber when a draft is near
    /// expiry (the design's "escalates once").
    inbox_pending: usize,
    inbox_expiring: bool,
}

impl Home {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        spawn_load(api.clone(), tx.clone());
        spawn_inbox_count(api.clone(), tx.clone());
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::TodayLoaded(today) => {
                self.today = Some(*today);
                self.loaded_at = Some(Instant::now());
                self.loading = false;
            }
            Action::HomeLoadFailed => self.loading = false,
            Action::HomeInboxLoaded { pending, expiring } => {
                self.inbox_pending = pending;
                self.inbox_expiring = expiring;
            }
            Action::RefreshHome => {
                self.loading = true;
                spawn_load(api.clone(), tx.clone());
                spawn_inbox_count(api.clone(), tx.clone());
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let Some(today) = self.today.as_ref() else {
            let msg = if self.loading {
                "loading…"
            } else {
                "Couldn't load today. Press `r` to refresh."
            };
            frame.render_widget(Paragraph::new(msg).block(bordered("Home")), area);
            return;
        };

        // The screen reads top to bottom: the date, the timer lead band, today's
        // plan, the logged/review/left strip, and the books mid-chapter beneath.
        let plan_rows = if today.plan.items.is_empty() {
            2 // the calm two-line invitation
        } else {
            today.plan.items.len() as u16
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),             // date line
                Constraint::Length(4),             // lead band: timer + pace fold
                Constraint::Length(plan_rows + 2), // today's plan (+ borders)
                Constraint::Length(2),             // stats strip (+ a spacer row)
                Constraint::Min(4),                // mid-chapter reading
            ])
            .split(area);

        self.render_header_row(frame, chunks[0], today);
        self.render_lead(frame, chunks[1], today);
        render_plan(frame, chunks[2], today);
        render_stats(frame, chunks[3], today);
        render_reading(frame, chunks[4], today);
    }

    /// The top row: the date (right-aligned) with the ambient inbox chip on the
    /// left when drafts are pending (§Inbox · the ambient count — the design's
    /// header-cell footprint). Split so the chip never collides with the date; a
    /// muted `◧ inbox N` normally, an amber `▾ inbox N` when a draft is near
    /// expiry. Nothing shows at inbox zero — quiet by default.
    fn render_header_row(&self, frame: &mut Frame, area: Rect, today: &Today) {
        if self.inbox_pending == 0 {
            render_date(frame, area, today);
            return;
        }
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);
        let (glyph, style) = if self.inbox_expiring {
            (
                "▾ ",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("◧ ", theme::muted())
        };
        let chip = Line::from(vec![
            Span::styled(glyph, style),
            Span::styled(format!("inbox {}", self.inbox_pending), style),
            Span::styled("  i →", theme::focused()),
        ]);
        frame.render_widget(Paragraph::new(chip), cols[0]);
        render_date(frame, cols[1], today);
    }

    /// The lead band — the two ambient reads that decide the next move: the
    /// running timer as `widgets::timer_cell` (the *same atom, larger*, not a
    /// second timer face), and the pace fold beneath it. When nothing runs, the
    /// calm idle line leads instead.
    fn render_lead(&self, frame: &mut Frame, area: Rect, today: &Today) {
        let elapsed = live_elapsed(&today.timer, self.loaded_at);
        let timer_line = match widgets::timer_cell(&today.timer, elapsed, false, false) {
            Some(spans) => Line::from(spans),
            None => Line::from(vec![
                Span::styled("○ ", theme::muted()),
                Span::styled(
                    "no timer",
                    Style::default()
                        .fg(theme::MUTED)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("   ⎵ t start", theme::muted()),
            ]),
        };
        frame.render_widget(
            Paragraph::new(vec![timer_line, pace_line(today)]).block(bordered("")),
            area,
        );
    }
}

/// The pace fold beneath the timer — this week's promise, pre-folded to the
/// single worst-behind target. `pace: null` is the on-pace state, rendered as a
/// calm line (silence is on-pace, baked into the API — never a red or empty
/// panel). Behind wears a small warn chip naming the worst target by scope and
/// how many trail. The full now-tick meters stay on Progress (`g p`) — the
/// `/today` fold carries only `delta_minutes`, not the fill/expected/target the
/// meter needs.
fn pace_line(today: &Today) -> Line<'static> {
    match today.pace.as_ref() {
        None => Line::from(vec![
            Span::styled("✓ ", Style::default().fg(theme::SUCCESS)),
            Span::styled(
                "This week · on pace",
                Style::default()
                    .fg(theme::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   nothing behind — the quiet state", theme::muted()),
        ]),
        Some(pace) => {
            let hours = pace.worst.delta_minutes as f64 / 60.0;
            Line::from(vec![
                Span::styled(format!("{:<10}", pace.worst.scope_name), theme::muted()),
                Span::styled(
                    format!(" behind {hours:.1}h "),
                    Style::default().fg(Color::Black).bg(theme::WARN),
                ),
                Span::styled(
                    format!("   worst of {} targets trailing", pace.behind_count),
                    theme::muted(),
                ),
                Span::styled("   g p → Progress", theme::focused()),
            ])
        }
    }
}

fn render_date(frame: &mut Frame, area: Rect, today: &Today) {
    let d = today.date.day;
    let line = format!(
        "{} · {} {} · {}",
        title_case(&today.date.weekday),
        d.strftime("%b"),
        d.day(),
        today.date.week
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(line, theme::muted()))).alignment(Alignment::Right),
        area,
    );
}

fn render_plan(frame: &mut Frame, area: Rect, today: &Today) {
    let plan = &today.plan;
    if plan.items.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("Nothing planned for today."),
                Line::from(Span::styled(
                    "the day is open — ⎵ p plan it, or ⎵ t just start a timer",
                    theme::muted(),
                )),
            ])
            .alignment(Alignment::Center)
            .block(bordered("Today's plan")),
            area,
        );
        return;
    }

    let done = plan.items.iter().filter(|i| i.state == "done").count();
    let live = plan.items.iter().filter(|i| i.state == "live").count();
    let logged: u32 = plan.items.iter().map(|i| i.logged_minutes).sum();
    let planned: u32 = plan.items.iter().map(|i| i.size_minutes).sum();
    let title = format!(
        "Today's plan · {done} done · {live} live · {} left · {logged}m logged / {planned}m planned",
        plan.left_count
    );

    let rows: Vec<Row> = plan
        .items
        .iter()
        .map(|it| {
            let (glyph, gcolor) = state_glyph(&it.state);
            let title_cell = match it.moved_from.as_deref() {
                Some(from) => Cell::from(Line::from(vec![
                    Span::raw(it.title.clone()),
                    Span::styled(format!("  moved from {from}"), theme::muted()),
                ])),
                None => Cell::from(it.title.clone()),
            };
            let row = Row::new(vec![
                Cell::from(glyph).style(Style::default().fg(gcolor)),
                Cell::from(it.kind.clone().unwrap_or_default()).style(theme::muted()),
                title_cell,
                Cell::from(format!("{} / {}m", it.logged_minutes, it.size_minutes))
                    .style(theme::muted()),
            ]);
            // The live item leads the eye — the full-row selection highlight.
            if it.state == "live" {
                row.style(theme::selection())
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(10),
            Constraint::Min(10),
            Constraint::Length(12),
        ],
    )
    .block(bordered(title));
    frame.render_widget(table, area);
}

fn render_stats(frame: &mut Frame, area: Rect, today: &Today) {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let sep = Span::styled("   ·   ", theme::muted());
    let line = Line::from(vec![
        Span::styled("logged today  ", theme::muted()),
        Span::styled(fmt_minutes(today.totals.logged_minutes), bold),
        sep.clone(),
        Span::styled("review  ", theme::muted()),
        Span::styled(format!("{} due", today.review.due_count), bold),
        Span::styled(
            format!(" · {} stale", today.review.stale_count),
            Style::default().fg(theme::WARN),
        ),
        Span::styled(format!(" · ~{}m", today.review.est_minutes), theme::muted()),
        Span::styled("  g r →", theme::focused()),
        sep,
        Span::styled("left to plan  ", theme::muted()),
        Span::styled(format!("{} items", today.plan.left_count), bold),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_reading(frame: &mut Frame, area: Rect, today: &Today) {
    let block = bordered(format!("Mid-chapter · {} books", today.reading.len()));
    if today.reading.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("No books in progress.", theme::muted())).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = today
        .reading
        .iter()
        .map(|b| {
            let mut head = vec![
                widgets::status_pill(BookStatus::Reading),
                Span::raw("  "),
                Span::styled(
                    b.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(author) = b.author.as_deref() {
                head.push(Span::styled(format!("  · {author}"), theme::muted()));
            }
            if let Some(ch) = b.next_chapter.as_ref() {
                head.push(Span::styled(
                    format!("    next ch.{} · {}", ch.number, ch.title),
                    theme::muted(),
                ));
            }

            let mut bar = widgets::progress_bar(b.progress_percent.unwrap_or(0.0), 30);
            if let Some(total) = b.chapters_total {
                // Chapters read = the next unread chapter's number minus one; a
                // finished book (no next chapter) has read them all.
                let read = b
                    .next_chapter
                    .as_ref()
                    .map(|c| c.number.saturating_sub(1))
                    .unwrap_or(total);
                bar.spans.push(Span::styled(
                    format!("   ·   {read} / {total} ch"),
                    theme::muted(),
                ));
            }

            ListItem::new(vec![Line::from(head), bar, Line::from("")])
        })
        .collect();
    frame.render_widget(List::new(items).block(block), area);
}

/// The plan-row glyph for a lifecycle state: `✓` done, `●` live, `·` left,
/// `○` planned (and the fallback for anything unrecognised).
fn state_glyph(state: &str) -> (&'static str, Color) {
    match state {
        "done" => ("✓", theme::SUCCESS),
        "live" => ("●", theme::SUCCESS),
        "left" => ("·", theme::WARN),
        _ => ("○", theme::MUTED),
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn fmt_minutes(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

fn spawn_load(api: ApiClient, tx: UnboundedSender<Action>) {
    tokio::spawn(async move {
        match api.today().await {
            Ok(today) => {
                let _ = tx.send(Action::TodayLoaded(Box::new(today)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("today failed: {e}"),
                });
                let _ = tx.send(Action::HomeLoadFailed);
            }
        }
    });
}

/// The ambient pending-drafts count — a light `list_pending_tasks()` read beside
/// the `today()` load (the `/today` aggregate carries no drafts count, and the
/// CLI invents no server endpoint). Quiet by default: a failed read stays silent
/// (the chip just doesn't show), never a notify tile.
fn spawn_inbox_count(api: ApiClient, tx: UnboundedSender<Action>) {
    tokio::spawn(async move {
        if let Ok(tasks) = api.list_pending_tasks().await {
            let expiring = tasks.iter().any(super::inbox::is_expiring_soon);
            let _ = tx.send(Action::HomeInboxLoaded {
                pending: tasks.len(),
                expiring,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::mpsc;

    fn deps() -> (ApiClient, mpsc::UnboundedSender<Action>) {
        let api = ApiClient::with_token(url::Url::parse("http://localhost").unwrap(), "tok".into());
        let (tx, _rx) = mpsc::unbounded_channel();
        (api, tx)
    }

    fn today(json: serde_json::Value) -> Today {
        serde_json::from_value(json).unwrap()
    }

    fn render_home(home: &mut Home) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                home.render(f, area);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn full() -> serde_json::Value {
        serde_json::json!({
            "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
            "timer": { "running": true, "bound": true,
                       "label": "Raft leader election", "elapsed_seconds": 1453 },
            "pace": null,
            "plan": { "items": [
                { "id":1, "title":"Designing Data-Intensive Apps", "state":"done",
                  "kind":"read", "size_minutes":45, "logged_minutes":45 },
                { "id":2, "title":"Raft leader election", "state":"live",
                  "kind":"build", "size_minutes":120, "logged_minutes":34 },
                { "id":3, "title":"Spaced-rep drills", "state":"left",
                  "kind":"review", "size_minutes":25, "logged_minutes":0, "moved_from":"Sun" }
            ], "left_count": 2 },
            "totals": { "logged_minutes": 95 },
            "review": { "due_count": 4, "stale_count": 1, "est_minutes": 25 },
            "reading": [
                { "id":10, "title":"Designing Data-Intensive Applications", "author":"Kleppmann",
                  "progress_percent":42, "chapters_total":12,
                  "next_chapter": { "number":7, "title":"Transactions" } }
            ]
        })
    }

    #[tokio::test]
    async fn today_loaded_populates_and_renders_the_enriched_home() {
        let (api, tx) = deps();
        let mut home = Home::default();
        home.handle(Action::TodayLoaded(Box::new(today(full()))), &api, &tx)
            .await;

        assert!(home.today.is_some());
        let text = render_home(&mut home);

        // The lead band shows the running timer, verbatim from the shared atom.
        assert!(text.contains("Raft leader election"), "lead band: {text}");
        // Today's plan panel with its counts and a carried-over row.
        assert!(text.contains("Today's plan"), "{text}");
        assert!(text.contains("1 done"), "counts: {text}");
        assert!(text.contains("1 live"), "counts: {text}");
        assert!(text.contains("moved from Sun"), "carry-over: {text}");
        // The logged/review/left strip.
        assert!(text.contains("logged today"), "stats: {text}");
        assert!(text.contains("4 due"), "review: {text}");
        // The mid-chapter reading panel with where-you-are.
        assert!(text.contains("Mid-chapter"), "{text}");
        assert!(text.contains("Transactions"), "next chapter: {text}");
        // `pace: null` folds to the calm on-pace line — no meter, no red.
        assert!(text.contains("on pace"), "on-pace fold: {text}");
        assert!(
            !text.contains("targets trailing"),
            "no behind chip when on pace: {text}"
        );
    }

    #[tokio::test]
    async fn behind_pace_folds_to_the_worst_target_chip() {
        let (api, tx) = deps();
        let mut home = Home::default();
        home.handle(
            Action::TodayLoaded(Box::new(today(serde_json::json!({
                "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
                "timer": { "running": true, "bound": true, "label": "Raft", "elapsed_seconds": 1453 },
                "pace": { "behind_count": 2, "worst": {
                    "target_id": 42, "axis": "domain", "scope_value": 7,
                    "scope_name": "systems", "delta_minutes": 108
                } }
            })))),
            &api,
            &tx,
        )
        .await;

        let text = render_home(&mut home);
        // The worst target named by scope, the warn chip (108m => 1.8h), and how
        // many trail. No on-pace line when behind.
        assert!(text.contains("systems"), "scope: {text}");
        assert!(text.contains("behind 1.8h"), "warn chip: {text}");
        assert!(
            text.contains("2 targets trailing"),
            "trailing count: {text}"
        );
        assert!(
            !text.contains("on pace"),
            "no calm line when behind: {text}"
        );
    }

    #[tokio::test]
    async fn ambient_inbox_chip_shows_the_pending_count_and_hides_at_zero() {
        let (api, tx) = deps();
        let mut home = Home::default();
        home.handle(Action::TodayLoaded(Box::new(today(full()))), &api, &tx)
            .await;

        // Zero pending: the chip stays hidden — quiet by default.
        assert!(
            !render_home(&mut home).contains("inbox 3"),
            "chip hidden at zero"
        );

        // A pending count surfaces the chip in the stats strip.
        home.handle(
            Action::HomeInboxLoaded {
                pending: 3,
                expiring: false,
            },
            &api,
            &tx,
        )
        .await;
        let text = render_home(&mut home);
        assert!(text.contains("inbox 3"), "ambient chip: {text}");
        assert!(text.contains("i →"), "triage affordance: {text}");
    }

    #[tokio::test]
    async fn empty_plan_and_idle_timer_render_the_calm_states() {
        let (api, tx) = deps();
        let mut home = Home::default();
        home.handle(
            Action::TodayLoaded(Box::new(today(serde_json::json!({
                "date": { "day": "2026-07-06", "weekday": "mon", "week": "2026-W28" },
                "timer": { "running": false },
                "pace": null
            })))),
            &api,
            &tx,
        )
        .await;

        let text = render_home(&mut home);
        // Idle timer: the band is a calm `no timer`, never blank.
        assert!(text.contains("no timer"), "idle band: {text}");
        // Empty plan: a calm invitation, not a blank panel.
        assert!(
            text.contains("Nothing planned for today"),
            "empty plan: {text}"
        );
    }
}
