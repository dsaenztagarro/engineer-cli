//! Connect — the git-source connect flow over the assisted-capture sources
//! (`/api/v1/capture/sources`, ADR 0035; assisted-capture.dc.html §Connect · git
//! source). Reachable from the Inbox screen via `c` (the design's footer key):
//! the inbox triages the drafts, this is where a source is opted in so those
//! drafts appear in the first place.
//!
//! One screen, a list of the capture sources (git / calendar) with their connect
//! state, and three verbs behind a modal prompt:
//!
//!   connect     — `c` opens the trust statement (`reads` / `never_reads` /
//!                 `promise`, rendered **verbatim before connecting** — the
//!                 brief's hard requirement), then a confirm. The git source
//!                 takes no body; the calendar captures a feed URL first.
//!   disconnect  — `d` arms the confirm; disconnect turns the source *off*
//!                 without deleting captured drafts (disconnect ≠ delete).
//!   sync        — `s` enqueues a scan for a connected source.
//!
//! When the git source has no GitHub connection it is not `connectable`: `c`
//! renders the server's **requirement pointer** honestly (the detail + the web
//! URL — "connect GitHub on the web first") instead of offering a connect that
//! would only 422. GitHub OAuth is web-only (engineer ADR 0018); the CLI never
//! fakes a second auth path.
//!
//! The verbs are **live-only** — connecting needs the server, so an offline
//! gesture can't synthesize an opt-in that never happened. The honest move is a
//! clear offline refusal, the same deviation the triage verbs took (#94, epic
//! #118 decision log).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, ApiError, CaptureSource};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// The live-only refusal: connecting is a server write whose outcome (the opt-in
/// flag, or the requirement `422`) can't be synthesized offline. Recorded on
/// epic #118, the same shape as the triage verbs' offline refusal.
const OFFLINE_REFUSAL: &str = "offline — connecting needs the server; retry online";

/// A modal prompt open over the sources list.
enum Prompt {
    /// The trust statement + a confirm, for a `connectable` source. `feed` is
    /// `Some` (the captured URL) for a source that takes a feed URL (calendar),
    /// `None` for one that takes no body (git).
    Connect {
        source: CaptureSource,
        feed: Option<String>,
    },
    /// The honest requirement pointer — the git source can't connect until
    /// GitHub is connected on the web. No connect is offered; only dismiss.
    Requirement { source: CaptureSource },
    /// The disconnect confirm — drafts survive, so the copy says so.
    Disconnect { source: CaptureSource },
}

/// One connect/disconnect/sync verb, carrying what its server call needs.
enum Verb {
    Connect { key: String, feed: Option<String> },
    Disconnect { key: String },
    Sync { key: String },
}

#[derive(Default)]
pub struct Connect {
    sources: Vec<CaptureSource>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    prompt: Option<Prompt>,
    /// A verb is in flight — guards a second fire before the re-read.
    in_flight: bool,
}

