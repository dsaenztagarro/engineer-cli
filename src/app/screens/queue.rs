//! The Queue inspector — the intent-log's own face (offline-write.dc.html
//! §Queue inspector). A durable, ordered log of pending writes, rendered as a
//! glance, not a sync manager: the same `queue::pending()` read the headless
//! `engineer queue` table prints (one source of truth, shaped by `queue::view`),
//! with the board's four gestures over it.
//!
//!   `j`/`k`  move the full-row `▌` cursor over the intents
//!   `r`      retry now — a reconnect drain through the shipped `drain_reporting`
//!            (the ambient replay transcript streams as intents land)
//!   `x`      drop the selected **diverged** write — armed, then confirmed by a
//!            second `x` (the queue's one user-chosen delete; routes the #109
//!            `queue::drop_intent`, which refuses to orphan queued dependents)
//!   `⏎`      open a diverged intent's reconcile flow — routed to the shipped
//!            reconcile panel on the Timer screen (#106/#109), never a second UI
//!   `q`/Esc  close, back to Home
//!
//! Parked intents render dim with their reason (kept for review, out of the
//! replay line); an empty queue reads calm. Reachable via the `g q` goto chord
//! and the `:queue` palette verb — the header's `↑N` / diverged chip is a
//! static status read (the TUI header is not interactive), so it is not a
//! deep-link; navigation is the way in.

use crossterm::event::KeyEvent;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::ApiClient;
use crate::app::action::Action;
use crate::app::screens::ScreenKind;
use crate::queue::{self, view, Intent, IntentState, QueueStore, QueuedClient};
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

#[derive(Default)]
pub struct Queue {
    /// Every stored intent, in queue order (pending / diverged / parked) — the
    /// same read the CLI table shows.
    intents: Vec<Intent>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    /// A first `x` on a diverged row arms this; only the very next `x` drops —
    /// the confirmed delete is never a single keystroke. Any move / other
    /// gesture disarms it.
    confirm_drop: bool,
    /// A retry drain is in flight — guards a double `r`.
    draining: bool,
    /// Queue + cache locations for the store seam; `None` in production (the
    /// shared XDG paths). Tests inject a scratch dir.
    queue_paths: QueuePaths,
}

impl Queue {
    pub fn on_enter(&mut self, _api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        spawn_load(tx, self.queue_paths.clone());
    }

