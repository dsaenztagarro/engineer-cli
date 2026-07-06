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

/// An activity's lifecycle status as a black-ink semantic pill, matching the
/// book status-pill idiom. Status is free-form on the wire, so the colour is
/// keyed off the value and an unrecognised status still renders literally
/// (neutral fill) rather than being dropped. An absent status reads ` logged `.
pub fn activity_status_pill(status: Option<&str>) -> Span<'static> {
    let s = status.unwrap_or("").trim().to_ascii_lowercase();
    let (label, fg): (String, Color) = if s.is_empty() {
        (" logged ".into(), theme::MUTED)
    } else if s.contains("complet") || s == "done" {
        (" done ".into(), theme::SUCCESS)
    } else if s.contains("progress") || s.contains("started") || s.contains("active") {
        (" active ".into(), theme::ACCENT)
    } else if s.contains("plan") || s.contains("pending") || s.contains("todo") {
        (" planned ".into(), theme::MUTED)
    } else if s.contains("abandon") || s.contains("cancel") || s.contains("drop") {
        (" stopped ".into(), theme::DANGER)
    } else {
        (format!(" {} ", s.replace('_', " ")), theme::MUTED)
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

/// The persistent header timer cell, speaking the full status-line grammar
/// (timer.dc.html §Status line): one glyph carries the state, the bar keeps a
/// fixed shape per state, and `narrow` drops the label down to glyph + clock.
/// Absent returns `None` — a screen with no live timer has a clean header.
///
/// States: `●` running (green; muted title after the clock, italic *untitled*
/// when unbound), `‖` paused (amber, frozen muted clock), `◐` idle + amber
/// ` idle ` pill, `◆` focus work + pomodoro dots, `○ break` muted. The `over`
/// form arrives with the overrun ticket.
pub fn timer_cell(
    t: &crate::api::Timer,
    elapsed_secs: i64,
    narrow: bool,
    offer: bool,
) -> Option<Vec<Span<'static>>> {
    if !t.running {
        return None;
    }
    let time = fmt_elapsed(elapsed_secs);
    let focus = t.mode.as_deref() == Some("focus");
    let on_break = focus && t.phase.as_deref() == Some("break");
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if t.idle == Some(true) {
        spans.push(Span::styled("◐ ", bold(theme::WARN)));
        spans.push(Span::styled(time, bold(theme::WARN)));
        if !narrow {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                " idle ",
                Style::default().fg(Color::Black).bg(theme::WARN),
            ));
        }
    } else if t.paused {
        spans.push(Span::styled("‖ ", bold(theme::WARN)));
        spans.push(Span::styled(time, bold(theme::MUTED)));
        if !narrow {
            spans.push(Span::styled(" paused", theme::muted()));
        }
    } else if on_break {
        spans.push(Span::styled("○ ", bold(theme::MUTED)));
        spans.push(Span::styled(format!("break {time}"), bold(theme::MUTED)));
        if offer && !narrow {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                " work? ",
                Style::default().fg(Color::Black).bg(theme::ACCENT),
            ));
        } else if !narrow {
            spans.push(Span::styled(" not counting", theme::muted()));
        }
    } else if focus {
        spans.push(Span::styled("◆ ", bold(theme::ACCENT)));
        spans.push(Span::styled(
            time,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if offer && !narrow {
            // A finished work interval waits for a decision (§Focus offers).
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                " break? ",
                Style::default().fg(Color::Black).bg(theme::ACCENT),
            ));
        }
        if !narrow {
            // Pomodoro dots: banked intervals green, the live one accent. The
            // round length is a settings knob with no API yet, so no empty
            // remainder dots are drawn.
            let done = t.intervals_completed.unwrap_or(0) as usize;
            spans.push(Span::styled(
                format!(" {}", "●".repeat(done)),
                Style::default().fg(theme::SUCCESS),
            ));
            spans.push(Span::styled("●", Style::default().fg(theme::ACCENT)));
        }
    } else {
        spans.push(Span::styled("● ", bold(theme::SUCCESS)));
        spans.push(Span::styled(
            time,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if !narrow {
            if t.bound {
                if let Some(label) = t.label.as_deref() {
                    spans.push(Span::styled(
                        format!(" {}", truncate_label(label, 24)),
                        theme::muted(),
                    ));
                }
            } else {
                spans.push(Span::styled(
                    " untitled",
                    Style::default()
                        .fg(theme::MUTED)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
    }
    Some(spans)
}

fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        label.to_string()
    } else {
        let cut: String = label.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
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

    fn snap(json: serde_json::Value) -> crate::api::Timer {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn timer_cell_running_shows_green_dot_time_and_title() {
        let cell = timer_cell(
            &snap(serde_json::json!({
                "running": true, "bound": true, "label": "Read DDIA ch.7"
            })),
            272,
            false,
            false,
        )
        .expect("running renders a cell");
        assert_eq!(cell[0].style.fg, Some(theme::SUCCESS));
        assert_eq!(cell_text(Some(cell)), "● 04:32 Read DDIA ch.7");
    }

    #[test]
    fn timer_cell_unbound_reads_untitled_in_italics() {
        let cell = timer_cell(
            &snap(serde_json::json!({ "running": true, "bound": false })),
            849,
            false,
            false,
        )
        .unwrap();
        let last = cell.last().unwrap();
        assert_eq!(last.content, " untitled");
        assert!(last.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn timer_cell_paused_freezes_the_clock_muted() {
        let cell = timer_cell(
            &snap(serde_json::json!({ "running": true, "paused": true })),
            3661,
            false,
            false,
        )
        .unwrap();
        assert_eq!(cell[0].style.fg, Some(theme::WARN));
        assert_eq!(cell[1].style.fg, Some(theme::MUTED));
        assert_eq!(cell_text(Some(cell)), "‖ 1:01:01 paused");
    }

    #[test]
    fn timer_cell_idle_wears_the_amber_pill() {
        let cell = timer_cell(
            &snap(serde_json::json!({ "running": true, "idle": true })),
            9660,
            false,
            false,
        )
        .unwrap();
        let text = cell_text(Some(cell.clone()));
        assert!(text.contains("◐"), "{text}");
        assert!(text.contains(" idle "), "{text}");
        assert_eq!(cell.last().unwrap().style.bg, Some(theme::WARN));
    }

    #[test]
    fn timer_cell_focus_work_counts_pomodoro_dots() {
        let cell = timer_cell(
            &snap(serde_json::json!({
                "running": true, "mode": "focus", "phase": "work",
                "intervals_completed": 2
            })),
            1928,
            false,
            false,
        )
        .unwrap();
        let text = cell_text(Some(cell));
        // ◆ + two banked dots + the live one.
        assert!(text.starts_with("◆ "), "{text}");
        assert!(text.ends_with("●●●"), "{text}");
    }

    #[test]
    fn timer_cell_offer_pills_flag_the_finished_phase() {
        let work = timer_cell(
            &snap(serde_json::json!({
                "running": true, "mode": "focus", "phase": "work"
            })),
            3000,
            false,
            true,
        )
        .unwrap();
        assert!(cell_text(Some(work)).contains(" break? "));

        let brk = timer_cell(
            &snap(serde_json::json!({
                "running": true, "mode": "focus", "phase": "break"
            })),
            600,
            false,
            true,
        )
        .unwrap();
        assert!(cell_text(Some(brk)).contains(" work? "));
    }

    #[test]
    fn timer_cell_break_is_muted_and_not_counting() {
        let cell = timer_cell(
            &snap(serde_json::json!({
                "running": true, "mode": "focus", "phase": "break"
            })),
            252,
            false,
            false,
        )
        .unwrap();
        assert_eq!(cell_text(Some(cell)), "○ break 04:12 not counting");
    }

    #[test]
    fn timer_cell_narrow_is_glyph_and_clock_only() {
        let cell = timer_cell(
            &snap(serde_json::json!({
                "running": true, "bound": true, "label": "Read DDIA ch.7"
            })),
            3134,
            true,
            false,
        )
        .unwrap();
        assert_eq!(cell_text(Some(cell)), "● 52:14");
    }

    #[test]
    fn timer_cell_absent_is_nothing() {
        assert!(timer_cell(
            &snap(serde_json::json!({ "running": false })),
            0,
            false,
            false
        )
        .is_none());
    }

    #[test]
    fn long_labels_truncate_with_an_ellipsis() {
        assert_eq!(
            truncate_label("Implement Raft leader election end to end", 24),
            "Implement Raft leader e…"
        );
        assert_eq!(truncate_label("short", 24), "short");
    }
}
