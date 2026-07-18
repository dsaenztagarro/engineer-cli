//! Tier-3 of the error/notification model (design-system.dc.html §ERROR &
//! NOTIFICATION MODEL, reference: §SIGN IN · SERVER ERROR): the **blocking
//! screen**. When the whole content area is meaningless without something that
//! failed — auth down / 5xx, an expired session (401 → re-auth), missing config
//! — the screen fills with the failure and offers the one recovery action.
//!
//! This is the loudest, rarest tier and deliberately has few owners: only Login
//! (its own read is the session) and the global 401→re-auth path route here.
//! Every other screen's failure is a Tier-2 panel (`ui::panel`) while the rest
//! of the screen stays live.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{layout::bordered, theme};

/// The one recovery a blocking screen offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// `⏎ retry` — re-run the thing that failed (a 5xx that may pass next time).
    Retry,
    /// `⏎ sign in` — the session expired; re-authenticate.
    ReAuth,
    /// No forward move — only `q` quits (e.g. missing config the app can't fix).
    /// The atom supports it; the missing-config *trigger* isn't wired yet (no
    /// config-validation path exists), so this variant has no non-test caller.
    #[allow(dead_code)]
    QuitOnly,
}

impl Recovery {
    /// The `⏎` action label, or `None` when there's nothing but quit.
    fn enter_label(self) -> Option<&'static str> {
        match self {
            Recovery::Retry => Some("retry"),
            Recovery::ReAuth => Some("sign in"),
            Recovery::QuitOnly => None,
        }
    }
}

/// A whole-screen blocking failure. `headline` is the loud one-line summary
/// (rendered as a full-width danger bar); `detail` are the muted lines beneath
/// (the cause, and any "nothing was changed" reassurance); `footnote` is an
/// optional dim diagnostic tail.
pub struct Blocking {
    pub title: String,
    pub headline: String,
    pub detail: Vec<String>,
    pub recovery: Recovery,
    pub footnote: Option<String>,
}

/// Render `b` filling `area` — a centred bordered panel, the danger headline
/// bar, the muted detail, and the recovery line.
pub fn render_blocking(frame: &mut Frame, area: Rect, b: &Blocking) {
    // Centre a panel wide enough for the headline, matching the Login layout.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(11),
            Constraint::Min(0),
        ])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(60),
            Constraint::Min(0),
        ])
        .split(rows[1]);

    let danger_bar = Style::default()
        .fg(Color::Black)
        .bg(theme::DANGER)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(format!(" ✖ {} ", b.headline), danger_bar))
            .alignment(Alignment::Center),
        Line::from(""),
    ];
    for d in &b.detail {
        lines
            .push(Line::from(Span::styled(d.clone(), theme::muted())).alignment(Alignment::Center));
    }
    lines.push(Line::from(""));
    lines.push(recovery_line(b.recovery));
    if let Some(f) = &b.footnote {
        lines.push(Line::from(""));
        lines.push(
            Line::from(Span::styled(f.clone(), Style::default().fg(theme::BORDER)))
                .alignment(Alignment::Center),
        );
    }

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(bordered(b.title.clone())),
        cols[1],
    );
}

fn recovery_line(recovery: Recovery) -> Line<'static> {
    let key = theme::focused();
    let mut spans = vec![Span::raw("Press ")];
    if let Some(label) = recovery.enter_label() {
        spans.push(Span::styled("Enter", key));
        spans.push(Span::raw(format!(" to {label}  ·  ")));
    }
    spans.push(Span::styled("q", key));
    spans.push(Span::raw(" to quit"));
    Line::from(spans).alignment(Alignment::Center)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn draw(b: &Blocking) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| render_blocking(f, f.area(), b)).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn server_error_offers_retry() {
        let text = draw(&Blocking {
            title: "Sign in".into(),
            headline: "can't reach the identity server".into(),
            detail: vec![
                "identity.dev → HTTP 500".into(),
                "nothing was changed.".into(),
            ],
            recovery: Recovery::Retry,
            footnote: None,
        });
        assert!(text.contains("✖ can't reach the identity server"), "{text}");
        assert!(text.contains("HTTP 500"), "{text}");
        assert!(text.contains("Enter"), "offers the retry key: {text}");
        assert!(text.contains("retry"), "{text}");
    }

    #[test]
    fn reauth_says_sign_in() {
        let text = draw(&Blocking {
            title: "Sign in".into(),
            headline: "session expired".into(),
            detail: vec![],
            recovery: Recovery::ReAuth,
            footnote: None,
        });
        assert!(text.contains("sign in"), "re-auth label: {text}");
    }

    #[test]
    fn quit_only_offers_no_enter_action() {
        let text = draw(&Blocking {
            title: "Sign in".into(),
            headline: "missing config".into(),
            detail: vec![],
            recovery: Recovery::QuitOnly,
            footnote: None,
        });
        assert!(text.contains("quit"), "{text}");
        assert!(!text.contains("Enter"), "no forward move offered: {text}");
    }
}
