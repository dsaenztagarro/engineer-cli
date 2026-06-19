use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Book, BookChapter, BookStatus, BookUpdate};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

pub struct BookDetail {
    book: Option<Book>,
    chapters: Vec<BookChapter>,
    state: ListState,
    edit_page: Option<String>,
}

impl Default for BookDetail {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { book: None, chapters: vec![], state, edit_page: None }
    }
}

impl BookDetail {
    pub fn on_enter(&mut self, _api: &ApiClient, _tx: &UnboundedSender<Action>) {
        // Loaded by Books::BooksOpen which emits BookDetailLoaded.
    }

    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.edit_page.is_none() {
            return None;
        }
        match key.code {
            KeyCode::Esc => Some(Action::CancelEdit),
            KeyCode::Enter => Some(Action::SubmitPage),
            KeyCode::Backspace => Some(Action::EditPageBackspace),
            KeyCode::Char(c) if c.is_ascii_digit() => Some(Action::EditPageInput(c)),
            _ => None,
        }
    }

    pub async fn handle(&mut self, action: Action, api: &ApiClient, tx: &UnboundedSender<Action>) -> Option<(Level, String)> {
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
                        let id = book.id;
                        let api = api.clone();
                        let tx = tx.clone();
                        let body = BookUpdate { current_page: Some(page), ..Default::default() };
                        tokio::spawn(async move {
                            match api.update_book(id, &body).await {
                                Ok(b) => {
                                    let _ = tx.send(Action::BookUpdated(Box::new(b)));
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Success,
                                        text: "page updated".into(),
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
                }
                self.edit_page = None;
            }
            Action::ToggleChapterDone => {
                if let Some(chapter) = self.selected_chapter().cloned() {
                    if let Some(book) = &self.book {
                        let id = book.id;
                        let chapter_id = chapter.id;
                        let api = api.clone();
                        let tx = tx.clone();
                        // Mark this chapter as current and advance cursor.
                        let body = BookUpdate { current_chapter_id: Some(chapter_id), ..Default::default() };
                        tokio::spawn(async move {
                            match api.update_book(id, &body).await {
                                Ok(b) => {
                                    let _ = tx.send(Action::BookUpdated(Box::new(b)));
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Error,
                                        text: format!("update failed: {e}"),
                                    });
                                }
                            }
                        });
                        self.move_cursor(1);
                    }
                }
            }
            Action::BookStatusPicker => {
                return Some((Level::Info, "status picker: r/c/u/h/a (TODO modal)".into()))
            }
            Action::PickStatus(s) => {
                if let Some(book) = &self.book {
                    let id = book.id;
                    let api = api.clone();
                    let tx = tx.clone();
                    let body = BookUpdate { status: Some(s), ..Default::default() };
                    tokio::spawn(async move {
                        if let Ok(b) = api.update_book(id, &body).await {
                            let _ = tx.send(Action::BookUpdated(Box::new(b)));
                        }
                    });
                }
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
                Span::styled(format!("  · {}", book.author.clone().unwrap_or_default()), theme::muted()),
            ]),
            widgets::progress_bar(pct, 40),
        ];
        if let Some(p) = book.current_page {
            header_lines.push(Line::from(Span::styled(
                format!(
                    "page {}{}",
                    p,
                    book.page_count.map(|t| format!(" / {t}")).unwrap_or_default()
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
                let mark = if c.done { "✓" } else if c.skipped { "·" } else { " " };
                let is_current = Some(c.id) == cur_id;
                let style = if is_current { theme::focused() } else { ratatui::style::Style::default() };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" [{}] ", mark), if c.done { theme::focused() } else { theme::muted() }),
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
    }

    pub fn hints(&self) -> Line<'static> {
        if self.edit_page.is_some() {
            return Line::from(Span::styled("page · digits + Enter · Esc cancel", theme::muted()));
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

#[allow(dead_code)]
const _: BookStatus = BookStatus::Reading;
