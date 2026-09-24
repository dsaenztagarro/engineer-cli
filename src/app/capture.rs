//! The quick-capture overlay: a modal note editor reachable from any screen (ADR 0005).

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

use crate::api::{
    derive_title_content, Anchor, AnchorData, ApiClient, ApiError, Book, Note, NoteInput,
};
use crate::app::action::Action;
use crate::app::screens::{open_queued, QueuePaths};
use crate::messages;
use crate::queue::WriteOutcome;
use crate::ui::notify::Level;
use crate::ui::picker::{Picker, PickerItem};
use crate::ui::{layout::bordered, theme, widgets};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Content,
    Book,
    Anchor,
    Page,
}

/// `echo` is only a composing label; the durable read-back is the server's
/// `address_label`, never re-derived here.
#[derive(Clone)]
struct AnchorChoice {
    chapter_id: Option<i64>,
    section_id: Option<i64>,
    echo: String,
}

pub struct QuickCapture {
    content: TextArea<'static>,
    page: Input,
    field: Field,
    book_id: Option<i64>,
    book_label: Option<String>,
    chapter_id: Option<i64>,
    section_id: Option<i64>,
    anchor_echo: Option<String>,
    anchor_data: Option<AnchorData>,
    anchor_failure: Option<String>,
    anchor_loading: bool,
    anchor_picker: Option<Picker<AnchorChoice>>,
    anchor_touched: bool,
    editing: Option<i64>,
    pending: bool,
    confirm_discard: bool,
    picking: bool,
    book_query: String,
    book_results: Vec<Book>,
    book_state: ListState,
    queue_paths: QueuePaths,
}

impl Default for QuickCapture {
    fn default() -> Self {
        Self {
            content: make_textarea(""),
            page: Input::default(),
            field: Field::Content,
            book_id: None,
            book_label: None,
            chapter_id: None,
            section_id: None,
            anchor_echo: None,
            anchor_data: None,
            anchor_failure: None,
            anchor_loading: false,
            anchor_picker: None,
            anchor_touched: false,
            editing: None,
            pending: false,
            confirm_discard: false,
            picking: false,
            book_query: String::new(),
            book_results: vec![],
            book_state: ListState::default(),
            queue_paths: None,
        }
    }
}

