use ratatui::style::{Color, Modifier, Style};

use super::tokens;

pub const ACCENT: Color = Color::Indexed(tokens::ACCENT);
pub const ACCENT_DIM: Color = Color::Indexed(tokens::ACCENT_DIM);
pub const BORDER: Color = Color::Indexed(tokens::RULE);
pub const MUTED: Color = Color::Indexed(tokens::TEXT_SECONDARY);
pub const INK_ON_FILL: Color = Color::Indexed(tokens::TEXT_INVERSE);
// The token set has no decisions for work states yet (ADR 0006), so pace, pills
// and queue states borrow the notice decisions whose hues they share.
pub const SUCCESS: Color = Color::Indexed(tokens::NOTICE_SUCCESS);
pub const WARN: Color = Color::Indexed(tokens::NOTICE_WARNING);
pub const DANGER: Color = Color::Indexed(tokens::ERROR);

pub fn focused() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn header() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn selection() -> Style {
    Style::default()
        .bg(ACCENT_DIM)
        .fg(INK_ON_FILL)
        .add_modifier(Modifier::BOLD)
}
