use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::api::{ActivityCreate, ApiClient, FieldError};
use crate::app::action::Action;
use crate::app::screens::{ScreenKind, ScreenMode};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

const FIELDS: &[&str] = &["title", "kind", "duration", "notes"];

pub struct ActivityNew {
    title: Input,
    kind: Input,
    duration: Input,
    notes: Input,
    focus: usize,
    mode: ScreenMode,
    errors: Vec<FieldError>,
    pending: bool,
}

impl Default for ActivityNew {
    fn default() -> Self {
        Self {
            title: Input::default(),
            kind: Input::new("study".into()),
            duration: Input::new("30".into()),
            notes: Input::default(),
            focus: 0,
            mode: ScreenMode::Normal,
            errors: vec![],
            pending: false,
        }
    }
}

impl ActivityNew {
    pub fn on_enter(&mut self, _api: &ApiClient, _tx: &UnboundedSender<Action>) {}

    pub fn mode(&self) -> ScreenMode {
        self.mode
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::ActivityFieldNext => {
                self.focus = (self.focus + 1) % FIELDS.len();
            }
            Action::ActivityFieldPrev => {
                self.focus = (self.focus + FIELDS.len() - 1) % FIELDS.len();
            }
            Action::ActivityEnterInsert => self.mode = ScreenMode::Insert,
            Action::ActivityLeaveInsert => self.mode = ScreenMode::Normal,
            Action::ActivityKey(key) => {
                let evt = crossterm::event::Event::Key(key);
                match self.focus {
                    0 => {
                        self.title.handle_event(&evt);
                    }
                    1 => {
                        self.kind.handle_event(&evt);
                    }
                    2 => {
                        // Allow only digits for duration.
                        if let KeyCode::Char(c) = key.code {
                            if !c.is_ascii_digit() {
                                return None;
                            }
                        }
                        self.duration.handle_event(&evt);
                    }
                    3 => {
                        self.notes.handle_event(&evt);
                    }
                    _ => {}
                }
            }
            Action::ActivitySubmit => {
                if self.pending {
                    return Some((Level::Warning, "already submitting…".into()));
                }
                let body = ActivityCreate {
                    title: self.title.value().to_string(),
                    kind: opt_str(self.kind.value()),
                    duration_minutes: self.duration.value().parse().ok(),
                    started_at: Some(jiff::Timestamp::now()),
                    notes_generated: opt_str(self.notes.value()),
                    ..Default::default()
                };
                if body.title.trim().is_empty() {
                    self.errors = vec![FieldError {
                        field: "title".into(),
                        detail: "can't be blank".into(),
                    }];
                    return Some((Level::Warning, "title required".into()));
                }
                self.pending = true;
                let api = api.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match api.create_activity(&body).await {
                        Ok(_) => {
                            let _ = tx.send(Action::ActivityCreated);
                            let _ = tx.send(Action::Notify {
                                level: Level::Success,
                                text: "activity logged".into(),
                            });
                            let _ = tx.send(Action::Goto(ScreenKind::Home));
                        }
                        Err(e) => {
                            let errors = e.field_errors().to_vec();
                            let _ = tx.send(Action::ActivityFailed {
                                errors,
                                detail: e.to_string(),
                            });
                        }
                    }
                });
            }
            Action::ActivityCreated => {
                self.pending = false;
            }
            Action::ActivityFailed { errors, detail } => {
                self.pending = false;
                self.errors = errors;
                return Some((Level::Error, detail));
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let outer = bordered("New activity");
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(inner);

        let mode = match self.mode {
            ScreenMode::Normal => "NORMAL",
            ScreenMode::Insert => "INSERT",
        };

        for (i, name) in FIELDS.iter().enumerate() {
            let value = match i {
                0 => self.title.value(),
                1 => self.kind.value(),
                2 => self.duration.value(),
                3 => self.notes.value(),
                _ => "",
            };
            let is_focus = i == self.focus;
            let err = self.errors.iter().find(|e| e.field == *name);
            let title = format!(" {} {} ", name, if err.is_some() { "✗" } else { "" });
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(if is_focus {
                    theme::focused()
                } else {
                    ratatui::style::Style::default().fg(theme::BORDER)
                })
                .title(Span::styled(
                    title,
                    if err.is_some() {
                        ratatui::style::Style::default().fg(theme::DANGER)
                    } else {
                        theme::muted()
                    },
                ));
            let body = if let Some(e) = err {
                Line::from(vec![
                    Span::raw(value.to_string()),
                    Span::raw("    "),
                    Span::styled(
                        format!("⚠ {}", e.detail),
                        ratatui::style::Style::default().fg(theme::DANGER),
                    ),
                ])
            } else {
                Line::from(value.to_string())
            };
            frame.render_widget(Paragraph::new(body).block(block), chunks[i]);
        }

        let footer_text = format!(
            "-- {} --     j/k field · i edit · Esc normal · :w / <Space>s submit",
            mode
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(footer_text, theme::muted()))),
            chunks[4],
        );
    }

    pub fn hints(&self) -> Line<'static> {
        if matches!(self.mode, ScreenMode::Insert) {
            return Line::from(Span::styled(
                "INSERT · type · Esc to leave",
                theme::focused(),
            ));
        }
        widgets::footer_hints(&[
            ("j/k", "field"),
            ("i", "edit"),
            ("⎵s", "submit"),
            ("Esc", "back"),
        ])
    }
}

fn opt_str(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}
