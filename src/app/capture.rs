//! Quick-capture overlay — the "five-second capture" half of the notes daily
//! loop (daily-loop.brief.md §5, notes.html). Reachable from *any* screen via
//! the `<Space>c` leader and rendered as a centered modal over whatever screen
//! is showing, so a thought never costs a navigation.
//!
//! **Capture is sacred** (brief §3): the overlay never loses input. An
//! accidental `Esc` on a non-empty draft only *arms* a discard — a second `Esc`
//! confirms, any other key cancels the warning and resumes editing. The draft
//! lives in `App::capture` until the note is saved or explicitly discarded.
//!
//! Fields: a multiline **content** thought (the star), an optional **book**
//! anchor (a live search over the books list, the timer bind-panel idiom), and
//! an optional **page**. Save is explicit — `Ctrl-S` (Enter is a newline in the
//! content editor). The one editor serves both new notes (POST) and edits of an
//! existing one (PATCH), opened pre-filled from the browser.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use tui_textarea::{CursorMove, TextArea};

use crate::api::{Anchor, ApiClient, Book, Note, NoteInput};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Longest first-line slice we lift into a note's title. The full text always
/// lands in `content`, so truncating the title never loses input.
const TITLE_MAX: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Content,
    Book,
    Page,
}

pub struct QuickCapture {
    content: TextArea<'static>,
    page: Input,
    field: Field,
    book_id: Option<i64>,
    book_label: Option<String>,
    /// `Some(note_id)` when editing an existing note (save → PATCH).
    editing: Option<i64>,
    pending: bool,
    /// A non-empty draft caught one `Esc`; the next `Esc` discards.
    confirm_discard: bool,
    // Book picker (live search), mirroring the timer bind panel.
    picking: bool,
    book_query: String,
    book_results: Vec<Book>,
    book_state: ListState,
}

impl Default for QuickCapture {
    fn default() -> Self {
        Self {
            content: make_textarea(""),
            page: Input::default(),
            field: Field::Content,
            book_id: None,
            book_label: None,
            editing: None,
            pending: false,
            confirm_discard: false,
            picking: false,
            book_query: String::new(),
            book_results: vec![],
            book_state: ListState::default(),
        }
    }
}