impl Connect {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        tokio::spawn(async move {
            match api.list_capture_sources().await {
                Ok(sources) => {
                    let _ = tx.send(Action::ConnectLoaded(sources));
                }
                Err(e) => {
                    let _ = tx.send(Action::ConnectLoadFailed(format!(
                        "sources load failed: {e}"
                    )));
                }
            }
        });
    }

    /// The feed-URL capture (calendar) owns keys before the global keymap while
    /// open; the confirm prompts own their submit/cancel keys.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        let prompt = self.prompt.as_ref()?;
        match prompt {
            // The calendar feed-URL field swallows every key so the URL is never
            // disturbed by the global keymap.
            Prompt::Connect { feed: Some(_), .. } => Some(match key.code {
                KeyCode::Enter => Action::ConnectPromptSubmit,
                KeyCode::Esc => Action::ConnectPromptCancel,
                KeyCode::Backspace => Action::ConnectFeedBackspace,
                KeyCode::Char(c) => Action::ConnectFeedInput(c),
                _ => return None,
            }),
            Prompt::Connect { feed: None, .. } | Prompt::Disconnect { .. } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => Some(Action::ConnectPromptSubmit),
                KeyCode::Esc | KeyCode::Char('n') => Some(Action::ConnectPromptCancel),
                _ => None,
            },
            Prompt::Requirement { .. } => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h') => {
                    Some(Action::ConnectPromptCancel)
                }
                _ => None,
            },
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::ConnectLoaded(sources) => {
                self.sources = sources;
                self.loading = false;
                self.error = None;
                self.in_flight = false;
                self.clamp_selection();
            }
            Action::ConnectLoadFailed(e) => {
                self.loading = false;
                self.error = Some(e.clone());
                return Some((Level::Error, e));
            }
            Action::RefreshConnect => {
                self.loading = true;
                self.prompt = None;
                self.in_flight = false;
                self.fetch(api, tx);
            }
            Action::ConnectMove(delta) => {
                if self.prompt.is_none() {
                    self.move_selection(delta);
                }
            }
            Action::ConnectBegin => {
                if self.prompt.is_some() {
                    return None;
                }
                if let Some(source) = self.current().cloned() {
                    if source.connected {
                        return Some((
                            Level::Info,
                            format!("{} is already connected · d to disconnect", source.name),
                        ));
                    }
                    self.prompt = Some(if !source.connectable {
                        Prompt::Requirement { source }
                    } else {
                        let feed = source.wants_feed_url().then(String::new);
                        Prompt::Connect { source, feed }
                    });
                }
            }
            Action::ConnectDisconnectBegin => {
                if self.prompt.is_some() {
                    return None;
                }
                if let Some(source) = self.current().cloned() {
                    if source.connected {
                        self.prompt = Some(Prompt::Disconnect { source });
                    } else {
                        return Some((Level::Info, format!("{} isn't connected", source.name)));
                    }
                }
            }
            Action::ConnectSync => {
                if self.prompt.is_some() {
                    return None;
                }
                if let Some(source) = self.current().cloned() {
                    if source.connected {
                        self.fire(Verb::Sync { key: source.key }, api, tx);
                    } else {
                        return Some((
                            Level::Info,
                            format!("connect {} before syncing it", source.name),
                        ));
                    }
                }
            }
            Action::ConnectPromptSubmit => match self.prompt.take() {
                Some(Prompt::Connect { source, feed }) => {
                    self.fire(
                        Verb::Connect {
                            key: source.key,
                            feed,
                        },
                        api,
                        tx,
                    );
                }
                Some(Prompt::Disconnect { source }) => {
                    self.fire(Verb::Disconnect { key: source.key }, api, tx);
                }
                // The requirement pointer has no submit — dismissing is all.
                other => self.prompt = other,
            },
            Action::ConnectPromptCancel => self.prompt = None,
            Action::ConnectFeedInput(c) => {
                if let Some(Prompt::Connect { feed: Some(s), .. }) = self.prompt.as_mut() {
                    s.push(c);
                }
            }
            Action::ConnectFeedBackspace => {
                if let Some(Prompt::Connect { feed: Some(s), .. }) = self.prompt.as_mut() {
                    s.pop();
                }
            }
            Action::ConnectActionFailed => self.in_flight = false,
            _ => {}
        }
        None
    }

    /// Fire a live-only verb against the server and, on a resolved outcome,
    /// re-read the sources. A transport failure is the honest offline refusal.
    fn fire(&mut self, verb: Verb, api: &ApiClient, tx: &UnboundedSender<Action>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let (api, tx) = (api.clone(), tx.clone());
        tokio::spawn(async move {
            let outcome: Result<String, ApiError> = match verb {
                Verb::Connect { key, feed } => api
                    .connect_capture_source(&key, feed.as_deref())
                    .await
                    .map(|s| format!("connected · {} — drafts flow into your inbox", s.name)),
                Verb::Disconnect { key } => api
                    .disconnect_capture_source(&key)
                    .await
                    .map(|s| format!("disconnected · {} — captured drafts kept", s.name)),
                Verb::Sync { key } => api
                    .sync_capture_source(&key)
                    .await
                    .map(|q| format!("sync queued · {}", q.key)),
            };
            match outcome {
                Ok(text) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Success,
                        text,
                    });
                    let _ = tx.send(Action::RefreshConnect);
                }
                Err(ApiError::Transport(_)) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: OFFLINE_REFUSAL.to_string(),
                    });
                    let _ = tx.send(Action::ConnectActionFailed);
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: problem_text(&e),
                    });
                    let _ = tx.send(Action::ConnectActionFailed);
                }
            }
        });
    }

    fn current(&self) -> Option<&CaptureSource> {
        self.sources.get(self.selected)
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.sources.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    fn clamp_selection(&mut self) {
        let len = self.sources.len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self.prompt.as_ref() {
            Some(Prompt::Connect { source, feed }) => {
                render_connect(frame, area, source, feed.as_deref())
            }
            Some(Prompt::Requirement { source }) => render_requirement(frame, area, source),
            Some(Prompt::Disconnect { source }) => render_disconnect(frame, area, source),
            None => self.render_list(frame, area),
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let block = bordered("Connect · sources");

        if self.sources.is_empty() {
            let body = if self.loading && self.error.is_none() {
                "loading…".to_string()
            } else if let Some(e) = &self.error {
                e.clone()
            } else {
                "no capture sources".to_string()
            };
            frame.render_widget(Paragraph::new(body).block(block), area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(block.inner(area));
        frame.render_widget(block, area);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "point engineer at what you already do",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ·  drafts appear in your inbox", theme::muted()),
            ])),
            chunks[0],
        );

        let rows: Vec<Row> = self.sources.iter().map(source_row).collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(16), // state pill
                Constraint::Length(20), // source name
                Constraint::Min(20),    // reads / requirement
            ],
        )
        .header(Row::new(vec!["STATE", "SOURCE", "READS"]).style(theme::header()))
        .row_highlight_style(theme::selection())
        .highlight_symbol("▌ ");
        let mut state = TableState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(table, chunks[1], &mut state);
    }

    pub fn hints(&self) -> Line<'static> {
        match self.prompt.as_ref() {
            Some(Prompt::Connect { feed: Some(_), .. }) => {
                widgets::footer_hints(&[("⏎", "connect"), ("Esc", "cancel")])
            }
            Some(Prompt::Connect { feed: None, .. }) => {
                widgets::footer_hints(&[("y/⏎", "connect"), ("Esc", "cancel")])
            }
            Some(Prompt::Requirement { .. }) => widgets::footer_hints(&[("Esc", "back")]),
            Some(Prompt::Disconnect { .. }) => {
                widgets::footer_hints(&[("y/⏎", "disconnect"), ("Esc", "cancel")])
            }
            None => widgets::footer_hints(&[
                ("j/k", "move"),
                ("c", "connect"),
                ("d", "disconnect"),
                ("s", "sync"),
                ("h", "back"),
            ]),
        }
    }
}

