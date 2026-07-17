use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Book, BookChapter, BookStatus, BookUpdate};
use crate::app::action::Action;
use crate::queue::WriteOutcome;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

/// The status picker's rows, in display order. The `r/c/u/h/a` mnemonics and the
/// kit's pill vocabulary (` reading `/` done `/` unread `/` hold `/` stop `) key
/// off this order, and `BookStatusMove` steps the cursor within it.
const STATUSES: [BookStatus; 5] = [
    BookStatus::Reading,
    BookStatus::Completed,
    BookStatus::Unread,
    BookStatus::OnHold,
    BookStatus::Abandoned,
];

pub struct BookDetail {
    book: Option<Book>,
    chapters: Vec<BookChapter>,
    state: ListState,
    edit_page: Option<String>,
    /// The status picker modal's cursor, `Some` while it's open.
    status_picker: Option<ListState>,
    /// Queue + read-cache locations for the offline write seam — `None`
    /// (production) uses the shared XDG paths, tests inject a scratch dir.
    queue_paths: QueuePaths,
}

impl Default for BookDetail {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            book: None,
            chapters: vec![],
            state,
            edit_page: None,
            status_picker: None,
            queue_paths: None,
        }
    }
}

impl BookDetail {
    pub fn on_enter(&mut self, _api: &ApiClient, _tx: &UnboundedSender<Action>) {
        // Loaded by Books::BooksOpen which emits BookDetailLoaded.
    }

    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        // The status picker is modal: while open it owns j/k, the r/c/u/h/a
        // mnemonics, Enter (confirm) and Esc (cancel), so `h` picks `on_hold`
        // rather than stepping back and Esc closes the modal, not the screen.
        if self.status_picker.is_some() {
            return match key.code {
                KeyCode::Esc => Some(Action::BookStatusCancel),
                KeyCode::Enter => Some(Action::BookStatusConfirm),
                KeyCode::Char('j') | KeyCode::Down => Some(Action::BookStatusMove(1)),
                KeyCode::Char('k') | KeyCode::Up => Some(Action::BookStatusMove(-1)),
                KeyCode::Char('r') => Some(Action::BookStatusSelect(BookStatus::Reading)),
                KeyCode::Char('c') => Some(Action::BookStatusSelect(BookStatus::Completed)),
                KeyCode::Char('u') => Some(Action::BookStatusSelect(BookStatus::Unread)),
                KeyCode::Char('h') => Some(Action::BookStatusSelect(BookStatus::OnHold)),
                KeyCode::Char('a') => Some(Action::BookStatusSelect(BookStatus::Abandoned)),
                _ => None,
            };
        }
        self.edit_page.as_ref()?;
        match key.code {
            KeyCode::Esc => Some(Action::CancelEdit),
            KeyCode::Enter => Some(Action::SubmitPage),
            KeyCode::Backspace => Some(Action::EditPageBackspace),
            KeyCode::Char(c) if c.is_ascii_digit() => Some(Action::EditPageInput(c)),
            _ => None,
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::BookDetailLoaded { book, chapters } => {
                self.book = Some(*book);
                self.chapters = chapters;
                self.state.select(self.current_chapter_index().or(Some(0)));
            }
            Action::ChapterMove(d) => self.move_cursor(d),
            Action::BeginEditPage => self.edit_page = Some(String::new()),
            Action::EditPageInput(c) => {
                if let Some(b) = self.edit_page.as_mut() {
                    b.push(c);
                }
            }
            Action::EditPageBackspace => {
                if let Some(b) = self.edit_page.as_mut() {
                    b.pop();
                }
            }
            Action::CancelEdit => self.edit_page = None,
            Action::SubmitPage => {
                if let (Some(book), Some(buf)) = (&self.book, &self.edit_page) {
                    if let Ok(page) = buf.parse::<u32>() {
                        spawn_book_update(
                            api,
                            tx,
                            self.queue_paths.clone(),
                            book.clone(),
                            BookUpdate {
                                current_page: Some(page),
                                ..Default::default()
                            },
                            Some("page updated".into()),
                            format!("page {page} · queued (offline) — will sync"),
                        );
                    }
                }
                self.edit_page = None;
            }
            Action::ToggleChapterDone => {
                if let Some(chapter) = self.selected_chapter().cloned() {
                    if let Some(book) = &self.book {
                        // Mark this chapter as current and advance cursor.
                        spawn_book_update(
                            api,
                            tx,
                            self.queue_paths.clone(),
                            book.clone(),
                            BookUpdate {
                                current_chapter_id: Some(chapter.id),
                                ..Default::default()
                            },
                            None,
                            "chapter · queued (offline) — will sync".into(),
                        );
                        self.move_cursor(1);
                    }
                }
            }
            Action::BookStatusPicker => {
                // Open the modal at the book's current status, so a confirm with
                // no movement is a no-op change the user can eyeball first.
                if let Some(book) = &self.book {
                    let mut state = ListState::default();
                    state.select(Some(status_index(book.status)));
                    self.status_picker = Some(state);
                }
            }
            Action::BookStatusMove(delta) => {
                if let Some(state) = self.status_picker.as_mut() {
                    let cur = state.selected().unwrap_or(0) as i32;
                    let next = (cur + delta).clamp(0, STATUSES.len() as i32 - 1);
                    state.select(Some(next as usize));
                }
            }
            Action::BookStatusSelect(status) => {
                if let Some(state) = self.status_picker.as_mut() {
                    state.select(Some(status_index(status)));
                }
            }
            Action::BookStatusCancel => self.status_picker = None,
            Action::BookStatusConfirm => {
                if let (Some(state), Some(book)) = (self.status_picker.as_ref(), &self.book) {
                    let status = STATUSES[state.selected().unwrap_or(0)];
                    spawn_book_update(
                        api,
                        tx,
                        self.queue_paths.clone(),
                        book.clone(),
                        BookUpdate {
                            status: Some(status),
                            ..Default::default()
                        },
                        Some(format!("status → {}", status.label())),
                        format!("status → {} · queued (offline) — will sync", status.label()),
                    );
                }
                self.status_picker = None;
            }
            Action::BookUpdated(b) => {
                self.book = Some(*b);
            }
            _ => {}
        }
        None
    }

    fn current_chapter_index(&self) -> Option<usize> {
        let cur_id = self.book.as_ref()?.current_chapter_id?;
        self.chapters.iter().position(|c| c.id == cur_id)
    }

    fn selected_chapter(&self) -> Option<&BookChapter> {
        self.state.selected().and_then(|i| self.chapters.get(i))
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.chapters.is_empty() {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.chapters.len() as i32 - 1);
        self.state.select(Some(next as usize));
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(5), Constraint::Min(0)])
            .split(area);

        let Some(book) = self.book.clone() else {
            frame.render_widget(Paragraph::new("loading…").block(bordered("Book")), area);
            return;
        };

        // Header
        let pct = book.progress_percent.unwrap_or(0.0);
        let mut header_lines = vec![
            Line::from(vec![
                widgets::status_pill(book.status),
                Span::raw("  "),
                Span::styled(book.title.clone(), theme::header()),
                Span::styled(
                    format!("  · {}", book.author.clone().unwrap_or_default()),
                    theme::muted(),
                ),
            ]),
            widgets::progress_bar(pct, 40),
        ];
        if let Some(p) = book.current_page {
            header_lines.push(Line::from(Span::styled(
                format!(
                    "page {}{}",
                    p,
                    book.page_count
                        .map(|t| format!(" / {t}"))
                        .unwrap_or_default()
                ),
                theme::muted(),
            )));
        }
        if let Some(buf) = &self.edit_page {
            header_lines.push(Line::from(vec![
                Span::styled("set page → ", theme::focused()),
                Span::raw(buf.clone()),
                Span::styled("█", theme::muted()),
            ]));
        }
        frame.render_widget(Paragraph::new(header_lines).block(bordered(" ")), chunks[0]);

        // Chapters
        let cur_id = book.current_chapter_id;
        let items: Vec<ListItem> = self
            .chapters
            .iter()
            .map(|c| {
                let mark = if c.done {
                    "✓"
                } else if c.skipped {
                    "·"
                } else {
                    " "
                };
                let is_current = Some(c.id) == cur_id;
                let style = if is_current {
                    theme::focused()
                } else {
                    ratatui::style::Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" [{}] ", mark),
                        if c.done {
                            theme::focused()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(format!("{:>3}.  ", c.number), theme::muted()),
                    Span::styled(c.title.clone(), style),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(bordered(format!("Chapters · {}", self.chapters.len())))
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, chunks[1], &mut self.state);

        // The status picker renders last, as a small centered modal over the body.
        if self.status_picker.is_some() {
            self.render_status_picker(frame, area);
        }
    }

    /// The status picker modal — five kit pills, one per `BookStatus`, each with
    /// its `r/c/u/h/a` mnemonic. The highlighted row is the pending choice; the
    /// footer carries the keymap.
    fn render_status_picker(&mut self, frame: &mut Frame, area: Rect) {
        let modal = centered(area, 34, STATUSES.len() as u16 + 2);
        frame.render_widget(Clear, modal);
        let block = bordered("Status");
        let inner = block.inner(modal);
        frame.render_widget(block, modal);

        let items: Vec<ListItem> = STATUSES
            .iter()
            .map(|&s| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {} ", mnemonic(s)), theme::muted()),
                    widgets::status_pill(s),
                    Span::styled(format!("  {}", s.label()), theme::muted()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        if let Some(state) = self.status_picker.as_mut() {
            frame.render_stateful_widget(list, inner, state);
        }
    }

    pub fn hints(&self) -> Line<'static> {
        if self.status_picker.is_some() {
            return Line::from(Span::styled(
                "status · j/k or r/c/u/h/a pick · ↵ set · Esc cancel",
                theme::muted(),
            ));
        }
        if self.edit_page.is_some() {
            return Line::from(Span::styled(
                "page · digits + Enter · Esc cancel",
                theme::muted(),
            ));
        }
        widgets::footer_hints(&[
            ("j/k", "chapter"),
            ("⎵", "done & next"),
            ("p", "page"),
            ("s", "status"),
            ("h", "back"),
        ])
    }
}

/// The `r/c/u/h/a` mnemonic key for a status, matching the picker's keymap.
fn mnemonic(status: BookStatus) -> char {
    match status {
        BookStatus::Reading => 'r',
        BookStatus::Completed => 'c',
        BookStatus::Unread => 'u',
        BookStatus::OnHold => 'h',
        BookStatus::Abandoned => 'a',
    }
}

/// A status's row index in `STATUSES` (the picker cursor position).
fn status_index(status: BookStatus) -> usize {
    STATUSES.iter().position(|&s| s == status).unwrap_or(0)
}

/// Route a book write through the offline seam — the shared arm behind the
/// detail's three writes (`s` status, `p` page, `⎵` chapter-done). Live, the
/// server's recomputed book (progress, chapter marks) feeds the detail; offline,
/// the seam field-flips `current` and the queued stand-in feeds it just the same
/// (progress stays at its last-known reading, honest until the drain), with a
/// muted "queued (offline)" line. `ok_msg` is the confirmed-only success line
/// (chapter-done stays silent, `None`).
fn spawn_book_update(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    current: Book,
    body: BookUpdate,
    ok_msg: Option<String>,
    queued_msg: String,
) {
    let id = current.id;
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => return notify_seam_error(&tx, "update failed", e),
        };
        match queued.update_book(id, &body, &current).await {
            Ok(WriteOutcome::Confirmed(b)) => {
                let _ = tx.send(Action::BookUpdated(Box::new(b)));
                if let Some(msg) = ok_msg {
                    let _ = tx.send(Action::Notify {
                        level: Level::Success,
                        text: msg,
                    });
                }
            }
            Ok(WriteOutcome::Provisional(b)) => {
                let _ = tx.send(Action::BookUpdated(Box::new(b)));
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: queued_msg,
                });
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("update failed: {e}"),
                });
            }
        }
    });
}

