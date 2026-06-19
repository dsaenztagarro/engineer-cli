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
