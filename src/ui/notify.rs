//! Typed, self-expiring user notifications.
//!
//! Replaces the old single-string toast: every message now carries a `Level`
//! so successes, warnings, and failures are visually distinct and stay on
//! screen for a level-appropriate duration. Rendered as a one-line tile in the
//! chrome footer (see `ui::layout::render_chrome`).

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
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
    /// Leading glyph for the tile.
    pub fn icon(self) -> &'static str {
        match self {
            Level::Info => "ℹ",
            Level::Success => "✓",
            Level::Warning => "⚠",
            Level::Error => "✖",
        }
    }

    /// Tile style. Info/Success read as coloured accents; Warning/Error fill the
    /// row with a contrasting background so failures are impossible to miss.
    pub fn style(self) -> Style {
        match self {
            Level::Info => Style::default().fg(theme::ACCENT),
            Level::Success => Style::default().fg(theme::SUCCESS).add_modifier(Modifier::BOLD),
            Level::Warning => Style::default().fg(Color::Black).bg(theme::WARN),
            Level::Error => Style::default().fg(Color::Black).bg(theme::DANGER),
        }
    }

    /// How long the notification stays before auto-expiring. Errors linger
    /// longest since they usually require the user to act.
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
        Self { level, text: text.into(), created: Instant::now() }
    }

    /// True once the notification has outlived its level's TTL.
    pub fn is_expired(&self) -> bool {
        self.created.elapsed() > self.level.ttl()
    }
}

/// Render the notification as a single-line tile filling `area`.
pub fn render_notification(frame: &mut Frame, area: Rect, n: &Notification) {
    let line = Line::from(Span::styled(
        format!(" {} {} ", n.level.icon(), n.text),
        n.level.style(),
    ));
    frame.render_widget(Paragraph::new(line), area);
}
