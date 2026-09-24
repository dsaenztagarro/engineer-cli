//! The Tier-2 inline panel state of the error model (ADR 0001).
//!
//! There is no generic `LoadState<T>` wrapper: the screens are too heterogeneous
//! (some map one read to many panels) for one container to fit without fighting
//! the `ListState` borrow.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::ui::theme;

pub enum PanelState {
    Loading,
    Empty { hint: Option<String> },
    Failed(PanelFailure),
}

/// Build `headline` and `reason` from [`crate::messages`] (ADR 0001).
#[derive(Clone)]
pub struct PanelFailure {
    pub headline: String,
    pub reason: String,
    pub retry_key: &'static str,
    /// True only where a read really keeps a cache: offering one that isn't
    /// there would be a lie.
    pub cached: bool,
}

/// Only for a region with no rows; a live region renders its own widget.
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
        assert!(!text.contains("✖"), "{text}");
    }

    #[test]
    fn loading_reads_as_loading() {
        assert!(draw(&PanelState::Loading).contains("loading…"));
    }

    #[test]
    fn an_empty_region_without_a_hint_reads_nothing_here_yet() {
        let text = draw(&PanelState::Empty { hint: None });
        assert!(text.contains("nothing here yet"), "{text}");
    }
}