impl QuickCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(text: &str) -> Self {
        let mut content = make_textarea(text);
        content.move_cursor(CursorMove::Bottom);
        content.move_cursor(CursorMove::End);
        Self {
            content,
            ..Self::default()
        }
    }

    pub fn for_edit(note: Note) -> Self {
        let text = note.content.clone().unwrap_or_else(|| note.title.clone());
        let cite = note.citations.first();
        let mut s = Self {
            content: make_textarea(&text),
            editing: Some(note.id),
            book_id: note.book_id,
            book_label: note.book_title,
            chapter_id: cite.and_then(|c| c.chapter_id),
            section_id: cite.and_then(|c| c.section_id),
            anchor_echo: cite.and_then(|c| c.address_label.clone()),
            ..Self::default()
        };
        if let Some(p) = cite.and_then(|c| c.page) {
            s.page = Input::new(p.to_string());
        }
        s
    }

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
        if self.anchor_picker.is_some() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            return match key.code {
                KeyCode::Esc => Some(Action::CaptureAnchorPickerClose),
                KeyCode::Enter => Some(Action::CaptureAnchorPickerSubmit),
                KeyCode::Up => Some(Action::CaptureAnchorMove(-1)),
                KeyCode::Down => Some(Action::CaptureAnchorMove(1)),
                KeyCode::Char('n') if ctrl => Some(Action::CaptureAnchorMove(1)),
                KeyCode::Char('p') if ctrl => Some(Action::CaptureAnchorMove(-1)),
                KeyCode::Backspace => Some(Action::CaptureAnchorBackspace),
                KeyCode::Char(c) if !ctrl => Some(Action::CaptureAnchorInput(c)),
                _ => None,
            };
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => Some(Action::CaptureSave),
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => Some(Action::CaptureEditExternal),
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
                self.confirm_discard = false;
                match self.field {
                    Field::Content => {
                        self.content.input(key);
                    }
                    Field::Book => match key.code {
                        KeyCode::Enter => self.open_picker(api, tx),
                        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                            self.clear_book();
                        }
                        _ => {}
                    },
                    Field::Anchor => match key.code {
                        // Re-dispatched so the picker-open notify returns through
                        // the reducer.
                        KeyCode::Enter => {
                            let _ = tx.send(Action::CaptureAnchorPickerOpen);
                        }
                        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                            self.clear_anchor();
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
                        self.anchor_touched = true;
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
                spawn_save(
                    api,
                    tx,
                    self.queue_paths.clone(),
                    self.editing,
                    self.build_input(),
                );
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
                    let changed = self.book_id != Some(book.id);
                    self.book_id = Some(book.id);
                    self.book_label = Some(book.title.clone());
                    if changed {
                        self.chapter_id = None;
                        self.section_id = None;
                        self.anchor_echo = None;
                        self.anchor_data = None;
                        self.anchor_touched = true;
                    }
                    self.field = Field::Anchor;
                }
                self.close_picker();
            }
            Action::CaptureBookPickerClose => self.close_picker(),
            Action::CaptureAnchorPickerOpen => return self.open_anchor_picker(api, tx),
            Action::CaptureAnchorDataLoaded(data) => {
                self.anchor_data = Some(*data);
                self.anchor_loading = false;
                self.anchor_failure = None;
                return self.mount_anchor_picker();
            }
            Action::CaptureAnchorDataFailed(reason) => {
                self.anchor_loading = false;
                self.anchor_failure = Some(reason);
            }
            Action::CaptureAnchorInput(c) => {
                if let Some(p) = self.anchor_picker.as_mut() {
                    p.input(c);
                }
            }
            Action::CaptureAnchorBackspace => {
                if let Some(p) = self.anchor_picker.as_mut() {
                    p.backspace();
                }
            }
            Action::CaptureAnchorMove(delta) => {
                if let Some(p) = self.anchor_picker.as_mut() {
                    p.move_cursor(delta);
                }
            }
            Action::CaptureAnchorPickerSubmit => {
                if let Some(choice) = self.anchor_picker.as_ref().and_then(|p| p.selected()) {
                    self.chapter_id = choice.chapter_id;
                    self.section_id = choice.section_id;
                    self.anchor_echo = Some(choice.echo.clone());
                    self.anchor_touched = true;
                    self.field = Field::Page;
                }
                self.anchor_picker = None;
            }
            Action::CaptureAnchorPickerClose => self.anchor_picker = None,
            _ => {}
        }
        None
    }

    fn open_anchor_picker(
        &mut self,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        let Some(book_id) = self.book_id else {
            return Some((Level::Warning, "pick a book first".into()));
        };
        if self.anchor_data.is_some() {
            return self.mount_anchor_picker();
        }
        self.anchor_failure = None;
        self.anchor_loading = true;
        spawn_anchor_data(api, tx, book_id);
        Some((Level::Info, "loading chapters…".into()))
    }

    fn mount_anchor_picker(&mut self) -> Option<(Level, String)> {
        let items = self
            .anchor_data
            .as_ref()
            .map(anchor_items)
            .unwrap_or_default();
        if items.is_empty() {
            return Some((Level::Warning, "no chapters to anchor for this book".into()));
        }
        self.anchor_picker = Some(Picker::new("chapter · §section", items));
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

    fn clear_book(&mut self) {
        self.book_id = None;
        self.book_label = None;
        self.clear_anchor();
        self.anchor_data = None;
    }

    fn clear_anchor(&mut self) {
        self.chapter_id = None;
        self.section_id = None;
        self.anchor_echo = None;
        self.anchor_touched = true;
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
            Field::Book => Field::Anchor,
            Field::Anchor => Field::Page,
            Field::Page => Field::Content,
        }
    }

    fn prev_field(&self) -> Field {
        match self.field {
            Field::Content => Field::Page,
            Field::Book => Field::Content,
            Field::Anchor => Field::Book,
            Field::Page => Field::Anchor,
        }
    }

    fn content_text(&self) -> String {
        self.content.lines().join("\n")
    }

    pub fn body(&self) -> String {
        self.content_text()
    }

    pub fn set_content(&mut self, text: &str) {
        self.content = make_textarea(text);
        self.content.move_cursor(CursorMove::Bottom);
        self.content.move_cursor(CursorMove::End);
    }

    fn has_input(&self) -> bool {
        !self.content_text().trim().is_empty()
            || self.book_id.is_some()
            || self.chapter_id.is_some()
            || self.section_id.is_some()
            || !self.page.value().trim().is_empty()
    }

    fn build_input(&self) -> NoteInput {
        let (title, content) = derive_title_content(&self.content_text());
        let anchors = if self.editing.is_some() && !self.anchor_touched {
            None
        } else {
            self.current_anchors()
        };
        NoteInput {
            title,
            content,
            book_id: self.book_id,
            anchors,
            ..Default::default()
        }
    }

    fn current_anchors(&self) -> Option<Vec<Anchor>> {
        self.book_id?;
        let page: Option<u32> = self.page.value().trim().parse().ok();
        if self.chapter_id.is_none() && self.section_id.is_none() && page.is_none() {
            return None;
        }
        Some(vec![Anchor {
            chapter_id: self.chapter_id,
            section_id: self.section_id,
            page,
            ..Default::default()
        }])
    }

    fn preview(&self) -> Option<String> {
        let page = self.page.value().trim();
        let mut place = self.anchor_echo.clone().unwrap_or_default();
        if !page.is_empty() {
            place = if place.is_empty() {
                format!("p.{page}")
            } else {
                format!("{place} · p.{page}")
            };
        }
        match (self.book_label.as_deref(), place.is_empty()) {
            (Some(b), false) => Some(format!("anchor: {b} · {place}")),
            (Some(b), true) => Some(format!("anchor: {b}")),
            (None, false) => Some(format!("anchor: {place} — pick a book to save it")),
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
        if let Some(picker) = &self.anchor_picker {
            picker.render(frame, inner);
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // content editor
                Constraint::Length(1), // book
                Constraint::Length(1), // chapter/§ anchor
                Constraint::Length(1), // page
                Constraint::Length(1), // anchor preview
            ])
            .split(inner);

        // Hide the editor's cursor off-focus so it doesn't compete with the
        // highlighted field.
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
        if let Some(reason) = &self.anchor_failure {
            let danger = Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  chapter/§  ", theme::muted()),
                    Span::styled("✖ couldn't load chapters", danger),
                    Span::styled("  ⏎ retry", theme::focused()),
                    // The reason trails so the headline and retry survive
                    // truncation.
                    Span::styled(format!("  ·  {reason}"), theme::muted()),
                ])),
                rows[2],
            );
        } else {
            let anchor_value = match (&self.anchor_echo, self.book_id.is_some()) {
                (Some(echo), _) => echo.clone(),
                (None, true) => "none — Enter to pick".to_string(),
                (None, false) => "— pick a book first".to_string(),
            };
            frame.render_widget(
                Paragraph::new(field_line(
                    self.field == Field::Anchor,
                    "chapter/§",
                    anchor_value,
                    false,
                )),
                rows[2],
            );
        }
        frame.render_widget(
            Paragraph::new(field_line(
                self.field == Field::Page,
                "page",
                self.page.value().to_string(),
                true,
            )),
            rows[3],
        );
        if let Some(preview) = self.preview() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  {preview}"),
                    theme::muted(),
                ))),
                rows[4],
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
        if self.anchor_picker.is_some() {
            return Line::from(Span::styled(
                "type to filter · ↑/↓ chapter/§ · ↵ pin · Esc back",
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
            ("^E", "$EDITOR"),
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

fn spawn_anchor_data(api: &ApiClient, tx: &UnboundedSender<Action>, book_id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.book_anchor_data(book_id).await {
            Ok(data) => {
                let _ = tx.send(Action::CaptureAnchorDataLoaded(Box::new(data)));
            }
            Err(ApiError::Unauthorized) => {
                let _ = tx.send(Action::SessionExpired);
            }
            Err(e) => {
                let _ = tx.send(Action::CaptureAnchorDataFailed(messages::fail_reason(
                    api.host(),
                    &e,
                )));
            }
        }
    });
}

fn anchor_items(data: &AnchorData) -> Vec<PickerItem<AnchorChoice>> {
    let mut items = Vec::new();
    for ch in &data.chapters {
        let ch_place = match ch.number {
            Some(n) => format!("ch {n}"),
            None => format!("ch #{}", ch.id),
        };
        let ch_label = match &ch.title {
            Some(t) => format!("{ch_place} · {t}"),
            None => ch_place.clone(),
        };
        items.push(PickerItem::new(
            ch_label,
            AnchorChoice {
                chapter_id: Some(ch.id),
                section_id: None,
                echo: ch_place.clone(),
            },
        ));
        for sec in &ch.sections {
            let sec_place = match &sec.number {
                Some(n) => format!("§{n}"),
                None => format!("§#{}", sec.id),
            };
            let sec_label = match &sec.title {
                Some(t) => format!("  {sec_place} · {t}"),
                None => format!("  {sec_place}"),
            };
            items.push(PickerItem::new(
                sec_label,
                AnchorChoice {
                    chapter_id: Some(ch.id),
                    section_id: Some(sec.id),
                    echo: format!("{ch_place} · {sec_place}"),
                },
            ));
        }
    }
    items
}

fn spawn_save(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    paths: QueuePaths,
    editing: Option<i64>,
    body: NoteInput,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        let queued = match open_queued(&api, &paths) {
            Ok(q) => q,
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("save failed: {e}"),
                });
                let _ = tx.send(Action::CaptureSaveFailed);
                return;
            }
        };
        let res = match editing {
            Some(id) => queued.update_note(id, &body).await,
            None => queued.create_note(&body).await,
        };
        match res {
            Ok(WriteOutcome::Confirmed(_)) => {
                let _ = tx.send(Action::CaptureSaved);
            }
            Ok(WriteOutcome::Provisional(_)) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Info,
                    text: "queued (offline) — will sync".into(),
                });
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

    fn render_capture(s: &mut QuickCapture) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut t = Terminal::new(TestBackend::new(90, 24)).unwrap();
        t.draw(|f| s.render(f, f.area())).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
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

    /// A base URL nothing listens on — reqwest fails before any response, which
    /// is exactly `ApiError::Transport`, the offline seam's trigger.
    fn dead_api() -> ApiClient {
        ApiClient::with_token(
            url::Url::parse("http://127.0.0.1:1/").unwrap(),
            "tok".into(),
        )
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("engineer-capture-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn save_with_content_marks_pending() {
        let (mut s, api, tx, _rx) = setup();
        // A scratch queue so the spawned save never touches the shared XDG state.
        let dir = scratch("pending");
        s.queue_paths = Some((dir.join("queue.json"), dir.join("timer-cache.json")));
        type_content(&mut s, &api, &tx, "a real thought").await;
        s.handle(Action::CaptureSave, &api, &tx).await;
        assert!(s.pending);
    }

    #[tokio::test]
    async fn offline_edit_save_queues_a_note_update_with_the_body() {
        let dir = scratch("edit");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 55, "title": "MVCC", "content": "MVCC keeps one version",
            "book_id": 11, "book_title": "SICP"
        }))
        .unwrap();
        let mut s = QuickCapture::for_edit(note);
        s.queue_paths = Some((dir.join("queue.json"), dir.join("timer-cache.json")));
        s.handle(Action::CaptureSave, &dead_api(), &tx).await;
        assert!(s.pending);

        let mut saw_saved = false;
        for _ in 0..3 {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("an action within 5s")
                .expect("an action")
            {
                Action::CaptureSaved => {
                    saw_saved = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(
            saw_saved,
            "an offline save still confirms (the draft is safe)"
        );

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "edit");
        assert_eq!(intents[0].stream, "note:55");
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

    fn sicp_anchor_data() -> AnchorData {
        serde_json::from_value(serde_json::json!({
            "chapters": [{
                "id": 3, "number": 3, "title": "Modularity, Objects, and State",
                "sections": [
                    { "id": 31, "number": "3.1", "title": "Assignment and Local State" },
                    { "id": 32, "number": "3.2", "title": "The Environment Model" }
                ]
            }]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn anchor_picker_open_without_a_book_warns_and_does_not_mount() {
        let (mut s, api, tx, _rx) = setup();
        let out = s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        assert!(matches!(out, Some((Level::Warning, _))));
        assert!(s.anchor_picker.is_none());
    }

    #[tokio::test]
    async fn anchor_picker_fetch_then_load_mounts_the_picker() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        let out = s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        assert!(matches!(out, Some((Level::Info, _))));
        assert!(s.anchor_loading);
        assert!(s.anchor_picker.is_none());
        s.handle(
            Action::CaptureAnchorDataLoaded(Box::new(sicp_anchor_data())),
            &api,
            &tx,
        )
        .await;
        assert!(!s.anchor_loading);
        assert!(s.anchor_picker.is_some());
    }

    #[tokio::test]
    async fn a_failed_anchor_read_marks_the_field_and_keeps_the_draft() {
        let (mut s, api, tx, _rx) = setup();
        type_content(&mut s, &api, &tx, "MVCC keeps one version").await;
        s.book_id = Some(11);
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        assert!(s.anchor_loading, "the fetch is in flight");
        s.handle(
            Action::CaptureAnchorDataFailed("identity.test → HTTP 500".into()),
            &api,
            &tx,
        )
        .await;
        assert!(!s.anchor_loading, "the stuck-spinner bug is fixed");
        assert!(s.anchor_failure.is_some(), "the failure is recorded");
        assert_eq!(
            s.content_text(),
            "MVCC keeps one version",
            "the draft survives an anchor failure"
        );
        assert!(s.anchor_picker.is_none());
        let text = render_capture(&mut s);
        assert!(
            text.contains("✖ couldn't load chapters"),
            "scoped Tier-2 in the anchor field: {text}"
        );
        assert!(text.contains("retry"), "offers a retry: {text}");
    }

    #[tokio::test]
    async fn reopening_a_failed_anchor_retries_and_clears_the_failure() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        s.anchor_failure = Some("boom".into());
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        assert!(s.anchor_failure.is_none(), "retry clears the prior failure");
        assert!(s.anchor_loading, "retry re-kicks the fetch");
    }

    #[tokio::test]
    async fn anchor_picker_pins_a_chapter_then_a_section_into_the_body() {
        let (mut s, api, tx, _rx) = setup();
        s.content = make_textarea("MVCC keeps one version");
        s.book_id = Some(11);
        s.anchor_data = Some(sicp_anchor_data());

        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        assert!(s.anchor_picker.is_some());

        s.handle(Action::CaptureAnchorPickerSubmit, &api, &tx).await;
        assert_eq!(s.chapter_id, Some(3));
        assert_eq!(s.section_id, None);
        assert!(s.anchor_touched);
        assert!(s.anchor_picker.is_none());
        let anchors = s.build_input().anchors.expect("a chapter yields an anchor");
        assert_eq!(anchors[0].chapter_id, Some(3));
        assert_eq!(anchors[0].section_id, None);

        // Rows: chapter, §3.1, §3.2.
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        s.handle(Action::CaptureAnchorMove(2), &api, &tx).await;
        s.handle(Action::CaptureAnchorPickerSubmit, &api, &tx).await;
        assert_eq!(s.chapter_id, Some(3));
        assert_eq!(s.section_id, Some(32));
        let anchors = s.build_input().anchors.expect("a section yields an anchor");
        assert_eq!(anchors[0].chapter_id, Some(3));
        assert_eq!(anchors[0].section_id, Some(32));
    }

    #[tokio::test]
    async fn anchor_picker_filter_then_cancel_leaves_the_anchor_untouched() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        s.anchor_data = Some(sicp_anchor_data());
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        s.handle(Action::CaptureAnchorInput('e'), &api, &tx).await;
        s.handle(Action::CaptureAnchorPickerClose, &api, &tx).await;
        assert!(s.anchor_picker.is_none());
        assert_eq!(s.chapter_id, None);
        assert!(!s.anchor_touched);
    }

    #[test]
    fn edit_without_touching_the_anchor_omits_it_from_the_patch() {
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 55, "title": "MVCC", "content": "MVCC keeps one version",
            "book_id": 3, "book_title": "SICP",
            "citations": [{ "id": 1, "chapter_id": 3, "page": 142 }]
        }))
        .unwrap();
        let s = QuickCapture::for_edit(note);
        assert!(!s.anchor_touched);
        let body = s.build_input();
        assert!(
            body.anchors.is_none(),
            "an untouched edit omits anchors (citations stay put)"
        );
        assert_eq!(body.book_id, Some(3));
    }

    #[tokio::test]
    async fn edit_that_pins_a_new_anchor_replaces_it_on_the_patch() {
        let (_ignored, api, tx, _rx) = setup();
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 55, "title": "MVCC", "content": "MVCC keeps one version",
            "book_id": 11, "book_title": "SICP",
            "citations": [{ "id": 1, "page": 142 }]
        }))
        .unwrap();
        let mut s = QuickCapture::for_edit(note);
        s.anchor_data = Some(sicp_anchor_data());
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        s.handle(Action::CaptureAnchorPickerSubmit, &api, &tx).await;
        assert!(s.anchor_touched);
        let anchors = s.build_input().anchors.expect("the touched anchor is sent");
        assert_eq!(anchors[0].chapter_id, Some(3));
    }

    #[test]
    fn for_edit_seeds_chapter_and_section_from_the_citation() {
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 7, "title": "t", "book_id": 11, "book_title": "SICP",
            "citations": [{
                "id": 1, "chapter_id": 3, "section_id": 32,
                "page": 294, "address_label": "ch 3 · §3.2 · p.294"
            }]
        }))
        .unwrap();
        let s = QuickCapture::for_edit(note);
        assert_eq!(s.chapter_id, Some(3));
        assert_eq!(s.section_id, Some(32));
        assert_eq!(s.anchor_echo.as_deref(), Some("ch 3 · §3.2 · p.294"));
        assert!(!s.anchor_touched);
    }

    fn book(id: i64) -> Book {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("book {id}"), "status": "reading"
        }))
        .unwrap()
    }

    async fn pick_book(
        s: &mut QuickCapture,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        id: i64,
    ) {
        s.picking = true;
        s.book_state.select(Some(0));
        s.handle(Action::CaptureBookResults(vec![book(id)]), api, tx)
            .await;
        s.handle(Action::CaptureBookPickerSubmit, api, tx).await;
    }

    #[tokio::test]
    async fn picking_a_different_book_drops_the_old_chapter_anchor() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        s.chapter_id = Some(3);
        s.anchor_data = Some(sicp_anchor_data());
        pick_book(&mut s, &api, &tx, 12).await;
        assert_eq!(s.book_id, Some(12));
        assert_eq!(s.chapter_id, None);
        assert!(s.anchor_data.is_none());
        assert!(s.anchor_touched);
    }

    #[tokio::test]
    async fn repicking_the_same_book_keeps_its_chapter_anchor() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        s.chapter_id = Some(3);
        pick_book(&mut s, &api, &tx, 11).await;
        assert_eq!(s.chapter_id, Some(3));
        assert!(!s.anchor_touched);
    }

    #[tokio::test]
    async fn a_chosen_book_moves_focus_to_the_chapter_anchor_and_a_pinned_chapter_to_the_page() {
        let (mut s, api, tx, _rx) = setup();
        pick_book(&mut s, &api, &tx, 11).await;
        assert!(s.field == Field::Anchor);
        s.anchor_data = Some(sicp_anchor_data());
        s.handle(Action::CaptureAnchorPickerOpen, &api, &tx).await;
        s.handle(Action::CaptureAnchorPickerSubmit, &api, &tx).await;
        assert!(s.field == Field::Page);
    }

    #[tokio::test]
    async fn clearing_the_book_drops_its_chapter_anchor() {
        let (mut s, api, tx, _rx) = setup();
        s.book_id = Some(11);
        s.chapter_id = Some(3);
        s.section_id = Some(32);
        s.anchor_data = Some(sicp_anchor_data());
        s.field = Field::Book;
        s.handle(Action::CaptureKey(key('x')), &api, &tx).await;
        assert_eq!(s.book_id, None);
        assert_eq!((s.chapter_id, s.section_id), (None, None));
        assert!(s.anchor_data.is_none());
    }

    #[tokio::test]
    async fn the_page_field_takes_digits_only() {
        let (mut s, api, tx, _rx) = setup();
        s.field = Field::Page;
        type_content(&mut s, &api, &tx, "p4a2").await;
        assert_eq!(s.page.value(), "42");
    }

    #[tokio::test]
    async fn editing_the_page_of_an_existing_note_replaces_its_citation() {
        let (_ignored, api, tx, _rx) = setup();
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 55, "title": "MVCC", "content": "MVCC keeps one version",
            "book_id": 3, "book_title": "SICP",
            "citations": [{ "id": 1, "page": 142 }]
        }))
        .unwrap();
        let mut s = QuickCapture::for_edit(note);
        s.field = Field::Page;
        s.handle(Action::CaptureKey(key('3')), &api, &tx).await;
        let anchors = s
            .build_input()
            .anchors
            .expect("a page edit is an anchor edit");
        assert_eq!(anchors[0].page, Some(1423));
    }

    #[tokio::test]
    async fn a_draft_holding_only_a_book_is_still_protected_from_esc() {
        let (mut s, api, tx, mut rx) = setup();
        s.book_id = Some(11);
        let warn = s.handle(Action::CaptureCancel, &api, &tx).await;
        assert!(matches!(warn, Some((Level::Warning, _))));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_save_while_one_is_in_flight_is_not_resent() {
        let (mut s, api, tx, mut rx) = setup();
        s.content = make_textarea("a real thought");
        s.pending = true;
        let out = s.handle(Action::CaptureSave, &api, &tx).await;
        assert!(matches!(out, Some((Level::Warning, _))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_page_without_a_book_is_not_pinned() {
        let mut s = QuickCapture::new();
        s.content = make_textarea("loose");
        s.page = Input::new("12".into());
        assert!(s.build_input().anchors.is_none());
        assert!(s.preview().unwrap().contains("pick a book to save it"));
    }

    #[tokio::test]
    async fn a_prefilled_draft_keeps_typing_at_the_end() {
        let (_ignored, api, tx, _rx) = setup();
        let mut s = QuickCapture::with_text("closures are\nobjects");
        type_content(&mut s, &api, &tx, "!").await;
        assert_eq!(s.content_text(), "closures are\nobjects!");
    }

    #[tokio::test]
    async fn a_401_on_the_chapter_read_routes_to_reauth_not_the_field() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let api = ApiClient::with_token(url::Url::parse(&server.uri()).unwrap(), "tok".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_anchor_data(&api, &tx, 11);
        let action = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("an action within 5s")
            .expect("an action");
        assert!(matches!(action, Action::SessionExpired), "{action:?}");
    }
}