    /// No inline-edit state to own before the global keymap.
    pub fn intercept_key(&mut self, _key: KeyEvent) -> Option<Action> {
        None
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::QueueLoaded(intents) => {
                self.intents = intents;
                self.loading = false;
                self.error = None;
                self.draining = false;
                self.confirm_drop = false;
                self.clamp_selection();
            }
            Action::QueueLoadFailed(e) => {
                self.loading = false;
                self.draining = false;
                self.error = Some(e.clone());
                return Some((Level::Error, e));
            }
            Action::QueueRefresh => {
                self.loading = true;
                self.confirm_drop = false;
                spawn_load(tx, self.queue_paths.clone());
            }
            Action::QueueSelectMove(delta) => {
                // Any move disarms a pending drop confirm — the arm is scoped to
                // the row it was pressed on.
                self.confirm_drop = false;
                self.move_selection(delta);
            }
            // `r` — retry now: a reconnect drain through the shipped
            // `drain_reporting` (streaming the ambient replay transcript), then a
            // reload. Skips instantly over an empty / parked-only queue.
            Action::QueueRetry => {
                if self.draining {
                    return None;
                }
                self.draining = true;
                self.confirm_drop = false;
                spawn_retry(api, tx, self.queue_paths.clone());
            }
            // `x` — drop the selected diverged write, armed then confirmed.
            Action::QueueDropSelected => {
                let (id, diverged) = self.current().map(|i| (i.id, i.is_diverged()))?;
                if !diverged {
                    self.confirm_drop = false;
                    return Some((
                        Level::Warning,
                        "only a diverged write can be dropped — pending writes replay on their own"
                            .into(),
                    ));
                }
                if !self.confirm_drop {
                    self.confirm_drop = true;
                    return Some((
                        Level::Warning,
                        "drop this queued write? `x` again to confirm — it will never be written"
                            .into(),
                    ));
                }
                self.confirm_drop = false;
                spawn_drop(api, tx, self.queue_paths.clone(), id);
            }
            // `⏎` — open a diverged intent's reconcile flow. The reconcile panel
            // is the Timer screen's (#106/#109); routing there (its `on_enter`
            // opens the panel for the head divergence) reuses the one shipped
            // reconcile surface rather than building a second.
            Action::QueueOpenReconcile => {
                if !self.current().map(Intent::is_diverged)? {
                    return Some((
                        Level::Info,
                        "nothing to reconcile — this write isn't diverged".into(),
                    ));
                }
                let _ = tx.send(Action::Goto(ScreenKind::Timer));
            }
            _ => {}
        }
        None
    }

    fn current(&self) -> Option<&Intent> {
        self.intents.get(self.selected)
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.intents.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    fn clamp_selection(&mut self) {
        let len = self.intents.len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let now = jiff::Timestamp::now().as_second();
        let depth = self.intents.len();
        let title = match self.intents.first() {
            Some(oldest) => format!(
                "Queue · {depth} · oldest {} ago",
                view::fmt_age(view::age_s(oldest, now))
            ),
            None => "Queue".to_string(),
        };
        let block = bordered(title);

        if self.intents.is_empty() {
            if let Some(e) = &self.error {
                // A queue that can't be read is loud — never a calm "synced"
                // over stuck writes (the CLI exits 5 on the same failure).
                render_error(frame, area, block, e);
            } else if self.loading {
                frame.render_widget(Paragraph::new("loading…").block(block), area);
            } else {
                render_empty(frame, area, block);
            }
            return;
        }

        // The table, then the calm reconnect footnote (the design's bottom
        // note): replay is automatic, this view is a look.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(2)])
            .split(block.inner(area));
        frame.render_widget(block, area);

        let [h_id, h_intent, h_target, h_age, h_state] = view::HEADERS;
        let rows: Vec<Row> = self.intents.iter().map(|i| intent_row(i, now)).collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(5),  // #
                Constraint::Length(12), // INTENT (verb word)
                Constraint::Length(16), // TARGET (stream)
                Constraint::Length(6),  // AGE
                Constraint::Min(10),    // STATE (+ a parked reason)
            ],
        )
        .header(Row::new(vec![h_id, h_intent, h_target, h_age, h_state]).style(theme::header()))
        .row_highlight_style(theme::selection())
        .highlight_symbol("▌ ");
        let mut state = TableState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(table, chunks[0], &mut state);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Replays automatically on reconnect · a look, not a sync manager.",
                    theme::muted(),
                )),
            ]),
            chunks[1],
        );
    }

    pub fn hints(&self) -> Line<'static> {
        widgets::footer_hints(&[
            ("j/k", "move"),
            ("r", "retry"),
            ("x", "drop"),
            ("⏎", "reconcile"),
            ("q", "close"),
        ])
    }
}

/// §Queue inspector · empty — the calm zero state: a synced queue is a success,
/// not a void.
fn render_empty(frame: &mut Frame, area: Rect, block: ratatui::widgets::Block<'static>) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "queue empty · everything synced",
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Offline writes land here and replay in order when the wire returns.",
            theme::muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

/// The loud read-failure state: a corrupt / unreadable queue is never a silent
/// "everything synced" over stuck writes (the CLI exits 5 on the same failure).
fn render_error(frame: &mut Frame, area: Rect, block: ratatui::widgets::Block<'static>, e: &str) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "✗ the queue could not be read",
            Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(e.to_string(), theme::muted())),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

/// One intent as a table row: the shared `# INTENT TARGET AGE` cells, plus a
/// STATE cell painted in its state's idiom — pending accent, diverged the one
/// loud danger, parked dim with its reason (the whole row dims to read as
/// kept-for-review, out of play).
fn intent_row(intent: &Intent, now: i64) -> Row<'static> {
    let r = view::row(intent, now);
    let (state_text, state_style) = match &intent.state {
        IntentState::Pending => (r.state.to_string(), Style::default().fg(theme::ACCENT)),
        IntentState::Diverged { .. } => (
            r.state.to_string(),
            Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD),
        ),
        IntentState::Parked { reason } => (format!("{} · {reason}", r.state), theme::muted()),
    };
    let row = Row::new(vec![
        Cell::from(r.id),
        Cell::from(r.intent),
        Cell::from(r.target),
        Cell::from(r.age),
        Cell::from(state_text).style(state_style),
    ]);
    if intent.is_parked() {
        row.style(theme::muted())
    } else {
        row
    }
}