/// A fixed-size rectangle centered in `area`, clamped to fit.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_book(id: i64, status: &str) -> Book {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "SICP", "status": status
        }))
        .unwrap()
    }

    fn dev_api() -> ApiClient {
        let config = Config::for_environment(Environment::Development);
        ApiClient::with_token(config.api_url.clone(), "tok".into())
    }

    /// A base URL nothing listens on — reqwest fails before any response, which
    /// is exactly `ApiError::Transport`, the offline seam's trigger.
    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("engineer-book-detail-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn loaded(
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        status: &str,
    ) -> BookDetail {
        let mut s = BookDetail::default();
        s.handle(
            Action::BookDetailLoaded {
                book: Box::new(make_book(7, status)),
                chapters: vec![],
            },
            api,
            tx,
        )
        .await;
        s
    }

    fn render_to_string(s: &mut BookDetail) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                s.render(f, area);
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

    // Status mapping is the wire contract: each `BookStatus` must serialise to
    // the API's snake_case enum value inside a `BookUpdate` body.
    #[test]
    fn status_maps_to_wire_enum_values() {
        let wire = |s| {
            serde_json::to_value(BookUpdate {
                status: Some(s),
                ..Default::default()
            })
            .unwrap()["status"]
                .clone()
        };
        assert_eq!(wire(BookStatus::Reading), "reading");
        assert_eq!(wire(BookStatus::Completed), "completed");
        assert_eq!(wire(BookStatus::Unread), "unread");
        assert_eq!(wire(BookStatus::OnHold), "on_hold");
        assert_eq!(wire(BookStatus::Abandoned), "abandoned");
    }

    #[tokio::test]
    async fn picker_opens_at_current_status() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let api = dev_api();
        let mut s = loaded(&api, &tx, "on_hold").await;
        s.handle(Action::BookStatusPicker, &api, &tx).await;
        let state = s.status_picker.as_ref().expect("picker is open");
        assert_eq!(state.selected(), Some(status_index(BookStatus::OnHold)));
    }

    #[tokio::test]
    async fn move_steps_and_clamps_the_selection() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let api = dev_api();
        let mut s = loaded(&api, &tx, "reading").await; // opens at index 0
        s.handle(Action::BookStatusPicker, &api, &tx).await;
        s.handle(Action::BookStatusMove(1), &api, &tx).await;
        assert_eq!(s.status_picker.as_ref().unwrap().selected(), Some(1));
        // Clamp at the top…
        s.handle(Action::BookStatusMove(-5), &api, &tx).await;
        assert_eq!(s.status_picker.as_ref().unwrap().selected(), Some(0));
        // …and at the bottom.
        s.handle(Action::BookStatusMove(99), &api, &tx).await;
        assert_eq!(
            s.status_picker.as_ref().unwrap().selected(),
            Some(STATUSES.len() - 1)
        );
    }

    #[tokio::test]
    async fn mnemonic_select_jumps_to_status() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let api = dev_api();
        let mut s = loaded(&api, &tx, "reading").await;
        s.handle(Action::BookStatusPicker, &api, &tx).await;
        s.handle(Action::BookStatusSelect(BookStatus::Abandoned), &api, &tx)
            .await;
        assert_eq!(
            s.status_picker.as_ref().unwrap().selected(),
            Some(status_index(BookStatus::Abandoned))
        );
    }

    #[tokio::test]
    async fn cancel_closes_without_patching() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let api = dev_api();
        let mut s = loaded(&api, &tx, "reading").await;
        s.handle(Action::BookStatusPicker, &api, &tx).await;
        s.handle(Action::BookStatusSelect(BookStatus::Completed), &api, &tx)
            .await;
        s.handle(Action::BookStatusCancel, &api, &tx).await;
        assert!(s.status_picker.is_none());
        assert!(rx.try_recv().is_err(), "cancel dispatches nothing");
    }

    #[tokio::test]
    async fn confirm_patches_selected_status_and_reflects_it() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/books/7"))
            .and(body_json(
                serde_json::json!({ "book": { "status": "on_hold" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "id": 7, "title": "SICP", "status": "on_hold" }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let api = ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = loaded(&api, &tx, "reading").await;
        // A scratch queue so the live write's drain-before-write never touches
        // the shared XDG state.
        let dir = scratch("confirm");
        s.queue_paths = Some((dir.join("queue.json"), dir.join("timer-cache.json")));

        s.handle(Action::BookStatusPicker, &api, &tx).await;
        s.handle(Action::BookStatusSelect(BookStatus::OnHold), &api, &tx)
            .await;
        s.handle(Action::BookStatusConfirm, &api, &tx).await;
        assert!(s.status_picker.is_none(), "confirm closes the picker");

        // The spawned PATCH sends the updated book, then a success notify.
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("PATCH result within 5s")
            .expect("an action");
        let updated = match first {
            Action::BookUpdated(b) => b,
            other => panic!("expected BookUpdated, got {other:?}"),
        };
        // Feeding the result back reflects the new status in the header.
        s.handle(Action::BookUpdated(updated), &api, &tx).await;
        assert_eq!(s.book.as_ref().unwrap().status, BookStatus::OnHold);

        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("notify within 5s")
            .expect("an action");
        assert!(matches!(
            second,
            Action::Notify {
                level: Level::Success,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn offline_status_confirm_queues_a_book_update_and_flips_the_pill() {
        let dir = scratch("offline-status");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = loaded(&dead_api(), &tx, "reading").await;
        s.queue_paths = Some((dir.join("queue.json"), dir.join("timer-cache.json")));

        s.handle(Action::BookStatusPicker, &dead_api(), &tx).await;
        s.handle(
            Action::BookStatusSelect(BookStatus::OnHold),
            &dead_api(),
            &tx,
        )
        .await;
        s.handle(Action::BookStatusConfirm, &dead_api(), &tx).await;

        // Offline: the seam field-flips the current book and feeds it back, then
        // an Info "queued (offline)" line.
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("BookUpdated within 5s")
            .expect("an action");
        let updated = match first {
            Action::BookUpdated(b) => b,
            other => panic!("expected BookUpdated, got {other:?}"),
        };
        s.handle(Action::BookUpdated(updated), &dead_api(), &tx)
            .await;
        assert_eq!(
            s.book.as_ref().unwrap().status,
            BookStatus::OnHold,
            "the provisional flip reflects locally"
        );
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("notify within 5s")
            .expect("an action");
        assert!(matches!(
            second,
            Action::Notify {
                level: Level::Info,
                ..
            }
        ));

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "book");
        assert_eq!(intents[0].stream, "book:7");
    }

    #[tokio::test]
    async fn picker_renders_pills_and_mnemonics() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let api = dev_api();
        let mut s = loaded(&api, &tx, "reading").await;
        s.handle(Action::BookStatusPicker, &api, &tx).await;
        let text = render_to_string(&mut s);
        // One label from each kit pill (reading/done/unread/hold/stop) renders.
        for pill in ["reading", "done", "unread", "hold", "stop"] {
            assert!(text.contains(pill), "pill '{pill}' missing from: {text}");
        }
    }
}
