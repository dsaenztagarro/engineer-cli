use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::notify::{render_notification, Notification};
use crate::ui::theme;

pub struct Chrome<'a> {
    pub user: Option<&'a str>,
    pub identity_host: &'a str,
    pub screen_title: &'a str,
    pub timer: Option<Vec<Span<'a>>>,
    pub notification: Option<&'a Notification>,
    pub hints: Line<'a>,
}

pub fn render_chrome(frame: &mut Frame, area: Rect, chrome: Chrome<'_>) -> Rect {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let user = chrome.user.unwrap_or("not signed in");
    let header = Line::from(vec![
        Span::styled("engineer", theme::header()),
        Span::styled("  ›  ", theme::muted()),
        Span::raw(chrome.screen_title.to_string()),
        Span::styled("    ", theme::muted()),
        Span::styled(format!("{user} @ {}", chrome.identity_host), theme::muted()),
    ]);

    if let Some(cell) = chrome.timer {
        let width: u16 = cell
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum::<u16>()
            .saturating_add(1);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(width)])
            .split(chunks[0]);
        frame.render_widget(Paragraph::new(header), cols[0]);
        frame.render_widget(
            Paragraph::new(Line::from(cell)).alignment(ratatui::layout::Alignment::Right),
            cols[1],
        );
    } else {
        frame.render_widget(Paragraph::new(header), chunks[0]);
    }

    if let Some(n) = chrome.notification {
        render_notification(frame, chunks[2], n);
    } else {
        frame.render_widget(Paragraph::new(chrome.hints.clone()), chunks[2]);
    }

    chunks[1]
}

pub fn bordered(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .title(Span::styled(format!(" {} ", title.into()), theme::header()))
}