impl QuickCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// A *new* draft pre-filled with text — the `:note <text>` palette handoff.
    /// The cursor lands at the end so the user keeps typing, adds an anchor, or
    /// saves (Ctrl-S) straight away. Empty text is just a blank capture.
    pub fn with_text(text: &str) -> Self {
        let mut content = make_textarea(text);
        content.move_cursor(CursorMove::Bottom);
        content.move_cursor(CursorMove::End);
        Self {
            content,
            ..Self::default()
        }
    }

    /// The overlay pre-filled to edit an existing note — one editor, two verbs.
    pub fn for_edit(note: Note) -> Self {
        let text = note.content.clone().unwrap_or_else(|| note.title.clone());
        let mut s = Self {
            content: make_textarea(&text),
            editing: Some(note.id),
            book_id: note.book_id,
            book_label: note.book_title,
            ..Self::default()
        };
        if let Some(p) = note.citations.first().and_then(|c| c.page) {
            s.page = Input::new(p.to_string());
        }
        s
    }

    /// Map a raw key to a capture `Action`. The book picker owns keys while
    /// open (live search); otherwise `Ctrl-S` saves, `Esc` closes/warns, `Tab`
    /// cycles fields, and everything else flows to the focused field.
    pub fn translate(&self, key: KeyEvent) -> Option<Action> {
        if self.picking {
            return match key.code {
                KeyCode::Esc => Some(Action::CaptureBookPickerClose),
                KeyCode::Enter => Some(Action::CaptureBookPickerSubmit),
                KeyCode::Up => Some(Action::CaptureBookMove(-1)),
                KeyCode::Down => Some(Action::CaptureBookMove(1)),
                KeyCode::Backspace => Some(Action::CaptureBookBackspace),
                KeyCode::Char(c) => Some(Action::CaptureBookInput(c)),
                _ => None,
            };
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => Some(Action::CaptureSave),
            (KeyCode::Esc, _) => Some(Action::CaptureCancel),
            (KeyCode::Tab, _) => Some(Action::CaptureFieldNext),
            (KeyCode::BackTab, _) => Some(Action::CaptureFieldPrev),
            _ => Some(Action::CaptureKey(key)),
        }
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        match action {
            Action::CaptureKey(key) => {
                // Any real keystroke cancels a pending discard warning.
                self.confirm_discard = false;
                match self.field {
                    Field::Content => {
                        self.content.input(key);
                    }
                    Field::Book => match key.code {
                        KeyCode::Enter => self.open_picker(api, tx),
                        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                            self.book_id = None;
                            self.book_label = None;
                        }
                        _ => {}
                    },
                    Field::Page => {
                        if let KeyCode::Char(c) = key.code {
                            if !c.is_ascii_digit() {
                                return None;
                            }
                        }
                        self.page.handle_event(&Event::Key(key));
                    }
                }
            }
            Action::CaptureFieldNext => {
                self.confirm_discard = false;
                self.field = self.next_field();
            }
            Action::CaptureFieldPrev => {
                self.confirm_discard = false;
                self.field = self.prev_field();
            }
            Action::CaptureSave => {
                if self.pending {
                    return Some((Level::Warning, "saving…".into()));
                }
                if self.content_text().trim().is_empty() {
                    return Some((
                        Level::Warning,
                        "note is empty — type a thought first".into(),
                    ));
                }
                self.pending = true;
                spawn_save(api, tx, self.editing, self.build_input());
            }
            Action::CaptureSaveFailed => self.pending = false,
            Action::CaptureCancel => {
                if self.has_input() && !self.confirm_discard {
                    self.confirm_discard = true;
                    return Some((
                        Level::Warning,
                        "unsaved note — Esc again to discard · Ctrl-S to save".into(),
                    ));
                }
                let _ = tx.send(Action::CaptureClose);
            }
            Action::CaptureBookInput(c) => {
                self.book_query.push(c);
                self.book_state.select(Some(0));
                spawn_book_search(api, tx, self.book_query.clone());
            }
            Action::CaptureBookBackspace => {
                self.book_query.pop();
                self.book_state.select(Some(0));
                spawn_book_search(api, tx, self.book_query.clone());
            }
            Action::CaptureBookMove(delta) => self.move_book_selection(delta),
            Action::CaptureBookResults(list) => {
                self.book_results = list;
                if self.book_results.is_empty() {
                    self.book_state.select(None);
                } else if self.book_state.selected().unwrap_or(0) >= self.book_results.len() {
                    self.book_state.select(Some(self.book_results.len() - 1));
                }
            }
            Action::CaptureBookPickerSubmit => {
                if let Some(book) = self
                    .book_state
                    .selected()
                    .and_then(|i| self.book_results.get(i))
                {
                    self.book_id = Some(book.id);
                    self.book_label = Some(book.title.clone());
                    // Nudge toward pinning a page now the book is chosen.
                    self.field = Field::Page;
                }
                self.close_picker();
            }
            Action::CaptureBookPickerClose => self.close_picker(),
            _ => {}
        }
        None
    }

    fn open_picker(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.picking = true;
        self.book_query.clear();
        self.book_results.clear();
        self.book_state.select(Some(0));
        spawn_book_search(api, tx, String::new());
    }

    fn close_picker(&mut self) {
        self.picking = false;
        self.book_query.clear();
        self.book_results.clear();
    }

    fn move_book_selection(&mut self, delta: i32) {
        let len = self.book_results.len();
        if len == 0 {
            self.book_state.select(None);
            return;
        }
        let cur = self.book_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.book_state.select(Some(next as usize));
    }

    fn next_field(&self) -> Field {
        match self.field {
            Field::Content => Field::Book,
            Field::Book => Field::Page,
            Field::Page => Field::Content,
        }
    }

    fn prev_field(&self) -> Field {
        match self.field {
            Field::Content => Field::Page,
            Field::Book => Field::Content,
            Field::Page => Field::Book,
        }
    }

    fn content_text(&self) -> String {
        self.content.lines().join("\n")
    }

    /// True when the draft holds any input worth protecting from an accidental
    /// discard — typed content, a chosen book, or a page.
    fn has_input(&self) -> bool {
        !self.content_text().trim().is_empty()
            || self.book_id.is_some()
            || !self.page.value().trim().is_empty()
    }

    /// Build the note payload from the draft. The simplest faithful anchor is a
    /// book plus a page: `book_id` links the book, and a `page` citation pins
    /// the place. A page with no book can't be anchored, so it's dropped.
    fn build_input(&self) -> NoteInput {
        let (title, content) = derive_title_content(&self.content_text());
        let page: Option<u32> = self.page.value().trim().parse().ok();
        let anchors = match (self.book_id, page) {
            (Some(_), Some(p)) => Some(vec![Anchor {
                page: Some(p),
                ..Default::default()
            }]),
            // On an edit, `anchors: None` leaves existing anchors untouched.
            _ => None,
        };
        NoteInput {
            title,
            content,
            book_id: self.book_id,
            anchors,
            ..Default::default()
        }
    }

    fn preview(&self) -> Option<String> {
        let page = self.page.value().trim().to_string();
        match (self.book_label.as_deref(), page.is_empty()) {
            (Some(b), false) => Some(format!("anchor: {b} · p.{page}")),
            (Some(b), true) => Some(format!("anchor: {b}")),
            (None, false) => Some(format!("anchor: p.{page} — pick a book to save it")),
            (None, true) => None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let modal = centered(area, 74, 18);
        frame.render_widget(Clear, modal);
        let title = if self.editing.is_some() {
            "Edit note"
        } else {
            "Quick capture"
        };
        let block = bordered(title);
        let inner = block.inner(modal);
        frame.render_widget(block, modal);

        if self.picking {
            self.render_picker(frame, inner);
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // content editor
                Constraint::Length(1), // book
                Constraint::Length(1), // page
                Constraint::Length(1), // anchor preview
            ])
            .split(inner);

        // Only show the editor's cursor when content has focus, so an idle
        // cursor doesn't compete with the highlighted meta field.
        self.content
            .set_cursor_style(if self.field == Field::Content {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            });
        frame.render_widget(&self.content, rows[0]);

        let book_value = self
            .book_label
            .clone()
            .unwrap_or_else(|| "none — Enter to pick".to_string());
        frame.render_widget(
            Paragraph::new(field_line(
                self.field == Field::Book,
                "book",
                book_value,
                false,
            )),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(field_line(
                self.field == Field::Page,
                "page",
                self.page.value().to_string(),
                true,
            )),
            rows[2],
        );
        if let Some(preview) = self.preview() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  {preview}"),
                    theme::muted(),
                ))),
                rows[3],
            );
        }
    }

    fn render_picker(&mut self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::raw("anchor to  "),
                    Span::styled(format!("{}_", self.book_query), theme::focused()),
                ]),
                Line::from(""),
            ]),
            rows[0],
        );
        let mut items: Vec<ListItem> = self
            .book_results
            .iter()
            .map(|b| ListItem::new(Line::from(b.title.clone())))
            .collect();
        if items.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "type to search your books…",
                theme::muted(),
            ))));
        }
        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, rows[1], &mut self.book_state);
    }

    pub fn hints(&self) -> Line<'static> {
        if self.picking {
            return Line::from(Span::styled(
                "type to search · ↑/↓ pick · ↵ select · Esc back",
                theme::muted(),
            ));
        }
        if self.confirm_discard {
            return Line::from(Span::styled(
                "unsaved — Esc again to discard · Ctrl-S to save",
                theme::focused(),
            ));
        }
        widgets::footer_hints(&[
            ("^S", "save"),
            ("Tab", "field"),
            ("↵", "book/newline"),
            ("Esc", "close"),
        ])
    }
}

