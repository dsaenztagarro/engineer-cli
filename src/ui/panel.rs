//! Tier-2 of the error/notification model (design-system.dc.html §ERROR &
//! NOTIFICATION MODEL): the **inline panel state**. When a bordered region's
//! data didn't load, its body — the space that would hold rows — shows the
//! reason and a retry key, while the rest of the screen stays live. The model's
//! one hard rule is drawn here: **empty** (no rows, calm/muted) is a different
//! thing from **failed** (couldn't fetch, loud/red), and the two never collapse
//! into each other.
//!
//! This is pure presentation. A screen keeps its own `Vec`/aggregate + loading
//! flag + optional [`PanelFailure`], computes a [`PanelState`] at render time,
//! and hands it here; a region that *does* have rows renders its own List/Table
//! as before. There is deliberately no generic `LoadState<T>` wrapper — the
//! screens are too heterogeneous (some map one read to many panels) for a single
//! container to fit without fighting the `ListState` borrow.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::ui::theme;

/// What a bordered region shows when it has no rows to draw.
pub enum PanelState {
    /// The first read is in flight and nothing is cached yet.
    Loading,
    /// The read succeeded with zero rows — a calm, muted invitation. `hint`
    /// is the screen-specific nudge ("Log one with `a`"); `None` falls back to
    /// a bare "nothing here yet".
    Empty { hint: Option<String> },
    /// The read failed — loud and red, with the reason and a recovery key.
    Failed(PanelFailure),
}

/// The Tier-2 failure a region renders instead of its rows. Built from
/// [`crate::messages`] so the wording matches the Tier-1 tile and the headless
/// stderr for the same outcome (§C). `Clone` so a screen can hand a
/// `PanelState::Failed` to the renderer without moving it out of `&self`.
#[derive(Clone)]
pub struct PanelFailure {
    /// The loud headline, no glyph — e.g. `messages::load_failed("books")`.
    pub headline: String,
    /// The muted cause line — `messages::fail_reason(host, &err)`.
    pub reason: String,
    /// The key that re-runs the read (almost always `"r"`).
    pub retry_key: &'static str,
    /// Whether an `o open last-cached` affordance is offered. Held `false`
    /// everywhere until a read actually keeps a cache — advertising a cache
    /// that isn't there would violate the honesty rule (§0·4).
    pub cached: bool,
}

/// Render `state` as the body of `block`, filling `area`. Call this only when
/// the region has no rows; a live region renders its own widget instead.
pub fn render_panel_state(
    frame: &mut Frame,
    area: Rect,
    block: Block<'static>,
    state: &PanelState,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = match state {
        PanelState::Loading => vec![Line::styled("loading…", theme::muted())],
        PanelState::Empty { hint } => vec![Line::styled(
            hint.clone().unwrap_or_else(|| "nothing here yet".into()),
            theme::muted(),
        )],
        PanelState::Failed(f) => failure_lines(f),
    };

    // Vertically centre the 1–3 body lines in the region, matching the mock's
    // padded-centre placement.
    let content_h = lines.len() as u16;
    let top = inner.height.saturating_sub(content_h) / 2;
    let body = Rect {
        x: inner.x,
        y: inner.y.saturating_add(top),
        width: inner.width,
        height: content_h.min(inner.height),
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
}

fn failure_lines(f: &PanelFailure) -> Vec<Line<'static>> {
    let danger = Style::default()
        .fg(theme::DANGER)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);

    let mut recovery: Vec<Span<'static>> = vec![
        Span::styled(format!(" {} ", f.retry_key), key),
        Span::styled("retry", theme::muted()),
    ];
    if f.cached {
        recovery.push(Span::styled("  ·  ", theme::muted()));
        recovery.push(Span::styled(" o ", key));
        recovery.push(Span::styled("open last-cached", theme::muted()));
    }

    vec![
        Line::styled(format!("✖ {}", f.headline), danger),
        Line::styled(f.reason.clone(), theme::muted()),
        Line::from(recovery),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Borders;
    use ratatui::Terminal;

    fn draw(state: &PanelState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        terminal
            .draw(|f| {
                let block = Block::default().borders(Borders::ALL).title(" Books ");
                render_panel_state(f, f.area(), block, state);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn failed_is_loud_with_reason_and_retry_key() {
        let text = draw(&PanelState::Failed(PanelFailure {
            headline: "couldn't load books".into(),
            reason: "identity.dev → HTTP 500".into(),
            retry_key: "r",
            cached: false,
        }));
        assert!(text.contains("✖ couldn't load books"), "{text}");
        assert!(text.contains("identity.dev → HTTP 500"), "{text}");
        assert!(text.contains("retry"), "{text}");
        // `o open last-cached` stays hidden while no cache backs it.
        assert!(!text.contains("open last-cached"), "{text}");
    }

    #[test]
    fn cached_failure_offers_open_last_cached() {
        let text = draw(&PanelState::Failed(PanelFailure {
            headline: "couldn't load books".into(),
            reason: "offline".into(),
            retry_key: "r",
            cached: true,
        }));
        assert!(text.contains("open last-cached"), "{text}");
    }

    #[test]
    fn empty_is_calm_and_shows_the_hint() {
        let text = draw(&PanelState::Empty {
            hint: Some("Log one with `a`".into()),
        });
        assert!(text.contains("Log one with `a`"), "{text}");
        // Empty is never dressed up as a failure.
        assert!(!text.contains("✖"), "{text}");
    }

    #[test]
    fn loading_reads_as_loading() {
        assert!(draw(&PanelState::Loading).contains("loading…"));
    }
}
