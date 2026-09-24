//! The Tier-3 blocking screen of the error model (ADR 0001).

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{layout::bordered, theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Retry,
    ReAuth,
    // No caller yet: nothing validates the config, so the missing-config
    // trigger this recovery exists for is not wired.
    #[allow(dead_code)]
    QuitOnly,
}

impl Recovery {
    fn enter_label(self) -> Option<&'static str> {
        match self {
            Recovery::Retry => Some("retry"),
            Recovery::ReAuth => Some("sign in"),
            Recovery::QuitOnly => None,
        }
    }
}

pub struct Blocking {
    pub title: String,
    pub headline: String,
    pub detail: Vec<String>,
    pub recovery: Recovery,
    pub footnote: Option<String>,
}

pub fn render_blocking(frame: &mut Frame, area: Rect, b: &Blocking) {
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
        .fg(theme::INK_ON_FILL)
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

    #[test]
    fn only_the_login_screen_raises_a_blocking_screen() {
        fn callers(dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    callers(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.ends_with("ui/blocking.rs")
                    && std::fs::read_to_string(&path)
                        .unwrap()
                        .contains("render_blocking")
                {
                    out.push(path.display().to_string());
                }
            }
        }
        let mut found = Vec::new();
        callers(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("app/screens/login.rs"), "{found:?}");
    }
}
