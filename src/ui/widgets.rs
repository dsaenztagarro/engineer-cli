use ratatui::style::{Color, Style};
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
