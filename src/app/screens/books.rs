use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Book, BookStatus};
use crate::app::action::{Action, BooksFilter};
use crate::app::screens::ScreenKind;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

pub struct Books {
    pub items: Vec<Book>,
    pub state: ListState,
    pub filter: BooksFilter,
    pub query: String,
    pub searching: bool,
    pub loading: bool,
}

impl Default for Books {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            items: vec![],
            state,
            filter: BooksFilter::Reading,
            query: String::new(),
            searching: false,
            loading: false,
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
        let q = if self.query.is_empty() {
            None
        } else {
            Some(self.query.clone())
        };
        tokio::spawn(async move {
            let q_ref = q.as_deref();
            match api.list_books(status, q_ref).await {
                Ok(list) => {
                    let _ = tx.send(Action::BooksLoaded(list.data));
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("books load failed: {e}"),
                    });
                    // Clear the loading state with an empty result.
                    let _ = tx.send(Action::BooksLoaded(vec![]));
                }
            }
        });
    }

    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if !self.searching {
            if matches!(key.code, KeyCode::Char('/')) {
                self.searching = true;
                self.query.clear();
                return Some(Action::Notify {
                    level: Level::Info,
                    text: "search: type then Enter".into(),
                });
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
                if self.state.selected().unwrap_or(0) >= self.items.len() {
                    self.state
                        .select(if self.items.is_empty() { None } else { Some(0) });
                }
            }
            Action::BooksFilter(f) => {
                self.filter = f;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::BooksMove(d) => self.move_cursor(d),
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
                    let _ = tx.send(Action::Goto(ScreenKind::BookDetail));
                    tokio::spawn(async move {
                        let chapters = match api.list_chapters(book.id).await {
                            Ok(list) => list.data,
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("chapters load failed: {e}"),
                                });
                                vec![]
                            }
                        };
                        let _ = tx.send(Action::BookDetailLoaded {
                            book: Box::new(book),
                            chapters,
                        });
                    });
                }
            }
            Action::BooksSearchInput(c) => self.query.push(c),
            Action::BooksSearchBackspace => {
                self.query.pop();
            }
            Action::BooksSearchSubmit => {
                self.searching = false;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::BooksSearchCancel => {
                self.searching = false;
                self.query.clear();
                self.loading = true;
                self.fetch(api, tx);
            }
            _ => {}
        }
        None
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.items.len() as i32 - 1);
        self.state.select(Some(next as usize));
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
        let title = if self.searching || !self.query.is_empty() {
            format!("Books · {} · /{}_", label, self.query)
        } else {
            format!("Books · {}", label)
        };
        let block = bordered(title);

        if self.loading && self.items.is_empty() {
            frame.render_widget(Paragraph::new("loading…").block(block), area);
            return;
        }

        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|b| {
                let pct = b.progress_percent.unwrap_or(0.0);
                let line = Line::from(vec![
                    widgets::status_pill(b.status),
                    Span::raw("  "),
                    Span::raw(b.title.clone()),
                    Span::styled(
                        format!("  · {}", b.author.clone().unwrap_or_default()),
                        theme::muted(),
                    ),
                    Span::raw("    "),
                    Span::styled(format!("{pct:>3.0}%"), theme::muted()),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, area, &mut self.state);
    }

    pub fn hints(&self) -> Line<'static> {
        if self.searching {
            return Line::from(Span::styled(
                "type to search · Enter to apply · Esc to cancel",
                theme::muted(),
            ));
        }
        widgets::footer_hints(&[
            ("j/k", "move"),
            ("↵", "open"),
            ("/", "search"),
            ("1/2/3", "filter"),
            ("h", "back"),
        ])
    }
}
