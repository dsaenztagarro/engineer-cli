use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, ApiError, Book, BookStatus};
use crate::app::action::{Action, BooksFilter};
use crate::app::screens::ScreenKind;
use crate::messages;
use crate::ui::notify::Level;
use crate::ui::panel::{render_panel_state, PanelFailure, PanelState};
use crate::ui::picker::{Picker, PickerItem};
use crate::ui::search::{self, SearchBox};
use crate::ui::{layout::bordered, theme, widgets};

pub struct Books {
    pub items: Vec<Book>,
    pub state: ListState,
    pub filter: BooksFilter,
    /// `/` re-queries the server; `n`/`N` step within the loaded set.
    pub search: SearchBox,
    pub loading: bool,
    failure: Option<PanelFailure>,
    /// `f` — a local fuzzy jump within the loaded set, beside the server-side `/`.
    picker: Option<Picker<i64>>,
}

impl Default for Books {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            items: vec![],
            state,
            filter: BooksFilter::Reading,
            search: SearchBox::default(),
            loading: false,
            failure: None,
            picker: None,
        }
    }
}

impl Books {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let api = api.clone();
        let tx = tx.clone();
        let status = match self.filter {
            BooksFilter::All => None,
            BooksFilter::Reading => Some(BookStatus::Reading),
            BooksFilter::Completed => Some(BookStatus::Completed),
        };
        let q = if self.search.is_empty() {
            None
        } else {
            Some(self.search.query.clone())
        };
        tokio::spawn(async move {
            let q_ref = q.as_deref();
            match api.list_books(status, q_ref).await {
                Ok(list) => {
                    let _ = tx.send(Action::BooksLoaded(list.data));
                }
                Err(ApiError::Unauthorized) => {
                    let _ = tx.send(Action::SessionExpired);
                }
                Err(e) => {
                    let _ = tx.send(Action::BooksLoadFailed(messages::fail_reason(
                        api.host(),
                        &e,
                    )));
                }
            }
        });
    }

    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.picker.is_some() {
            return Some(Action::BooksPickerKey(key));
        }
        if !self.search.active {
            if matches!(key.code, KeyCode::Char('/')) {
                self.search.open();
                return Some(Action::Notify {
                    level: Level::Info,
                    text: "search: type then Enter".into(),
                });
            }
            if matches!(key.code, KeyCode::Char('f')) {
                return Some(Action::BooksPickerOpen);
            }
            if !self.search.is_empty() {
                match key.code {
                    KeyCode::Char('n') => return Some(Action::BooksMatchStep(1)),
                    KeyCode::Char('N') => return Some(Action::BooksMatchStep(-1)),
                    _ => {}
                }
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => Some(Action::BooksSearchCancel),
            KeyCode::Enter => Some(Action::BooksSearchSubmit),
            KeyCode::Backspace => Some(Action::BooksSearchBackspace),
            KeyCode::Char(c) => Some(Action::BooksSearchInput(c)),
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
            Action::BooksLoaded(books) => {
                self.items = books;
                self.loading = false;
                self.failure = None;
                if self.state.selected().unwrap_or(0) >= self.items.len() {
                    self.state
                        .select(if self.items.is_empty() { None } else { Some(0) });
                }
            }
            Action::BooksLoadFailed(reason) => {
                self.loading = false;
                self.failure = Some(PanelFailure {
                    headline: messages::load_failed("books"),
                    reason,
                    retry_key: "r",
                    cached: false,
                });
            }
            Action::BooksFilter(f) => {
                self.filter = f;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::BooksMove(d) => self.move_cursor(d),
            Action::BooksMatchStep(d) => self.step_match(d),
            Action::BooksJumpStart => {
                self.state
                    .select(if self.items.is_empty() { None } else { Some(0) })
            }
            Action::BooksJumpEnd => {
                if !self.items.is_empty() {
                    self.state.select(Some(self.items.len() - 1));
                }
            }
            Action::BooksOpen => {
                if let Some(book) = self.selected().cloned() {
                    let api = api.clone();
                    let tx = tx.clone();
                    // Navigate first so the failure (or the payload) lands on the
                    // detail screen, which is now current.
                    let _ = tx.send(Action::Goto(ScreenKind::BookDetail));
                    tokio::spawn(async move {
                        match api.list_chapters(book.id).await {
                            Ok(list) => {
                                let _ = tx.send(Action::BookDetailLoaded {
                                    book: Box::new(book),
                                    chapters: list.data,
                                });
                            }
                            Err(ApiError::Unauthorized) => {
                                let _ = tx.send(Action::SessionExpired);
                            }
                            Err(e) => {
                                let _ = tx.send(Action::BookDetailLoadFailed(
                                    messages::fail_reason(api.host(), &e),
                                ));
                            }
                        }
                    });
                }
            }
            Action::BooksSearchInput(c) => self.search.input(c),
            Action::BooksSearchBackspace => self.search.backspace(),
            Action::BooksSearchSubmit => {
                self.search.apply();
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::BooksSearchCancel => {
                self.search.cancel();
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::BooksPickerOpen => {
                if !self.items.is_empty() {
                    let items = self
                        .items
                        .iter()
                        .map(|b| PickerItem::new(book_label(b), b.id))
                        .collect();
                    self.picker = Some(Picker::new("open book", items));
                }
            }
            Action::BooksPickerKey(key) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => self.picker = None,
                    KeyCode::Enter => {
                        let id = self.picker.as_ref().and_then(|p| p.selected().copied());
                        self.picker = None;
                        if let Some(idx) =
                            id.and_then(|id| self.items.iter().position(|b| b.id == id))
                        {
                            self.state.select(Some(idx));
                            let _ = tx.send(Action::BooksOpen);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(p) = self.picker.as_mut() {
                            p.backspace();
                        }
                    }
                    // Movement is on the arrows / Ctrl-n/p — plain letters filter.
                    KeyCode::Down => self.picker_move(1),
                    KeyCode::Up => self.picker_move(-1),
                    KeyCode::Char('n') if ctrl => self.picker_move(1),
                    KeyCode::Char('p') if ctrl => self.picker_move(-1),
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(p) = self.picker.as_mut() {
                            p.input(c);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    fn picker_move(&mut self, delta: i32) {
        if let Some(p) = self.picker.as_mut() {
            p.move_cursor(delta);
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.items.len() as i32 - 1);
        self.state.select(Some(next as usize));
    }

    fn step_match(&mut self, dir: i32) {
        let matches =
            search::match_indices(self.items.iter().map(book_label_ref), &self.search.query);
        let cur = self.state.selected().unwrap_or(0);
        if let Some(next) = search::step_match(&matches, cur, dir) {
            self.state.select(Some(next));
        }
    }

    fn selected(&self) -> Option<&Book> {
        self.state.selected().and_then(|i| self.items.get(i))
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let label = match self.filter {
            BooksFilter::All => "all",
            BooksFilter::Reading => "reading",
            BooksFilter::Completed => "completed",
        };
        let title = search::title_with_query(&format!("Books · {label}"), &self.search);
        let block = bordered(title);

        if self.items.is_empty() {
            let state = if let Some(f) = &self.failure {
                PanelState::Failed(f.clone())
            } else if self.loading {
                PanelState::Loading
            } else if !self.search.is_empty() {
                PanelState::Empty {
                    hint: Some(format!("no matches for \"{}\"", self.search.query)),
                }
            } else {
                PanelState::Empty {
                    hint: Some("no books on this shelf".into()),
                }
            };
            render_panel_state(frame, area, block, &state);
            return;
        }

        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|b| {
                let mut spans = vec![widgets::status_pill(b.status), Span::raw("  ")];
                spans.extend(search::highlight(
                    &b.title,
                    &self.search.query,
                    Style::default(),
                ));
                spans.push(Span::styled(
                    format!("  · {}", b.author.clone().unwrap_or_default()),
                    theme::muted(),
                ));
                spans.push(Span::raw("    "));
                let pct = b.progress_percent.unwrap_or(0.0);
                spans.push(Span::styled(format!("{pct:>3.0}%"), theme::muted()));
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, area, &mut self.state);

        if let Some(p) = &self.picker {
            p.render(frame, area);
        }
    }

    pub fn hints(&self) -> Line<'static> {
        if self.picker.is_some() {
            return Line::from(Span::styled(
                "type to filter · ↑/↓ or ^n/^p move · ↵ open · Esc cancel",
                theme::muted(),
            ));
        }
        if self.search.active {
            return search::search_hints();
        }
        let mut hints: Vec<(&str, &str)> = vec![("j/k", "move"), ("↵", "open"), ("/", "search")];
        if !self.search.is_empty() {
            hints.push(("n/N", "match"));
        }
        hints.extend([("f", "find"), ("1/2/3", "filter"), ("h", "back")]);
        widgets::footer_hints(&hints)
    }
}

fn book_label(book: &Book) -> String {
    match &book.author {
        Some(a) if !a.is_empty() => format!("{} · {}", book.title, a),
        _ => book.title.clone(),
    }
}

/// Borrowing form for match-stepping — the title is enough to step by, and
/// avoids allocating a label per row each keystroke.
fn book_label_ref(book: &Book) -> &str {
    &book.title
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::mpsc;
    use url::Url;

    fn api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://identity.test/").unwrap(), "tok".into())
    }

    fn book(id: i64, title: &str) -> Book {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "status": "reading"
        }))
        .unwrap()
    }

    fn render(b: &mut Books) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 16)).unwrap();
        t.draw(|f| b.render(f, f.area())).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[tokio::test]
    async fn a_load_failure_renders_tier2_not_an_empty_shelf() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut b = Books::default();
        b.handle(
            Action::BooksLoadFailed("identity.test → HTTP 500".into()),
            &api(),
            &tx,
        )
        .await;
        assert!(b.failure.is_some(), "the failure is recorded");
        assert!(b.items.is_empty(), "no rows are invented on failure");
        let text = render(&mut b);
        assert!(
            text.contains("✖ couldn't load books"),
            "loud failure: {text}"
        );
        assert!(text.contains("HTTP 500"), "names the reason: {text}");
        assert!(
            !text.contains("no books on this shelf"),
            "failed is never dressed up as empty: {text}"
        );
    }

    #[tokio::test]
    async fn a_loaded_empty_shelf_is_calm_not_a_failure() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut b = Books::default();
        b.handle(Action::BooksLoaded(vec![]), &api(), &tx).await;
        let text = render(&mut b);
        assert!(
            text.contains("no books on this shelf"),
            "calm empty: {text}"
        );
        assert!(
            !text.contains("✖"),
            "no failure glyph on an empty shelf: {text}"
        );
    }

    #[tokio::test]
    async fn loading_shows_the_calm_loading_state() {
        let mut b = Books {
            loading: true,
            ..Default::default()
        };
        assert!(render(&mut b).contains("loading…"));
    }

    #[tokio::test]
    async fn a_successful_load_clears_a_prior_failure() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut b = Books::default();
        b.handle(Action::BooksLoadFailed("boom".into()), &api(), &tx)
            .await;
        b.handle(
            Action::BooksLoaded(vec![book(1, "The Rust Book")]),
            &api(),
            &tx,
        )
        .await;
        assert!(b.failure.is_none(), "a good read clears the Tier-2 failure");
        assert!(!render(&mut b).contains("✖"));
    }

    #[tokio::test]
    async fn n_steps_between_matches_over_loaded_rows() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut b = Books::default();
        b.handle(
            Action::BooksLoaded(vec![
                book(1, "The Rust Book"),
                book(2, "Go in Action"),
                book(3, "Rust Atomics"),
            ]),
            &api(),
            &tx,
        )
        .await;
        // A live query (as if `/rust` had been applied server-side and the
        // matching rows returned) — `n`/`N` step within the loaded set.
        b.search.query = "rust".into();
        b.state.select(Some(0));
        b.handle(Action::BooksMatchStep(1), &api(), &tx).await;
        assert_eq!(b.state.selected(), Some(2), "n jumps to the next match");
        b.handle(Action::BooksMatchStep(1), &api(), &tx).await;
        assert_eq!(
            b.state.selected(),
            Some(0),
            "n wraps back to the first match"
        );
        b.handle(Action::BooksMatchStep(-1), &api(), &tx).await;
        assert_eq!(b.state.selected(), Some(2), "N steps backward with wrap");
    }

    #[test]
    fn the_open_picker_owns_every_key() {
        let mut b = Books {
            items: vec![book(1, "Rust")],
            ..Books::default()
        };
        b.picker = Some(Picker::new(
            "open book",
            vec![PickerItem::new(String::from("Rust"), 1_i64)],
        ));
        for code in [KeyCode::Char('/'), KeyCode::Char('f'), KeyCode::Char('q')] {
            assert!(
                matches!(
                    b.intercept_key(KeyEvent::new(code, KeyModifiers::NONE)),
                    Some(Action::BooksPickerKey(_))
                ),
                "{code:?} filters the picker"
            );
        }
    }

    #[test]
    fn the_picker_label_carries_the_author_so_either_matches() {
        let mut b = book(1, "Crafting Interpreters");
        b.author = Some("Nystrom".into());
        assert_eq!(book_label(&b), "Crafting Interpreters · Nystrom");
    }
}
