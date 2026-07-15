//! Palette adapted from Engineer's design tokens (docs/designs/tokens.css).
//! 256-colour values map cleanly to most modern terminal emulators.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Indexed(105); // periwinkle — indigo-light, true to the brand hue
pub const ACCENT_DIM: Color = Color::Indexed(61); // indigo-dim, matches the accent hue
pub const BORDER: Color = Color::Indexed(240);
pub const MUTED: Color = Color::Indexed(244);
pub const SUCCESS: Color = Color::Indexed(108);
pub const WARN: Color = Color::Indexed(179);
pub const DANGER: Color = Color::Indexed(167);

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
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}