/// The problem's own detail (or title) — the 422s render honestly (the git
/// requirement's "Connect GitHub first…", a bad feed URL's field message).
fn problem_text(e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized => "not authenticated — run `engineer login`".to_string(),
        ApiError::Problem { detail, .. } if !detail.is_empty() => format!("refused · {detail}"),
        ApiError::Problem { title, .. } => format!("refused · {title}"),
        other => other.to_string(),
    }
}

/// One source row: a state pill, the source name, and the plain-language `reads`
/// line (or, for a git source that needs GitHub, the requirement detail).
fn source_row(s: &CaptureSource) -> Row<'static> {
    let state = if s.connected {
        Span::styled(
            " connected ",
            Style::default().fg(Color::Black).bg(theme::SUCCESS),
        )
    } else if !s.connectable {
        Span::styled(
            " needs GitHub ",
            Style::default().fg(Color::Black).bg(theme::WARN),
        )
    } else {
        Span::styled("not connected", theme::muted())
    };
    let reads = s
        .requirement
        .as_ref()
        .map(|r| r.detail.clone())
        .unwrap_or_else(|| s.trust.reads.clone());
    Row::new(vec![
        Cell::from(Line::from(state)),
        Cell::from(s.name.clone()),
        Cell::from(reads).style(theme::muted()),
    ])
}