fn field_line(focused: bool, label: &str, value: String, cursor: bool) -> Line<'static> {
    let marker = if focused { "▌ " } else { "  " };
    let value = if focused && cursor {
        format!("{value}_")
    } else {
        value
    };
    Line::from(vec![
        Span::styled(marker.to_string(), theme::focused()),
        Span::styled(format!("{label}  "), theme::muted()),
        Span::raw(value),
    ])
}

fn make_textarea(text: &str) -> TextArea<'static> {
    let lines: Vec<String> = if text.is_empty() {
        vec![String::new()]
    } else {
        text.split('\n').map(str::to_string).collect()
    };
    let mut ta = TextArea::new(lines);
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("type your thought…");
    ta
}

/// Split a captured thought into `(title, content)`: the title is the first
/// non-empty line (clipped to `TITLE_MAX`), and the full text is kept verbatim
/// in `content` so nothing is lost. The server's note model requires a title;
/// quick-capture is content-first, so we derive one.
pub(crate) fn derive_title_content(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    let first = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let title: String = first.chars().take(TITLE_MAX).collect();
    (title, Some(trimmed.to_string()))
}

/// A centered rectangle `pct`% as wide as `area` (capped at `area`), `max_h`
/// tall, so the overlay stays readable at 100×30 and degrades to 80×24.
fn centered(area: Rect, pct: u16, max_h: u16) -> Rect {
    let w = (area.width * pct / 100).clamp(1, area.width);
    let h = max_h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn spawn_book_search(api: &ApiClient, tx: &UnboundedSender<Action>, query: String) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let q = query.trim();
        let q = if q.is_empty() { None } else { Some(q) };
        if let Ok(list) = api.list_books(None, q).await {
            let _ = tx.send(Action::CaptureBookResults(list.data));
        }
    });
}

