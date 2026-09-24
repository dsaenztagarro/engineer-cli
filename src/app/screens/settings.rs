//! Timer settings screen — the per-user knobs, read-only (the web is the one writer).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, ApiError, TimerSettings};
use crate::app::action::Action;
use crate::messages;
use crate::ui::notify::Level;
use crate::ui::panel::{render_panel_state, PanelFailure, PanelState};
use crate::ui::{layout::bordered, theme, widgets};

#[derive(Default)]
pub struct Settings {
    settings: Option<TimerSettings>,
    loading: bool,
    failure: Option<PanelFailure>,
}

impl Settings {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        spawn_load(api, tx);
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::SettingsLoaded(s) => {
                self.loading = false;
                self.failure = None;
                self.settings = Some(*s);
            }
            Action::SettingsLoadFailed(reason) => {
                self.loading = false;
                self.failure = Some(PanelFailure {
                    headline: messages::load_failed("settings"),
                    reason,
                    retry_key: "r",
                    cached: false,
                });
            }
            Action::SettingsReload => {
                self.loading = true;
                spawn_load(api, tx);
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Your timer · read-only — edit on the web");

        let Some(s) = self.settings.as_ref() else {
            let state = if let Some(f) = &self.failure {
                PanelState::Failed(f.clone())
            } else {
                PanelState::Loading
            };
            render_panel_state(frame, area, block, &state);
            return;
        };

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let row = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("  {label:<34}"), theme::muted()),
                Span::styled(value, Style::default().add_modifier(Modifier::BOLD)),
            ])
        };
        let onoff = |b: bool| if b { "on" } else { "off" }.to_string();

        let lines = vec![
            Line::from(""),
            row("default mode", s.timer_mode.clone()),
            row(
                "focus · work / short / long break",
                format!(
                    "{}m · {}m · {}m",
                    s.focus_work_minutes, s.focus_short_break_minutes, s.focus_long_break_minutes
                ),
            ),
            row(
                "focus · long break every",
                format!("{}th break", s.focus_long_break_every),
            ),
            row(
                "idle guard · threshold",
                format!(
                    "{} · {}m without input",
                    onoff(s.idle_guard_enabled),
                    s.idle_threshold_minutes
                ),
            ),
            row("default reclaim action", s.idle_default_reclaim.clone()),
            row(
                "audit fences · long / short",
                format!("≥ {}h · < {}s", s.audit_long_hours, s.audit_short_seconds),
            ),
            row("audit badge", onoff(s.audit_badge_enabled)),
            row("overrun ping", onoff(s.overrun_ping_enabled)),
            Line::from(""),
            Line::from(Span::styled(
                "  These values drive every timer surface here. Editing lives on the web:",
                theme::muted(),
            )),
            Line::from(Span::styled("  engineer › Settings", theme::muted())),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }

    pub fn hints(&self) -> Line<'static> {
        widgets::footer_hints(&[("r", "refresh"), ("h", "home")])
    }
}

fn spawn_load(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.timer_settings().await {
            Ok(s) => {
                let _ = tx.send(Action::SettingsLoaded(Box::new(s)));
            }
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(e) => {
                let _ = tx.send(Action::SettingsLoadFailed(messages::fail_reason(
                    api.host(),
                    &e,
                )));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use ratatui::{backend::TestBackend, Terminal};
    use tokio::sync::mpsc;

    fn settings_json() -> TimerSettings {
        serde_json::from_value(serde_json::json!({
            "timer_mode": "stopwatch",
            "focus_work_minutes": 50,
            "focus_short_break_minutes": 10,
            "focus_long_break_minutes": 20,
            "focus_long_break_every": 4,
            "idle_guard_enabled": true,
            "idle_threshold_minutes": 15,
            "idle_default_reclaim": "trim",
            "audit_long_hours": 6,
            "audit_short_seconds": 60,
            "audit_badge_enabled": true,
            "overrun_ping_enabled": true
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn loaded_settings_render_every_knob() {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));

        let mut screen = Settings::default();
        screen
            .handle(Action::SettingsLoaded(Box::new(settings_json())), &api, &tx)
            .await;

        let mut terminal = Terminal::new(TestBackend::new(84, 26)).unwrap();
        terminal
            .draw(|frame| screen.render(frame, frame.area()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("50m · 10m · 20m"), "focus durations");
        assert!(content.contains("4th break"), "long break every");
        assert!(content.contains("15m without input"), "idle threshold");
        assert!(content.contains("≥ 6h · < 60s"), "audit fences");
        assert!(content.contains("edit on the web"), "web pointer");
    }

    fn render_to_string(screen: &mut Settings) -> String {
        let mut terminal = Terminal::new(TestBackend::new(84, 26)).unwrap();
        terminal
            .draw(|frame| screen.render(frame, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[tokio::test]
    async fn a_pending_read_renders_loading_not_a_failure() {
        let mut screen = Settings {
            loading: true,
            ..Settings::default()
        };
        let content = render_to_string(&mut screen);
        assert!(content.contains("loading…"), "{content}");
        assert!(!content.contains('✖'), "{content}");
    }

    #[tokio::test]
    async fn a_successful_load_clears_a_prior_failure() {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));

        let mut screen = Settings::default();
        screen
            .handle(Action::SettingsLoadFailed("boom".into()), &api, &tx)
            .await;
        screen
            .handle(Action::SettingsLoaded(Box::new(settings_json())), &api, &tx)
            .await;
        assert!(screen.failure.is_none());
    }

    async fn first_action_for_status(status: u16) -> Action {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/timer/settings"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_load(&api, &tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a message arrives")
            .expect("channel open")
    }

    #[tokio::test]
    async fn a_401_from_the_settings_read_routes_to_reauth() {
        let got = first_action_for_status(401).await;
        assert!(matches!(got, Action::SessionExpired), "got {got:?}");
    }

    #[tokio::test]
    async fn a_failed_settings_read_reports_itself_as_a_failure() {
        let got = first_action_for_status(500).await;
        assert!(matches!(got, Action::SettingsLoadFailed(_)), "got {got:?}");
    }

    #[tokio::test]
    async fn a_load_failure_renders_the_tier2_panel_not_a_blank() {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));

        let mut screen = Settings::default();
        screen
            .handle(
                Action::SettingsLoadFailed("identity.test → HTTP 500".into()),
                &api,
                &tx,
            )
            .await;
        assert!(screen.failure.is_some(), "the failure is recorded");
        assert!(screen.settings.is_none(), "no knobs invented on failure");

        let mut terminal = Terminal::new(TestBackend::new(84, 26)).unwrap();
        terminal
            .draw(|frame| screen.render(frame, frame.area()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("✖ couldn't load settings"),
            "loud failure: {content}"
        );
        assert!(content.contains("HTTP 500"), "names the reason: {content}");
    }
}
