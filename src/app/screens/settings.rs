//! Timer settings, view-only (timer.dc.html §Timer settings): the CLI reads
//! the per-user knobs and reflects them everywhere ("your 50m work", "your
//! long fence") — it does not edit them. Editing lives on the web, so there
//! is exactly one writer and no divergence.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, TimerSettings};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

#[derive(Default)]
pub struct Settings {
    settings: Option<TimerSettings>,
    loading: bool,
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
                self.settings = Some(*s);
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
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(s) = self.settings.as_ref() else {
            let text = if self.loading { "loading…" } else { "" };
            frame.render_widget(Paragraph::new(text), inner);
            return;
        };

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
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("settings load failed: {e}"),
                });
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
}