fn spawn_save(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    editing: Option<i64>,
    body: NoteInput,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let res = match editing {
            Some(id) => api.update_note(id, &body).await,
            None => api.create_note(&body).await,
        };
        match res {
            Ok(_) => {
                let _ = tx.send(Action::CaptureSaved);
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("save failed: {e}"),
                });
                let _ = tx.send(Action::CaptureSaveFailed);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use tokio::sync::mpsc;

    fn setup() -> (
        QuickCapture,
        ApiClient,
        mpsc::UnboundedSender<Action>,
        mpsc::UnboundedReceiver<Action>,
    ) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        (QuickCapture::new(), api, tx, rx)
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    async fn type_content(
        s: &mut QuickCapture,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        text: &str,
    ) {
        for c in text.chars() {
            s.handle(Action::CaptureKey(key(c)), api, tx).await;
        }
    }

    #[tokio::test]
    async fn typing_records_content_and_marks_input_present() {
        let (mut s, api, tx, _rx) = setup();
        type_content(&mut s, &api, &tx, "blocks").await;
        assert_eq!(s.content_text(), "blocks");
        assert!(s.has_input());
    }

    #[tokio::test]
    async fn esc_on_nonempty_draft_arms_discard_and_keeps_the_draft() {
        let (mut s, api, tx, mut rx) = setup();
        type_content(&mut s, &api, &tx, "keep me").await;

        // First Esc only warns — nothing is dispatched, the draft survives.
        let warn = s.handle(Action::CaptureCancel, &api, &tx).await;
        assert!(matches!(warn, Some((Level::Warning, _))));
        assert!(s.confirm_discard);
        assert!(rx.try_recv().is_err(), "no CaptureClose on the first Esc");
        assert_eq!(s.content_text(), "keep me");
    }

    #[tokio::test]
    async fn second_esc_confirms_the_discard() {
        let (mut s, api, tx, mut rx) = setup();
        type_content(&mut s, &api, &tx, "keep me").await;
        s.handle(Action::CaptureCancel, &api, &tx).await;
        s.handle(Action::CaptureCancel, &api, &tx).await;
        assert!(matches!(rx.try_recv(), Ok(Action::CaptureClose)));
    }

    #[tokio::test]
    async fn a_keystroke_after_the_warning_resumes_editing() {
        let (mut s, api, tx, _rx) = setup();
        type_content(&mut s, &api, &tx, "x").await;
        s.handle(Action::CaptureCancel, &api, &tx).await;
        assert!(s.confirm_discard);
        s.handle(Action::CaptureKey(key('y')), &api, &tx).await;
        assert!(!s.confirm_discard, "typing cancels the pending discard");
        assert_eq!(s.content_text(), "xy");
    }

    #[tokio::test]
    async fn esc_on_an_empty_draft_closes_immediately() {
        let (mut s, api, tx, mut rx) = setup();
        s.handle(Action::CaptureCancel, &api, &tx).await;
        assert!(matches!(rx.try_recv(), Ok(Action::CaptureClose)));
    }

    #[tokio::test]
    async fn save_with_empty_content_warns_and_does_not_dispatch() {
        let (mut s, api, tx, mut rx) = setup();
        let out = s.handle(Action::CaptureSave, &api, &tx).await;
        assert!(matches!(out, Some((Level::Warning, _))));
        assert!(!s.pending);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn save_with_content_marks_pending() {
        let (mut s, api, tx, _rx) = setup();
        type_content(&mut s, &api, &tx, "a real thought").await;
        s.handle(Action::CaptureSave, &api, &tx).await;
        assert!(s.pending);
    }

    #[test]
    fn derive_title_content_lifts_first_line_and_keeps_full_text() {
        let (title, content) = derive_title_content("closures are objects\n\nthe env model\n");
        assert_eq!(title, "closures are objects");
        assert_eq!(
            content.as_deref(),
            Some("closures are objects\n\nthe env model")
        );
    }

    #[test]
    fn derive_title_content_empty_is_empty() {
        let (title, content) = derive_title_content("   \n  ");
        assert!(title.is_empty());
        assert!(content.is_none());
    }

    #[test]
    fn build_input_anchors_a_book_and_page() {
        let mut s = QuickCapture::new();
        s.content = make_textarea("SICP blocks");
        s.book_id = Some(3);
        s.page = Input::new("142".into());
        let body = s.build_input();
        assert_eq!(body.book_id, Some(3));
        assert_eq!(body.title, "SICP blocks");
        let anchors = body.anchors.expect("a page yields an anchor");
        assert_eq!(anchors[0].page, Some(142));
    }

    #[test]
    fn build_input_book_only_links_without_an_anchor() {
        let mut s = QuickCapture::new();
        s.content = make_textarea("loose-ish");
        s.book_id = Some(9);
        let body = s.build_input();
        assert_eq!(body.book_id, Some(9));
        assert!(body.anchors.is_none());
    }

    #[test]
    fn build_input_loose_note_has_no_book_or_anchor() {
        let mut s = QuickCapture::new();
        s.content = make_textarea("just a thought");
        let body = s.build_input();
        assert!(body.book_id.is_none());
        assert!(body.anchors.is_none());
    }

    #[test]
    fn with_text_prefills_a_new_draft() {
        let s = QuickCapture::with_text("closures are objects");
        // Content is prefilled but this is a fresh note, not an edit.
        assert_eq!(s.content_text(), "closures are objects");
        assert!(s.editing.is_none());
        assert!(s.has_input());
    }

    #[test]
    fn for_edit_prefills_content_book_and_editing_id() {
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 55, "title": "old title", "content": "old body\nline two",
            "book_id": 3, "book_title": "SICP",
            "citations": [{ "id": 1, "page": 42 }]
        }))
        .unwrap();
        let s = QuickCapture::for_edit(note);
        assert_eq!(s.editing, Some(55));
        assert_eq!(s.content_text(), "old body\nline two");
        assert_eq!(s.book_id, Some(3));
        assert_eq!(s.page.value(), "42");
    }
}
