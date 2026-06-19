use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::ApiClient;
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Shown when there is no stored refresh token. Pressing Enter dispatches
/// `Action::Login`; the app spawns the OAuth browser flow and flips `pending`.
#[derive(Default)]
pub struct Login {
    pending: bool,
}

impl Login {
    pub fn on_enter(&mut self, _api: &ApiClient, _tx: &UnboundedSender<Action>) {}

    /// The browser flow has started — wait for the callback.
    pub fn set_pending(&mut self) {
        self.pending = true;
    }

    /// The flow ended without a token (cancelled, timed out, error).
    pub fn set_idle(&mut self) {
        self.pending = false;
    }

    pub async fn handle(
        &mut self,
        _action: Action,
        _api: &ApiClient,
        _tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Center a compact panel.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(7),
                Constraint::Min(0),
            ])
            .split(area);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(48),
                Constraint::Min(0),
            ])
            .split(rows[1]);

        let body = if self.pending {
            vec![
                Line::from(""),
                Line::from(Span::styled("Waiting for the browser…", theme::focused()))
                    .alignment(Alignment::Center),
                Line::from(Span::styled(
                    "Complete the login in your browser, then return here.",
                    theme::muted(),
                ))
                .alignment(Alignment::Center),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from("You're not signed in.").alignment(Alignment::Center),
                Line::from("").alignment(Alignment::Center),
                Line::from(vec![
                    Span::raw("Press "),
                    Span::styled("Enter", theme::focused()),
                    Span::raw(" to log in."),
                ])
                .alignment(Alignment::Center),
            ]
        };

        frame.render_widget(
            Paragraph::new(body)
                .alignment(Alignment::Center)
                .block(bordered("Sign in")),
            cols[1],
        );
    }

    pub fn hints(&self) -> Line<'static> {
        if self.pending {
            return widgets::footer_hints(&[("q", "quit")]);
        }
        widgets::footer_hints(&[("⏎", "log in"), ("q", "quit")])
    }
}