/// The store the board reads / mutates — the injected scratch path in tests,
/// the shared XDG queue in production.
fn queue_store(paths: &QueuePaths) -> Result<QueueStore, queue::QueueError> {
    match paths {
        Some((queue, _)) => Ok(QueueStore::at(queue.clone())),
        None => QueueStore::open_default(),
    }
}

/// Read the intent log off the reducer thread and hand it back — the same
/// `store.intents()` the CLI table reads. A store that can't be read is loud
/// (`QueueLoadFailed`), never a silent empty over stuck writes.
fn spawn_load(tx: &UnboundedSender<Action>, paths: QueuePaths) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let store = match queue_store(&paths) {
            Ok(store) => store,
            Err(e) => {
                let _ = tx.send(Action::QueueLoadFailed(e.to_string()));
                return;
            }
        };
        match store.intents() {
            Ok(intents) => {
                let _ = tx.send(Action::QueueLoaded(intents));
            }
            Err(e) => {
                let _ = tx.send(Action::QueueLoadFailed(e.to_string()));
            }
        }
    });
}

/// `r` retry now: drain through the shipped `drain_reporting`, streaming the
/// ambient replay transcript, then reload the board. Skips instantly on an
/// empty / parked-only / lock-held queue.
fn spawn_retry(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match open_queued(&api, &paths) {
            Ok(queued) => drain_behind(&queued, &tx).await,
            Err(e) => notify_seam_error(&tx, "retry failed", e),
        }
        let _ = tx.send(Action::QueueRefresh);
    });
}

/// `x` drop: route the #109 `queue::drop_intent` (the confirm already given by
/// the second `x`), then drain the now-unblocked stream behind it and reload.
/// A refusal (a parent with queued dependents, a non-diverged intent) surfaces
/// loudly and changes nothing.
fn spawn_drop(api: &ApiClient, tx: &UnboundedSender<Action>, paths: QueuePaths, id: u64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let store = match queue_store(&paths) {
            Ok(store) => store,
            Err(e) => return notify_seam_error(&tx, "drop failed", e),
        };
        match queue::drop_intent(&store, id) {
            Ok(dropped) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!(
                        "dropped — the queued {} left the queue; nothing was written",
                        dropped.kind.word()
                    ),
                });
                if let Ok(queued) = open_queued(&api, &paths) {
                    drain_behind(&queued, &tx).await;
                }
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("drop refused: {e} — the intent stays as it was"),
                });
            }
        }
        let _ = tx.send(Action::QueueRefresh);
    });
}