/// The trust statement (rendered **verbatim before connecting**) plus the
/// confirm — a feed-URL field for the calendar, a `y`/`⏎` confirm for git.
fn render_connect(frame: &mut Frame, area: Rect, source: &CaptureSource, feed: Option<&str>) {
    let mut lines = trust_lines(source);
    lines.push(Line::from(""));
    if let Some(url) = feed {
        lines.push(Line::from(Span::styled(
            "calendar feed url",
            theme::muted(),
        )));
        lines.push(Line::from(vec![
            Span::raw(url.to_string()),
            Span::styled("█", theme::muted()),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "⏎ connects with this feed · Esc cancels — nothing connects until you say so",
            theme::muted(),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                format!("Connect {}?", source.name),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  y/⏎ to connect · Esc to cancel — nothing connects until you say so.",
                theme::muted(),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(bordered(format!("Connect · {}", source.name)))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The honest requirement pointer — GitHub isn't connected, so the source can't
/// connect over the API. Render the server's detail and the web URL; don't retry.
fn render_requirement(frame: &mut Frame, area: Rect, source: &CaptureSource) {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} needs GitHub connected first", source.name),
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if let Some(req) = &source.requirement {
        lines.push(Line::from(Span::raw(req.detail.clone())));
        if let Some(url) = &req.url {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("connect it on the web  ", theme::muted()),
                Span::styled(url.clone(), Style::default().fg(theme::ACCENT)),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "GitHub sign-in happens in the browser — the CLI never asks for it here. Esc to go back.",
        theme::muted(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(bordered(format!("Connect · {}", source.name)))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The disconnect confirm — disconnect turns the source off; it does not delete
/// the drafts it already produced, and the copy says so.
fn render_disconnect(frame: &mut Frame, area: Rect, source: &CaptureSource) {
    let lines = vec![
        Line::from(Span::styled(
            format!("Disconnect {}?", source.name),
            Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "It stops producing new drafts. Drafts already in your inbox are kept —",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "disconnect turns the source off, it doesn't delete what it captured.",
            theme::muted(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y/⏎ to disconnect · Esc to keep it connected.",
            theme::muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(bordered("Connect · disconnect"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The plain-language trust statement, rendered **verbatim** from the payload —
/// the promise is the feature, so it is stated before anything connects and the
/// client never invents its own wording.
fn trust_lines(source: &CaptureSource) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "What this reads — and never reads",
            theme::header(),
        )),
        Line::from(""),
        trust_row("reads", &source.trust.reads),
        trust_row("never reads", &source.trust.never_reads),
        trust_row("promise", &source.trust.promise),
    ]
}

fn trust_row(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), theme::muted()),
        Span::raw(value.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn setup() -> (Connect, ApiClient, mpsc::UnboundedSender<Action>) {
        let api = ApiClient::with_token(Url::parse("http://localhost").unwrap(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Connect::default(), api, tx)
    }

    async fn feed(
        s: &mut Connect,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    fn git(connected: bool, connectable: bool) -> CaptureSource {
        let requirement = if connectable {
            serde_json::Value::Null
        } else {
            serde_json::json!({
                "kind": "github_connection",
                "detail": "Connect GitHub first — the scan uses your own connection.",
                "url": "https://engineer.example/github/connect"
            })
        };
        serde_json::from_value(serde_json::json!({
            "key": "git", "name": "Git activity",
            "connected": connected, "connectable": connectable, "requirement": requirement,
            "trust": {
                "reads": "Commit times and counts on repositories your activities anchor.",
                "never_reads": "Never messages, never code.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": []
        }))
        .unwrap()
    }

    fn calendar(connected: bool) -> CaptureSource {
        serde_json::from_value(serde_json::json!({
            "key": "calendar", "name": "Study calendar",
            "connected": connected, "connectable": true, "requirement": null,
            "trust": {
                "reads": "The titles and times of past events on this one calendar.",
                "never_reads": "Never descriptions, never attendees.",
                "promise": "Private, and nothing counts until you say so."
            },
            "params": ["feed_url"]
        }))
        .unwrap()
    }

    fn render(s: &mut Connect) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| s.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    async fn recv_matching(
        rx: &mut mpsc::UnboundedReceiver<Action>,
        pred: impl Fn(&Action) -> bool,
    ) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(a)) if pred(&a) => return true,
                Ok(Some(_)) => continue,
                _ => return false,
            }
        }
    }

    // ---- list: load, render, move ----

    #[tokio::test]
    async fn loaded_lists_the_sources_with_their_state() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, false), calendar(false)]),
        )
        .await;
        assert_eq!(s.sources.len(), 2);
        assert!(!s.loading);
        let text = render(&mut s);
        assert!(text.contains("Git activity"), "git row: {text}");
        assert!(text.contains("Study calendar"), "calendar row: {text}");
        // The un-connectable git source wears the requirement, not connect.
        assert!(text.contains("needs GitHub"), "git state pill: {text}");
    }

    #[tokio::test]
    async fn move_clamps_at_both_ends() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, true), calendar(false)]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::ConnectMove(-1)).await; // already at top
        assert_eq!(s.selected, 0);
        feed(&mut s, &api, &tx, Action::ConnectMove(1)).await;
        feed(&mut s, &api, &tx, Action::ConnectMove(1)).await; // clamps at last
        assert_eq!(s.selected, 1);
    }

    // ---- the trust gate: statement before the connect ----

    #[tokio::test]
    async fn connect_git_renders_the_trust_statement_before_connecting() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, true)]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::ConnectBegin).await;
        assert!(matches!(s.prompt, Some(Prompt::Connect { feed: None, .. })));
        let text = render(&mut s);
        // The trust strings are rendered verbatim, before any connect fires.
        assert!(
            text.contains("Commit times and counts"),
            "reads verbatim: {text}"
        );
        assert!(
            text.contains("Never messages, never code"),
            "never_reads verbatim: {text}"
        );
        assert!(
            text.contains("nothing counts until you say so"),
            "promise verbatim: {text}"
        );
        assert!(text.contains("Connect Git activity?"), "confirm: {text}");
    }

    // ---- the requirement path: GitHub not connected ----

    #[tokio::test]
    async fn connect_git_without_github_shows_the_requirement_pointer_not_a_connect() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, false)]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::ConnectBegin).await;
        assert!(matches!(s.prompt, Some(Prompt::Requirement { .. })));
        let text = render(&mut s);
        assert!(text.contains("needs GitHub connected first"), "{text}");
        assert!(text.contains("Connect GitHub first"), "detail: {text}");
        assert!(
            text.contains("engineer.example/github/connect"),
            "url: {text}"
        );
    }

    // ---- disconnect confirm ----

    #[tokio::test]
    async fn disconnect_arms_a_confirm_only_for_a_connected_source() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(true, true)]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::ConnectDisconnectBegin).await;
        assert!(matches!(s.prompt, Some(Prompt::Disconnect { .. })));
        let text = render(&mut s);
        assert!(text.contains("Disconnect Git activity?"), "{text}");
        // The confirm states the honesty: disconnect ≠ delete.
        assert!(
            text.contains("kept") || text.contains("doesn't delete"),
            "{text}"
        );
        // Esc cancels without a call.
        feed(&mut s, &api, &tx, Action::ConnectPromptCancel).await;
        assert!(s.prompt.is_none());
    }

    #[tokio::test]
    async fn disconnect_on_a_disconnected_source_is_a_noop_note() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, true)]),
        )
        .await;
        let note = s.handle(Action::ConnectDisconnectBegin, &api, &tx).await;
        assert!(s.prompt.is_none());
        assert!(matches!(note, Some((Level::Info, _))));
    }

    // ---- the calendar feed-url capture ----

    #[tokio::test]
    async fn calendar_connect_captures_a_feed_url_then_posts_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/connect"))
            .and(body_json(
                serde_json::json!({ "feed_url": "https://cal.example/basic.ics" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "calendar", "name": "Study calendar",
                "connected": true, "connectable": true, "requirement": null,
                "trust": { "reads": "…", "never_reads": "…", "promise": "…" },
                "params": ["feed_url"]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Connect::default();
        s.handle(Action::ConnectLoaded(vec![calendar(false)]), &api, &tx)
            .await;
        s.handle(Action::ConnectBegin, &api, &tx).await;
        assert!(matches!(
            s.prompt,
            Some(Prompt::Connect { feed: Some(_), .. })
        ));
        for c in "https://cal.example/basic.ics".chars() {
            s.handle(Action::ConnectFeedInput(c), &api, &tx).await;
        }
        s.handle(Action::ConnectPromptSubmit, &api, &tx).await;
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshConnect)).await);
    }

    // ---- the verbs: connect / disconnect / sync end-to-end ----

    #[tokio::test]
    async fn connect_git_fires_and_rereads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "git", "name": "Git activity",
                "connected": true, "connectable": true, "requirement": null,
                "trust": { "reads": "…", "never_reads": "…", "promise": "…" },
                "params": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Connect::default();
        s.handle(Action::ConnectLoaded(vec![git(false, true)]), &api, &tx)
            .await;
        s.handle(Action::ConnectBegin, &api, &tx).await;
        s.handle(Action::ConnectPromptSubmit, &api, &tx).await;
        assert!(s.in_flight);
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Success, text } if text.contains("connected")
            ))
            .await
        );
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::RefreshConnect)).await);
    }

    #[tokio::test]
    async fn disconnect_fires_and_rereads() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/capture/sources/git/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "git", "name": "Git activity",
                "connected": false, "connectable": true, "requirement": null,
                "trust": { "reads": "…", "never_reads": "…", "promise": "…" },
                "params": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Connect::default();
        s.handle(Action::ConnectLoaded(vec![git(true, true)]), &api, &tx)
            .await;
        s.handle(Action::ConnectDisconnectBegin, &api, &tx).await;
        s.handle(Action::ConnectPromptSubmit, &api, &tx).await;
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Success, text } if text.contains("disconnected")
            ))
            .await
        );
    }

    #[tokio::test]
    async fn sync_a_connected_source_queues_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/capture/sources/calendar/sync"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "queued": true, "key": "calendar"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Connect::default();
        s.handle(Action::ConnectLoaded(vec![calendar(true)]), &api, &tx)
            .await;
        s.handle(Action::ConnectSync, &api, &tx).await;
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Success, text } if text.contains("sync queued")
            ))
            .await
        );
    }

    #[tokio::test]
    async fn sync_a_disconnected_source_refuses_without_a_call() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::ConnectLoaded(vec![git(false, true)]),
        )
        .await;
        let note = s.handle(Action::ConnectSync, &api, &tx).await;
        assert!(matches!(note, Some((Level::Info, _))));
        assert!(!s.in_flight);
    }

    // ---- live-only: an offline verb refuses, never synthesizes ----

    #[tokio::test]
    async fn offline_connect_refuses_and_clears_the_guard() {
        // No server — the request is a transport failure (offline).
        let api = ApiClient::with_token(Url::parse("http://127.0.0.1:1").unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Connect::default();
        s.handle(Action::ConnectLoaded(vec![git(false, true)]), &api, &tx)
            .await;
        s.handle(Action::ConnectBegin, &api, &tx).await;
        s.handle(Action::ConnectPromptSubmit, &api, &tx).await;
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Error, text } if text.contains("offline")
            ))
            .await
        );
        s.handle(Action::ConnectActionFailed, &api, &tx).await;
        assert!(!s.in_flight);
    }

    // ---- intercept: prompts own their keys ----

    #[test]
    fn intercept_owns_prompt_keys_only_while_a_prompt_is_open() {
        let mut s = Connect::default();
        use crossterm::event::KeyModifiers;
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Closed: keys fall through to the global keymap.
        assert!(s.intercept_key(press(KeyCode::Char('y'))).is_none());
        // A git confirm owns y/⏎/Esc.
        s.prompt = Some(Prompt::Connect {
            source: git(false, true),
            feed: None,
        });
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('y'))),
            Some(Action::ConnectPromptSubmit)
        ));
        assert!(matches!(
            s.intercept_key(press(KeyCode::Esc)),
            Some(Action::ConnectPromptCancel)
        ));
        // The feed field types characters into the URL.
        s.prompt = Some(Prompt::Connect {
            source: calendar(false),
            feed: Some(String::new()),
        });
        assert!(matches!(
            s.intercept_key(press(KeyCode::Char('h'))),
            Some(Action::ConnectFeedInput('h'))
        ));
    }
}
