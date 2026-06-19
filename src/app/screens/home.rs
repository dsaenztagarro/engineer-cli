use jiff::{civil::Date, ToSpan, Zoned};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{Activity, ActivityFilters, ApiClient, Book, BookStatus};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

#[derive(Default)]
pub struct Home {
    today: Vec<Activity>,
    reading: Vec<Book>,
    loading: bool,
}

impl Home {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        spawn_load(api.clone(), tx.clone());
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::HomeLoaded { today, reading } => {
                self.today = today;
                self.reading = reading;
                self.loading = false;
            }
            Action::RefreshHome => {
                self.loading = true;
                spawn_load(api.clone(), tx.clone());
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);

        // ---- today's activities ----
        let total_min: u32 = self.today.iter().filter_map(|a| a.duration_minutes).sum();
        let title = format!("Today  ·  {} min logged", total_min);
        let block = bordered(title);

        if self.loading && self.today.is_empty() {
            frame.render_widget(Paragraph::new("loading…").block(block), chunks[0]);
        } else if self.today.is_empty() {
            frame.render_widget(
                Paragraph::new("No activity logged today. Press `a` to add one.").block(block),
                chunks[0],
            );
        } else {
            let rows: Vec<Row> = self
                .today
                .iter()
                .map(|a| {
                    let time = a
                        .started_at
                        .map(|t| t.strftime("%H:%M").to_string())
                        .unwrap_or_default();
                    let dur = a
                        .duration_minutes
                        .map(|d| format!("{d}m"))
                        .unwrap_or_default();
                    let kind = a.kind.clone().unwrap_or_default();
                    Row::new(vec![
                        Cell::from(time).style(theme::muted()),
                        Cell::from(kind),
                        Cell::from(dur).style(theme::muted()),
                        Cell::from(a.title.clone()),
                    ])
                })
                .collect();
            let table = Table::new(
                rows,
                [
                    Constraint::Length(6),
                    Constraint::Length(14),
                    Constraint::Length(6),
                    Constraint::Min(10),
                ],
            )
            .header(Row::new(vec!["TIME", "KIND", "DUR", "TITLE"]).style(theme::header()))
            .block(block);
            frame.render_widget(table, chunks[0]);
        }

        // ---- currently reading ----
        let block2 = bordered(format!("Currently reading  ·  {}", self.reading.len()));
        if self.reading.is_empty() && !self.loading {
            frame.render_widget(
                Paragraph::new("No books in `reading` status.").block(block2),
                chunks[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .reading
                .iter()
                .map(|b| {
                    let pct = b.progress_percent.unwrap_or(0.0);
                    ListItem::new(vec![
                        Line::from(vec![
                            widgets::status_pill(BookStatus::Reading),
                            Span::raw("  "),
                            Span::styled(
                                b.title.clone(),
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("  · {}", b.author.clone().unwrap_or_default()),
                                theme::muted(),
                            ),
                        ]),
                        widgets::progress_bar(pct, 30),
                        Line::from(""),
                    ])
                })
                .collect();
            frame.render_widget(List::new(items).block(block2), chunks[1]);
        }
    }
}

fn spawn_load(api: ApiClient, tx: UnboundedSender<Action>) {
    tokio::spawn(async move {
        let now = Zoned::now();
        let date: Date = now.date();
        let start = date
            .to_zoned(now.time_zone().clone())
            .ok()
            .map(|z| z.timestamp());
        let end = start.and_then(|s| s.checked_add(1.day()).ok());

        let filters = ActivityFilters {
            started_after: start,
            started_before: end,
            book_id: None,
        };
        let today = match api.list_activities(&filters).await {
            Ok(list) => list.data,
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("today's activities failed: {e}"),
                });
                vec![]
            }
        };
        let reading = match api.list_books(Some(BookStatus::Reading), None).await {
            Ok(list) => list.data,
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("reading list failed: {e}"),
                });
                vec![]
            }
        };
        let _ = tx.send(Action::HomeLoaded { today, reading });
    });
}
