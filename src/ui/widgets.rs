use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::BookStatus;
use crate::ui::theme;

pub fn status_pill(status: BookStatus) -> Span<'static> {
    let (label, fg) = match status {
        BookStatus::Reading => (" reading ", theme::ACCENT),
        BookStatus::Completed => (" done ", theme::SUCCESS),
        BookStatus::Unread => (" unread ", theme::MUTED),
        BookStatus::OnHold => (" hold ", theme::WARN),
        BookStatus::Abandoned => (" stop ", theme::DANGER),
    };
    Span::styled(label, Style::default().fg(Color::Black).bg(fg))
}

/// Inline progress bar like `███▍·····  42%`.
pub fn progress_bar(pct: f32, width: usize) -> Line<'static> {
    let pct = pct.clamp(0.0, 100.0);
    let filled = (pct / 100.0) * width as f32;
    let full = filled.floor() as usize;
    let frac = filled - full as f32;
    let partial = match (frac * 8.0) as usize {
        0 => "",
        1 => "▏",
        2 => "▎",
        3 => "▍",
        4 => "▌",
        5 => "▋",
        6 => "▊",
        _ => "▉",
    };
    let empty = width.saturating_sub(full + if partial.is_empty() { 0 } else { 1 });
    let bar = format!("{}{}{}", "█".repeat(full), partial, "·".repeat(empty));
    Line::from(vec![
        Span::styled(bar, Style::default().fg(theme::ACCENT)),
        Span::raw(format!("  {pct:>3.0}%")),
    ])
}

/// Pace meter bar like `█████·╎····` (progress.html §F). `fraction` is
/// actual/target (fill); `now_fraction` places the muted now-tick where the week
/// currently sits (the "am I on pace" mark). `color` tints the fill by pace
/// state. `show_tick` is false for met targets, whose bar is already full.
pub fn pace_bar(
    fraction: f64,
    now_fraction: f64,
    width: usize,
    color: Color,
    show_tick: bool,
) -> Vec<Span<'static>> {
    let filled = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    let now_col = if show_tick && width > 0 {
        Some((now_fraction.clamp(0.0, 1.0) * width as f64).floor() as usize)
            .map(|c| c.min(width - 1))
    } else {
        None
    };
    let fill_style = Style::default().fg(color);
    let empty_style = Style::default().fg(theme::BORDER);
    let tick_style = theme::muted();

    // Coalesce equal-styled cells into runs so the bar renders as a few spans.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut buf_style = empty_style;
    for i in 0..width {
        let (ch, style) = if now_col == Some(i) {
            ("╎", tick_style)
        } else if i < filled {
            ("█", fill_style)
        } else {
            ("·", empty_style)
        };
        if style != buf_style && !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut buf), buf_style));
        }
        buf_style = style;
        buf.push_str(ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, buf_style));
    }
    spans
}

/// Elapsed time in the timer's compact idiom: `mm:ss` under an hour, widening
/// to `h:mm:ss` (then `hh:mm:ss`) once it crosses one hour. Mirrors the web
/// pill contract (navigation-bar.html §M) — the number grows a field, it never
/// shape-shifts by title or kind.
pub fn fmt_elapsed(total_secs: i64) -> String {
    let total = total_secs.max(0);
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// The persistent header timer cell (web pill contract, translated to the grid):
/// a fixed-width glyph + elapsed, no title, one accent colour, never
/// shape-shifting. Running is a solid accent `●`; paused is an amber `‖` (the
/// clock has stopped advancing). Absent returns `None` — the cell renders as
/// nothing, so a screen with no live timer has a clean header.
pub fn timer_cell(running: bool, paused: bool, elapsed_secs: i64) -> Option<Vec<Span<'static>>> {
    if !running {
        return None;
    }
    let (glyph, color) = if paused {
        ("‖", theme::WARN)
    } else {
        ("●", theme::ACCENT)
    };
    let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    Some(vec![Span::styled(
        format!("{glyph} {}", fmt_elapsed(elapsed_secs)),
        style,
    )])
}

pub fn footer_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(hints.len() * 3);
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme::muted()));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default().fg(Color::Black).bg(theme::ACCENT),
        ));
        spans.push(Span::raw(format!(" {label}")));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_uses_mm_ss_under_an_hour() {
        assert_eq!(fmt_elapsed(0), "00:00");
        assert_eq!(fmt_elapsed(59), "00:59");
        assert_eq!(fmt_elapsed(60), "01:00");
        assert_eq!(fmt_elapsed(272), "04:32");
        assert_eq!(fmt_elapsed(3599), "59:59");
    }

    #[test]
    fn fmt_elapsed_widens_to_h_mm_ss_at_one_hour() {
        assert_eq!(fmt_elapsed(3600), "1:00:00");
        assert_eq!(fmt_elapsed(3661), "1:01:01");
        assert_eq!(fmt_elapsed(36_000), "10:00:00");
    }

    #[test]
    fn fmt_elapsed_floors_negatives_to_zero() {
        assert_eq!(fmt_elapsed(-5), "00:00");
    }

    fn cell_text(cell: Option<Vec<Span<'static>>>) -> String {
        cell.map(|spans| spans.iter().map(|s| s.content.to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn timer_cell_running_shows_accent_dot_and_time() {
        let cell = timer_cell(true, false, 272).expect("running renders a cell");
        assert_eq!(cell[0].style.fg, Some(theme::ACCENT));
        assert_eq!(cell_text(Some(cell)), "● 04:32");
    }

    #[test]
    fn timer_cell_paused_shows_warn_pause_bar() {
        let cell = timer_cell(true, true, 3661).expect("paused renders a cell");
        assert_eq!(cell[0].style.fg, Some(theme::WARN));
        assert_eq!(cell_text(Some(cell)), "‖ 1:01:01");
    }

    #[test]
    fn timer_cell_absent_is_nothing() {
        assert!(timer_cell(false, false, 0).is_none());
    }
}
