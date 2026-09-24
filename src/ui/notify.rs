//! Typed, self-expiring user notifications.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::time::Instant;

use crate::ui::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    pub fn icon(self) -> &'static str {
        match self {
            Level::Info => "ℹ",
            Level::Success => "✓",
            Level::Warning => "⚠",
            Level::Error => "✖",
        }
    }

    pub fn style(self) -> Style {
        match self {
            Level::Info => Style::default().fg(theme::ACCENT),
            Level::Success => Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
            Level::Warning => Style::default().fg(theme::INK_ON_FILL).bg(theme::WARN),
            Level::Error => Style::default().fg(theme::INK_ON_FILL).bg(theme::DANGER),
        }
    }

    pub fn ttl(self) -> Duration {
        match self {
            Level::Info | Level::Success => Duration::from_secs(4),
            Level::Warning => Duration::from_secs(6),
            Level::Error => Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub level: Level,
    pub text: String,
    pub created: Instant,
}

impl Notification {
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
            created: Instant::now(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created.elapsed() > self.level.ttl()
    }
}

pub fn render_notification(frame: &mut Frame, area: Rect, n: &Notification) {
    let line = Line::from(Span::styled(
        format!(" {} {} ", n.level.icon(), n.text),
        n.level.style(),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn levels_are_distinct() {
        assert_ne!(Level::Info.icon(), Level::Error.icon());
        assert_ne!(Level::Success.icon(), Level::Warning.icon());
        assert!(Level::Error.ttl() > Level::Info.ttl());
    }

    #[tokio::test]
    async fn renders_error_tile_with_icon_and_text() {
        let n = Notification::new(Level::Error, "boom");
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
        terminal
            .draw(|f| render_notification(f, f.area(), &n))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("boom"));
        assert!(text.contains(Level::Error.icon()));
    }
}
