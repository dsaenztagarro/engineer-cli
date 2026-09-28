use std::io::IsTerminal;

use ratatui::style::{Color, Modifier, Style};

use super::tokens;

/// The colours both faces paint text with, named once for the screen and the
/// headless verbs alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Accent,
    Muted,
    // The token set has no decisions for work states yet (the terminal-surface
    // record), so pace, pills and queue states borrow the notice decisions
    // whose hues they share.
    Success,
    Warn,
    Danger,
}

impl Ink {
    pub const fn index(self) -> u8 {
        match self {
            Ink::Accent => tokens::ACCENT,
            Ink::Muted => tokens::TEXT_SECONDARY,
            Ink::Success => tokens::NOTICE_SUCCESS,
            Ink::Warn => tokens::NOTICE_WARNING,
            Ink::Danger => tokens::ERROR,
        }
    }

    pub const fn color(self) -> Color {
        Color::Indexed(self.index())
    }
}

pub const ACCENT: Color = Ink::Accent.color();
pub const ACCENT_DIM: Color = Color::Indexed(tokens::ACCENT_DIM);
pub const BORDER: Color = Color::Indexed(tokens::RULE);
pub const MUTED: Color = Ink::Muted.color();
pub const INK_ON_FILL: Color = Color::Indexed(tokens::TEXT_INVERSE);
pub const SUCCESS: Color = Ink::Success.color();
pub const WARN: Color = Ink::Warn.color();
pub const DANGER: Color = Ink::Danger.color();

/// Whether a headless verb colours its output: only on a terminal, and never
/// under `NO_COLOR`.
pub fn headless_colour() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Paint headless output in an ink, or leave it plain.
pub fn paint(s: &str, ink: Ink, colored: bool) -> String {
    if colored {
        format!("\x1b[38;5;{}m{s}\x1b[0m", ink.index())
    } else {
        s.to_string()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_output_is_plain_when_colour_is_off() {
        assert_eq!(paint("✗ diverged", Ink::Danger, false), "✗ diverged");
    }

    #[test]
    fn danger_ink_paints_with_the_error_token() {
        assert_eq!(
            paint("✗", Ink::Danger, true),
            format!("\x1b[38;5;{}m✗\x1b[0m", tokens::ERROR)
        );
    }
}
