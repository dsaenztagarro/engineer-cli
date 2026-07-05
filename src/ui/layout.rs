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
    /// The persistent timer cell (`widgets::timer_cell`), right-aligned in the
    /// header row on every screen. `None` when no timer is running.
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

    // The timer cell claims a fixed narrow slice at the far right of the header
    // (web pill contract: fixed width, never the activity title). When absent
    // the header text spans the whole row.
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

    // Footer shows an active notification as a level-styled tile; otherwise the
    // screen's keybinding hints.
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
