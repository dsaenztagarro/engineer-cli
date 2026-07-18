use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::ApiClient;
use crate::app::action::Action;
use crate::ui::blocking::{render_blocking, Blocking, Recovery};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Shown when there is no stored refresh token, or when the session must be
/// re-established. `Idle`/`Pending` are the plain sign-in prompt; `ServerError`
/// and `Expired` are the design's Tier-3 blocking states (§SIGN IN · SERVER
/// ERROR): the whole screen carries the failure and its one recovery key.
#[derive(Default)]
enum State {
    #[default]
    Idle,
    Pending,
    /// The identity server was unreachable / 5xx — the flow can't start.
    ServerError(String),
    /// A 401 invalidated the stored session — re-authenticate.
    Expired,
}

#[derive(Default)]
pub struct Login {
    state: State,
}

impl Login {
    pub fn on_enter(&mut self, _api: &ApiClient, _tx: &UnboundedSender<Action>) {}

    /// The browser flow has started — wait for the callback.
    pub fn set_pending(&mut self) {
        self.state = State::Pending;
    }

    /// The flow ended without a token (cancelled, timed out, keyring error).
    pub fn set_idle(&mut self) {
        self.state = State::Idle;
    }

    /// The identity server couldn't be reached (Tier 3, retry).
    pub fn set_server_error(&mut self, reason: impl Into<String>) {
        self.state = State::ServerError(reason.into());
    }

    /// A 401 invalidated the session (Tier 3, re-auth).
    pub fn set_expired(&mut self) {
        self.state = State::Expired;
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
        // The Tier-3 states take the whole content area.
        match &self.state {
            State::ServerError(reason) => {
                render_blocking(
                    frame,
                    area,
                    &Blocking {
                        title: "Sign in".into(),
                        headline: "can't reach the identity server".into(),
                        detail: vec![
                            reason.clone(),
                            "the sign-in flow can't start — nothing was changed.".into(),
                        ],
                        recovery: Recovery::Retry,
                        footnote: None,
                    },
                );
                return;
            }
            State::Expired => {
                render_blocking(
                    frame,
                    area,
                    &Blocking {
                        title: "Sign in".into(),
                        headline: "session expired".into(),
                        detail: vec!["your session is no longer valid — sign in again.".into()],
                        recovery: Recovery::ReAuth,
                        footnote: None,
                    },
                );
                return;
            }
            State::Idle | State::Pending => {}
        }

        // Idle / Pending: the compact centred sign-in prompt.
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

        let body = if matches!(self.state, State::Pending) {
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
        match self.state {
            State::Pending => widgets::footer_hints(&[("q", "quit")]),
            State::ServerError(_) => widgets::footer_hints(&[("⏎", "retry"), ("q", "quit")]),
            State::Expired => widgets::footer_hints(&[("⏎", "sign in"), ("q", "quit")]),
            State::Idle => widgets::footer_hints(&[("⏎", "log in"), ("q", "quit")]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(l: &mut Login) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| l.render(f, f.area())).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn idle_is_the_plain_sign_in_prompt() {
        let mut l = Login::default();
        let text = render(&mut l);
        assert!(text.contains("You're not signed in."), "{text}");
        assert!(!text.contains("✖"), "no blocking bar when idle: {text}");
    }

    #[test]
    fn a_server_error_renders_the_tier3_blocking_screen() {
        let mut l = Login::default();
        l.set_server_error("identity.dev → HTTP 500");
        let text = render(&mut l);
        assert!(
            text.contains("✖ can't reach the identity server"),
            "loud blocking headline: {text}"
        );
        assert!(text.contains("HTTP 500"), "names the cause: {text}");
        assert!(text.contains("retry"), "offers retry: {text}");
    }

    #[test]
    fn an_expired_session_asks_to_sign_in_again() {
        let mut l = Login::default();
        l.set_expired();
        let text = render(&mut l);
        assert!(text.contains("session expired"), "{text}");
        assert!(text.contains("sign in"), "re-auth recovery: {text}");
    }
}