/// Continue the drain, streaming the shipped reconnect transcript
/// (`ReplayProgress` per landed intent, the report tile at the end) — the same
/// tail the Timer reconcile gestures run, so the board and the panel replay
/// identically.
async fn drain_behind(queued: &QueuedClient, tx: &UnboundedSender<Action>) {
    let tx2 = tx.clone();
    if let Some(report) = queued
        .drain_reporting(|intent| {
            let _ = tx2.send(Action::ReplayProgress {
                word: intent.kind.word().to_string(),
            });
        })
        .await
    {
        let _ = tx.send(Action::ReplayFinished(report));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ConflictInfo, FieldError};
    use crate::queue::IntentKind;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A per-test scratch (queue.json, cache) so a spawned drain / drop lands in
    /// a throwaway dir, never the shared XDG queue.
    fn scratch_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-queue-screen-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("queue.json"), dir.join("timer-cache.json"))
    }

    fn store_at(paths: &(std::path::PathBuf, std::path::PathBuf)) -> QueueStore {
        QueueStore::at(paths.0.clone())
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn screen(paths: (std::path::PathBuf, std::path::PathBuf)) -> Queue {
        Queue {
            queue_paths: Some(paths),
            ..Default::default()
        }
    }

    fn diverge_first(store: &QueueStore) {
        store
            .mutate(|doc| {
                doc.intents_mut()[0].state = IntentState::Diverged {
                    status: 422,
                    title: "Segment overlaps".into(),
                    detail: String::new(),
                    type_uri: None,
                    errors: Vec::<FieldError>::new(),
                    code: None,
                    conflict: Box::new(ConflictInfo::default()),
                };
            })
            .unwrap();
    }

    async fn feed(s: &mut Queue, api: &ApiClient, tx: &mpsc::UnboundedSender<Action>, a: Action) {
        s.handle(a, api, tx).await;
    }

    fn render(s: &mut Queue) -> String {
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
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Some(a)) if pred(&a) => return true,
                Ok(Some(_)) => continue,
                _ => return false,
            }
        }
    }

    // ---- list render: the shared columns, parked + diverged states ----

    #[tokio::test]
    async fn empty_queue_renders_the_calm_synced_state() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut s = screen(scratch_paths());
        feed(&mut s, &dead_api(), &tx, Action::QueueLoaded(vec![])).await;
        let text = render(&mut s);
        assert!(text.contains("everything synced"), "zero state: {text}");
    }

    #[tokio::test]
    async fn list_renders_the_shared_columns_and_the_states() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        // Park the third so the dim/reason render is covered.
        store
            .enqueue(IntentKind::TimerResume {
                at: "2026-07-15T09:50:00Z".parse().unwrap(),
            })
            .unwrap();
        diverge_first(&store); // the pause diverges
        store
            .mutate(|doc| {
                doc.intents_mut()[2].state = IntentState::Parked {
                    reason: "took server · Conflict".into(),
                };
            })
            .unwrap();

        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        let text = render(&mut s);
        // The shared header columns.
        assert!(text.contains("INTENT"), "header: {text}");
        assert!(text.contains("TARGET"), "header: {text}");
        assert!(text.contains("STATE"), "header: {text}");
        // The verb words + the three states.
        assert!(text.contains("pause"), "verb: {text}");
        assert!(text.contains("diverged"), "diverged state: {text}");
        assert!(text.contains("parked"), "parked state: {text}");
        // Parked renders its reason.
        assert!(text.contains("took server"), "parked reason: {text}");
    }

    #[tokio::test]
    async fn a_read_failure_renders_loud_not_a_calm_synced() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut s = screen(scratch_paths());
        let out = s
            .handle(
                Action::QueueLoadFailed("queue.json is corrupt".into()),
                &dead_api(),
                &tx,
            )
            .await;
        assert!(matches!(out, Some((Level::Error, _))), "surfaced loudly");
        let text = render(&mut s);
        assert!(text.contains("could not be read"), "loud state: {text}");
        assert!(text.contains("corrupt"), "names the failure: {text}");
        assert!(!text.contains("everything synced"), "never a calm lie");
    }

    // ---- j/k selection ----

    #[tokio::test]
    async fn selection_moves_and_clamps() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let paths = scratch_paths();
        let store = store_at(&paths);
        for _ in 0..3 {
            store
                .enqueue(IntentKind::TimerPause {
                    at: "2026-07-15T09:40:00Z".parse().unwrap(),
                })
                .unwrap();
        }
        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        feed(&mut s, &dead_api(), &tx, Action::QueueSelectMove(-1)).await; // clamps at top
        assert_eq!(s.selected, 0);
        feed(&mut s, &dead_api(), &tx, Action::QueueSelectMove(1)).await;
        feed(&mut s, &dead_api(), &tx, Action::QueueSelectMove(1)).await;
        feed(&mut s, &dead_api(), &tx, Action::QueueSelectMove(1)).await; // clamps at last
        assert_eq!(s.selected, 2);
    }

    // ---- r retry: a drain streams the transcript ----

    #[tokio::test]
    async fn retry_drains_and_streams_the_replay_transcript() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/timer/pause"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "running": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = screen(paths);
        feed(
            &mut s,
            &client(&server),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        feed(&mut s, &client(&server), &tx, Action::QueueRetry).await;
        assert!(s.draining, "the guard is set while the drain runs");
        // The shipped reconnect transcript: a ReplayProgress for the landed
        // pause, then the board reloads.
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::ReplayProgress { word } if word == "pause"
            ))
            .await
        );
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::QueueRefresh)).await);
    }

    #[tokio::test]
    async fn retry_is_guarded_against_a_double_fire() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut s = screen(scratch_paths());
        s.draining = true;
        // A second `r` while draining is a no-op — no new spawn.
        feed(&mut s, &dead_api(), &tx, Action::QueueRetry).await;
        assert!(s.draining);
    }

    // ---- x drop: the confirm flow ----

    #[tokio::test]
    async fn drop_on_a_pending_intent_refuses_without_arming() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        let out = s.handle(Action::QueueDropSelected, &dead_api(), &tx).await;
        assert!(
            matches!(out, Some((Level::Warning, t)) if t.contains("only a diverged write")),
            "pending refuses drop"
        );
        assert!(!s.confirm_drop, "a pending row never arms");
    }

    #[tokio::test]
    async fn drop_arms_then_confirms_and_removes_the_diverged_intent() {
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        diverge_first(&store);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = screen(paths.clone());
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;

        // First `x` arms.
        let out = s.handle(Action::QueueDropSelected, &dead_api(), &tx).await;
        assert!(matches!(out, Some((Level::Warning, t)) if t.contains("again to confirm")));
        assert!(s.confirm_drop, "armed");
        // Second `x` drops — spawns the delete, then the board refreshes.
        let out = s.handle(Action::QueueDropSelected, &dead_api(), &tx).await;
        assert!(out.is_none(), "confirmed drop returns no immediate warning");
        assert!(!s.confirm_drop, "disarmed after firing");
        assert!(
            recv_matching(&mut rx, |a| matches!(
                a,
                Action::Notify { level: Level::Success, text } if text.contains("dropped")
            ))
            .await
        );
        assert!(recv_matching(&mut rx, |a| matches!(a, Action::QueueRefresh)).await);
        // The store no longer holds the diverged segment.
        assert!(
            store.intents().unwrap().is_empty(),
            "the one user-chosen delete"
        );
    }

    #[tokio::test]
    async fn a_move_disarms_a_pending_drop_confirm() {
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        diverge_first(&store);

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        s.handle(Action::QueueDropSelected, &dead_api(), &tx).await; // arm
        assert!(s.confirm_drop);
        feed(&mut s, &dead_api(), &tx, Action::QueueSelectMove(1)).await;
        assert!(!s.confirm_drop, "moving off the row disarms");
    }

    // ---- ⏎ route to the shipped reconcile panel ----

    #[tokio::test]
    async fn enter_on_a_diverged_intent_routes_to_the_timer_reconcile() {
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::SegmentCreate {
                activity_id: 9,
                started_at: "2026-07-15T14:02:00Z".parse().unwrap(),
                minutes: 45,
            })
            .unwrap();
        diverge_first(&store);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        let out = s.handle(Action::QueueOpenReconcile, &dead_api(), &tx).await;
        assert!(out.is_none());
        assert!(
            recv_matching(&mut rx, |a| matches!(a, Action::Goto(ScreenKind::Timer))).await,
            "⏎ routes to the shipped reconcile panel on the Timer screen"
        );
    }

    #[tokio::test]
    async fn enter_on_a_pending_intent_hints_instead_of_routing() {
        let paths = scratch_paths();
        let store = store_at(&paths);
        store
            .enqueue(IntentKind::TimerPause {
                at: "2026-07-15T09:40:00Z".parse().unwrap(),
            })
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut s = screen(paths);
        feed(
            &mut s,
            &dead_api(),
            &tx,
            Action::QueueLoaded(store.intents().unwrap()),
        )
        .await;
        let out = s.handle(Action::QueueOpenReconcile, &dead_api(), &tx).await;
        assert!(
            matches!(out, Some((Level::Info, t)) if t.contains("isn't diverged")),
            "a pending row has nothing to reconcile"
        );
    }
}
