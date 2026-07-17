//! Notes browser — the "findable later" half of the notes daily loop
//! (daily-loop.brief.md §5, notes.html). Quick-capture (the "five-second"
//! half) is the app-level overlay in `src/app/capture.rs`; this screen lists,
//! searches, reads, archives, and hands a note back to that overlay for edits.
//!
//! A loose thought and an anchored one sit side by side: an anchored note reads
//! its place back in one line of grid text (`SICP · ch 3 · p.142`); a loose one
//! is a single row with no anchor line. Archived notes (revealed with `t`) are
//! dimmed in place rather than hidden, so the ledger stays legible.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, Note, NoteFilters};
use crate::app::action::Action;
use crate::queue::WriteOutcome;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

use super::{notify_seam_error, open_queued, QueuePaths};

pub struct Notes {
    items: Vec<Note>,
    state: ListState,
    query: String,
    searching: bool,
    /// When set, archived notes are folded back in (dimmed) via the `archived=all`
    /// server filter; otherwise the list is active-only.
    show_archived: bool,
    loading: bool,
    /// `Some` while the full-content detail read is open, over the list.
    detail: Option<Note>,
    /// A permanent-delete caught its first press on the detail; the next press
    /// of the same key confirms, any other key disarms (guarded, live-only).
    delete_armed: bool,
    /// Queue + read-cache locations for the offline write seam — `None`
    /// (production) uses the shared XDG paths, tests inject a scratch dir.
    queue_paths: QueuePaths,
}

impl Default for Notes {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            items: vec![],
            state,
            query: String::new(),
            searching: false,
            show_archived: false,
            loading: false,
            detail: None,
            delete_armed: false,
            queue_paths: None,
        }
    }
}

impl Notes {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        self.fetch(api, tx);
    }

    fn fetch(&self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        let (api, tx) = (api.clone(), tx.clone());
        let filters = NoteFilters {
            q: if self.query.is_empty() {
                None
            } else {
                Some(self.query.clone())
            },
            // "all" keeps active notes and folds archived ones back in (dimmed);
            // None is active-only. We never request archived-only here.
            archived: self.show_archived.then(|| "all".to_string()),
            ..Default::default()
        };
        tokio::spawn(async move {
            match api.list_notes(&filters).await {
                Ok(list) => {
                    let _ = tx.send(Action::NotesLoaded(list.data));
                }
                Err(e) => {
                    let _ = tx.send(Action::Notify {
                        level: Level::Error,
                        text: format!("notes load failed: {e}"),
                    });
                    let _ = tx.send(Action::NotesLoaded(vec![]));
                }
            }
        });
    }

    /// The detail read and the search prompt own keys before the global keymap.
    pub fn intercept_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.detail.is_some() {
            // A pending delete confirmation intercepts the very next key: the
            // same `X` confirms, anything else cancels (guarded gesture — the
            // note is only destroyed on the deliberate second press).
            if self.delete_armed {
                return Some(if key.code == KeyCode::Char('X') {
                    Action::NotesDeleteConfirm
                } else {
                    Action::NotesDeleteDisarm
                });
            }
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('h') | KeyCode::Char('q') => {
                    Some(Action::NotesCloseDetail)
                }
                // `u` = detach from book (unlink), distinct from `a` archive.
                KeyCode::Char('u') => Some(Action::NotesUnlinkSelected),
                // `X` (shift) arms the permanent delete — deliberate, far from
                // `a`, and never a bare key in the browse list.
                KeyCode::Char('X') => Some(Action::NotesDeleteArm),
                _ => None,
            };
        }
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
            KeyCode::Esc => Some(Action::NotesSearchCancel),
            KeyCode::Enter => Some(Action::NotesSearchSubmit),
            KeyCode::Backspace => Some(Action::NotesSearchBackspace),
            KeyCode::Char(c) => Some(Action::NotesSearchInput(c)),
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
            Action::NotesLoaded(notes) => {
                self.items = notes;
                self.loading = false;
                let len = self.items.len();
                if self.state.selected().unwrap_or(0) >= len {
                    self.state
                        .select(if len == 0 { None } else { Some(len - 1) });
                } else if len > 0 && self.state.selected().is_none() {
                    self.state.select(Some(0));
                }
            }
            Action::NotesMove(d) => self.move_cursor(d),
            Action::NotesJumpStart => {
                self.state
                    .select(if self.items.is_empty() { None } else { Some(0) });
            }
            Action::NotesJumpEnd => {
                if !self.items.is_empty() {
                    self.state.select(Some(self.items.len() - 1));
                }
            }
            Action::NotesSearchInput(c) => self.query.push(c),
            Action::NotesSearchBackspace => {
                self.query.pop();
            }
            Action::NotesSearchSubmit => {
                self.searching = false;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::NotesSearchCancel => {
                self.searching = false;
                self.query.clear();
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::NotesToggleArchived => {
                self.show_archived = !self.show_archived;
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::RefreshNotes => {
                self.loading = true;
                self.fetch(api, tx);
            }
            Action::NotesOpenDetail => {
                if let Some(note) = self.selected().cloned() {
                    // Open instantly from the list row, then refine with the full
                    // record (content + citations the list may omit).
                    let id = note.id;
                    self.detail = Some(note);
                    let (api, tx) = (api.clone(), tx.clone());
                    tokio::spawn(async move {
                        if let Ok(full) = api.get_note(id).await {
                            let _ = tx.send(Action::NotesDetailLoaded(Box::new(full)));
                        }
                    });
                }
            }
            Action::NotesDetailLoaded(note) => {
                if self.detail.is_some() {
                    self.detail = Some(*note);
                }
            }
            Action::NotesCloseDetail => {
                self.detail = None;
                self.delete_armed = false;
            }
            Action::NotesArchiveSelected => {
                if let Some(note) = self.selected().cloned() {
                    let archived = note.archived_at.is_some();
                    let (api, tx) = (api.clone(), tx.clone());
                    let paths = self.queue_paths.clone();
                    tokio::spawn(async move {
                        let queued = match open_queued(&api, &paths) {
                            Ok(q) => q,
                            Err(e) => return notify_seam_error(&tx, "archive failed", e),
                        };
                        let res = if archived {
                            queued.unarchive_note(note.id).await
                        } else {
                            queued.archive_note(note.id).await
                        };
                        match res {
                            Ok(WriteOutcome::Confirmed(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: if archived {
                                        "note unarchived".into()
                                    } else {
                                        "note archived".into()
                                    },
                                });
                                let _ = tx.send(Action::RefreshNotes);
                            }
                            Ok(WriteOutcome::Provisional(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Info,
                                    text: if archived {
                                        "unarchived · queued (offline) — will sync".into()
                                    } else {
                                        "archived · queued (offline) — will sync".into()
                                    },
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("archive failed: {e}"),
                                });
                            }
                        }
                    });
                }
            }
            Action::NotesEditSelected => {
                if let Some(note) = self.selected().cloned() {
                    // Hand the note to the app-level quick-capture overlay,
                    // pre-filled for a PATCH — one editor for new and existing.
                    let _ = tx.send(Action::CaptureOpenEdit(Box::new(note)));
                }
            }
            Action::NotesUnlinkSelected => {
                // Detach the open note from its book — the note survives, only
                // its book anchor is severed (a different intent from archive).
                let note = self.detail.as_ref().or_else(|| self.selected());
                match note {
                    Some(n) if n.book_id.is_some() || n.book_title.is_some() => {
                        let id = n.id;
                        let (api, tx) = (api.clone(), tx.clone());
                        let paths = self.queue_paths.clone();
                        tokio::spawn(async move {
                            let queued = match open_queued(&api, &paths) {
                                Ok(q) => q,
                                Err(e) => return notify_seam_error(&tx, "unlink failed", e),
                            };
                            match queued.unlink_note(id).await {
                                Ok(WriteOutcome::Confirmed(_)) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Success,
                                        text: "detached from book".into(),
                                    });
                                    let _ = tx.send(Action::NotesCloseDetail);
                                    let _ = tx.send(Action::RefreshNotes);
                                }
                                Ok(WriteOutcome::Provisional(_)) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Info,
                                        text: "detached · queued (offline) — will sync".into(),
                                    });
                                    let _ = tx.send(Action::NotesCloseDetail);
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Notify {
                                        level: Level::Error,
                                        text: format!("unlink failed: {e}"),
                                    });
                                }
                            }
                        });
                    }
                    Some(_) => {
                        return Some((Level::Warning, "not linked to a book".into()));
                    }
                    None => {}
                }
            }
            Action::NotesDeleteArm => {
                self.delete_armed = true;
                return Some((
                    Level::Warning,
                    "delete (permanent) — press X again to confirm".into(),
                ));
            }
            Action::NotesDeleteDisarm => {
                self.delete_armed = false;
                return Some((Level::Info, "delete cancelled".into()));
            }
            Action::NotesDeleteConfirm => {
                self.delete_armed = false;
                // Delete is destructive and terminal — the one note write kept
                // live-only (edit/archive/unarchive/unlink now ride the queue,
                // #111), with an honest offline refusal (the #94/#95
                // triage/connect precedent): a synthesized offline "deleted" would
                // be a lie the queue can't walk back, so it needs the server.
                if let Some(note) = self.detail.as_ref().or_else(|| self.selected()) {
                    let id = note.id;
                    let (api, tx) = (api.clone(), tx.clone());
                    tokio::spawn(async move {
                        match api.delete_note(id).await {
                            Ok(()) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Success,
                                    text: "deleted (permanent)".into(),
                                });
                                let _ = tx.send(Action::NotesCloseDetail);
                                let _ = tx.send(Action::RefreshNotes);
                            }
                            Err(crate::api::ApiError::Transport(_)) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: "offline — delete needs the server; retry online".into(),
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::Notify {
                                    level: Level::Error,
                                    text: format!("delete failed: {e}"),
                                });
                            }
                        }
                    });
                }
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

    fn selected(&self) -> Option<&Note> {
        self.state.selected().and_then(|i| self.items.get(i))
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(note) = self.detail.clone() {
            self.render_detail(frame, area, &note);
            return;
        }

        let title = if self.searching || !self.query.is_empty() {
            format!("Notes · /{}_", self.query)
        } else if self.show_archived {
            "Notes · incl. archived".to_string()
        } else {
            "Notes".to_string()
        };
        let block = bordered(title);

        if self.loading && self.items.is_empty() {
            frame.render_widget(Paragraph::new("loading…").block(block), area);
            return;
        }
        if self.items.is_empty() {
            let msg = if self.query.is_empty() {
                "No notes yet. Press <Space>c anywhere to capture one."
            } else {
                "No notes match that search."
            };
            frame.render_widget(Paragraph::new(msg).block(block), area);
            return;
        }

        let items: Vec<ListItem> = self.items.iter().map(note_list_item).collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect, note: &Note) {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            note_headline(note),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        match anchor_line(note) {
            Some(anchor) => lines.push(Line::from(Span::styled(anchor, theme::focused()))),
            None => lines.push(Line::from(Span::styled("loose note", theme::muted()))),
        }
        if note.archived_at.is_some() {
            lines.push(Line::from(Span::styled("archived", theme::muted())));
        }
        if self.delete_armed {
            // The destructive gesture reads red and unmistakable — never the
            // quiet dim of archive.
            lines.push(Line::from(Span::styled(
                "✖ delete (permanent) — X again to confirm, cannot be undone",
                Style::default()
                    .fg(theme::DANGER)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));
        for raw in note.content.as_deref().unwrap_or("").split('\n') {
            lines.push(Line::from(raw.to_string()));
        }
        let cites: Vec<&str> = note
            .citations
            .iter()
            .filter_map(|c| c.address_label.as_deref())
            .collect();
        if !cites.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("citations", theme::muted())));
            for c in cites {
                lines.push(Line::from(Span::styled(format!("  · {c}"), theme::muted())));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(bordered("Note"))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    pub fn hints(&self) -> Line<'static> {
        if self.delete_armed {
            return Line::from(Span::styled(
                "delete (permanent) — X again to confirm · any key cancels",
                theme::focused(),
            ));
        }
        if self.detail.is_some() {
            return widgets::footer_hints(&[
                ("e", "edit"),
                ("a", "archive"),
                ("u", "detach book"),
                ("X", "delete"),
                ("q", "back"),
            ]);
        }
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
            ("a", "archive"),
            ("e", "edit"),
            ("t", "archived"),
            ("h", "back"),
        ])
    }
}

/// The one-line preview: the note's title, falling back to the first non-empty
/// line of its content for a note that was captured content-first.
pub(crate) fn note_headline(note: &Note) -> String {
    let title = note.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    note.content
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(empty note)")
        .to_string()
}

/// The one-line anchor read-back for an anchored note — `book · ch 3 · p.142`.
/// `None` for a loose thought (no book). The place is the first citation's
/// server-rendered `address_label`, falling back to a bare `p.N` from its page.
pub(crate) fn anchor_line(note: &Note) -> Option<String> {
    if note.book_id.is_none() && note.book_title.is_none() {
        return None;
    }
    let book = note.book_title.as_deref().unwrap_or("book");
    let place = note.citations.first().and_then(|c| {
        c.address_label
            .clone()
            .or_else(|| c.page.map(|p| format!("p.{p}")))
    });
    Some(match place {
        Some(p) => format!("{book} · {p}"),
        None => book.to_string(),
    })
}

fn note_list_item(note: &Note) -> ListItem<'static> {
    let archived = note.archived_at.is_some();
    let head_style = if archived {
        theme::muted()
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(note_headline(note), head_style),
        if archived {
            Span::styled("  (archived)", theme::muted())
        } else {
            Span::raw("")
        },
    ])];
    if let Some(anchor) = anchor_line(note) {
        lines.push(Line::from(Span::styled(
            format!("  {anchor}"),
            theme::muted(),
        )));
    }
    ListItem::new(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use tokio::sync::mpsc;

    fn setup() -> (Notes, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Notes::default(), api, tx)
    }

    fn note(json: serde_json::Value) -> Note {
        serde_json::from_value(json).unwrap()
    }

    async fn feed(
        s: &mut Notes,
        api: &ApiClient,
        tx: &mpsc::UnboundedSender<Action>,
        action: Action,
    ) {
        s.handle(action, api, tx).await;
    }

    #[tokio::test]
    async fn loaded_sets_items_and_clamps_selection() {
        let (mut s, api, tx) = setup();
        s.state.select(Some(9)); // stale selection past the new list
        feed(
            &mut s,
            &api,
            &tx,
            Action::NotesLoaded(vec![
                note(serde_json::json!({ "id": 1, "title": "a" })),
                note(serde_json::json!({ "id": 2, "title": "b" })),
            ]),
        )
        .await;
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.state.selected(), Some(1));
    }

    #[tokio::test]
    async fn loaded_empty_deselects() {
        let (mut s, api, tx) = setup();
        feed(&mut s, &api, &tx, Action::NotesLoaded(vec![])).await;
        assert_eq!(s.state.selected(), None);
    }

    #[tokio::test]
    async fn move_clamps_within_bounds() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::NotesLoaded(vec![
                note(serde_json::json!({ "id": 1, "title": "a" })),
                note(serde_json::json!({ "id": 2, "title": "b" })),
            ]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::NotesMove(-5)).await;
        assert_eq!(s.state.selected(), Some(0));
        feed(&mut s, &api, &tx, Action::NotesMove(5)).await;
        assert_eq!(s.state.selected(), Some(1));
    }

    #[tokio::test]
    async fn search_input_builds_query_then_submit_exits_search() {
        let (mut s, api, tx) = setup();
        s.searching = true;
        for c in "blocks".chars() {
            feed(&mut s, &api, &tx, Action::NotesSearchInput(c)).await;
        }
        assert_eq!(s.query, "blocks");
        feed(&mut s, &api, &tx, Action::NotesSearchSubmit).await;
        assert!(!s.searching);
        assert_eq!(s.query, "blocks"); // submit keeps the term applied
    }

    #[tokio::test]
    async fn cancel_search_clears_query() {
        let (mut s, api, tx) = setup();
        s.searching = true;
        s.query = "x".into();
        feed(&mut s, &api, &tx, Action::NotesSearchCancel).await;
        assert!(!s.searching);
        assert!(s.query.is_empty());
    }

    #[tokio::test]
    async fn toggle_archived_flips_the_flag() {
        let (mut s, api, tx) = setup();
        assert!(!s.show_archived);
        feed(&mut s, &api, &tx, Action::NotesToggleArchived).await;
        assert!(s.show_archived);
        feed(&mut s, &api, &tx, Action::NotesToggleArchived).await;
        assert!(!s.show_archived);
    }

    #[tokio::test]
    async fn open_then_close_detail_transitions() {
        let (mut s, api, tx) = setup();
        feed(
            &mut s,
            &api,
            &tx,
            Action::NotesLoaded(vec![note(serde_json::json!({ "id": 7, "title": "a" }))]),
        )
        .await;
        feed(&mut s, &api, &tx, Action::NotesOpenDetail).await;
        assert!(s.detail.is_some());
        feed(&mut s, &api, &tx, Action::NotesCloseDetail).await;
        assert!(s.detail.is_none());
    }

    #[tokio::test]
    async fn edit_selected_dispatches_capture_open_edit() {
        let (mut s, api, tx) = setup();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        feed(
            &mut s,
            &api,
            &tx2,
            Action::NotesLoaded(vec![note(
                serde_json::json!({ "id": 42, "title": "edit me" }),
            )]),
        )
        .await;
        s.handle(Action::NotesEditSelected, &api, &tx2).await;
        let got = rx2.try_recv().expect("an action was dispatched");
        match got {
            Action::CaptureOpenEdit(n) => assert_eq!(n.id, 42),
            other => panic!("expected CaptureOpenEdit, got {other:?}"),
        }
        drop(tx);
    }

    #[test]
    fn anchor_line_reads_back_book_and_address_label() {
        let n = note(serde_json::json!({
            "id": 1, "title": "closures",
            "book_id": 3, "book_title": "SICP",
            "citations": [{ "id": 1, "address_label": "ch 3 · p.142", "page": 142 }]
        }));
        assert_eq!(anchor_line(&n).as_deref(), Some("SICP · ch 3 · p.142"));
    }

    #[test]
    fn anchor_line_falls_back_to_bare_page() {
        let n = note(serde_json::json!({
            "id": 1, "title": "t", "book_title": "TAPL",
            "citations": [{ "id": 1, "page": 88 }]
        }));
        assert_eq!(anchor_line(&n).as_deref(), Some("TAPL · p.88"));
    }

    #[test]
    fn anchor_line_is_none_for_a_loose_note() {
        let n = note(serde_json::json!({ "id": 1, "title": "just a thought" }));
        assert_eq!(anchor_line(&n), None);
    }

    #[test]
    fn headline_falls_back_to_content_head_when_title_blank() {
        let n = note(serde_json::json!({
            "id": 1, "title": "", "content": "\n  first real line\nsecond"
        }));
        assert_eq!(note_headline(&n), "first real line");
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    #[test]
    fn detail_maps_unlink_and_delete_keys_only_while_open() {
        let mut s = Notes {
            detail: Some(note(
                serde_json::json!({ "id": 4, "title": "t", "book_id": 11 }),
            )),
            ..Notes::default()
        };
        assert!(matches!(
            s.intercept_key(key('u')),
            Some(Action::NotesUnlinkSelected)
        ));
        assert!(matches!(
            s.intercept_key(key('X')),
            Some(Action::NotesDeleteArm)
        ));
        assert!(matches!(
            s.intercept_key(key('q')),
            Some(Action::NotesCloseDetail)
        ));
        // In the browse list, the delete key is inert — never a bare list delete.
        s.detail = None;
        assert!(s.intercept_key(key('X')).is_none());
        assert!(s.intercept_key(key('u')).is_none());
    }

    #[tokio::test]
    async fn delete_arms_then_a_second_press_confirms() {
        let (mut s, api, tx) = setup();
        s.detail = Some(note(serde_json::json!({ "id": 9, "title": "doomed" })));

        // First X arms and warns; the note is not yet touched.
        assert!(matches!(
            s.intercept_key(key('X')),
            Some(Action::NotesDeleteArm)
        ));
        let out = s.handle(Action::NotesDeleteArm, &api, &tx).await;
        assert!(matches!(out, Some((Level::Warning, _))));
        assert!(s.delete_armed);

        // A second X (while armed) confirms → the wire call fires; disarmed after.
        assert!(matches!(
            s.intercept_key(key('X')),
            Some(Action::NotesDeleteConfirm)
        ));
        s.handle(Action::NotesDeleteConfirm, &api, &tx).await;
        assert!(!s.delete_armed);
    }

    #[tokio::test]
    async fn delete_disarms_on_any_other_key() {
        let (mut s, api, tx) = setup();
        s.detail = Some(note(serde_json::json!({ "id": 9, "title": "doomed" })));
        s.intercept_key(key('X'));
        s.handle(Action::NotesDeleteArm, &api, &tx).await;
        assert!(s.delete_armed);

        // Any non-X key while armed cancels — the note survives.
        assert!(matches!(
            s.intercept_key(key('j')),
            Some(Action::NotesDeleteDisarm)
        ));
        let out = s.handle(Action::NotesDeleteDisarm, &api, &tx).await;
        assert!(matches!(out, Some((Level::Info, _))));
        assert!(!s.delete_armed);
    }

    #[tokio::test]
    async fn closing_the_detail_disarms_a_pending_delete() {
        let (mut s, api, tx) = setup();
        s.detail = Some(note(serde_json::json!({ "id": 9, "title": "doomed" })));
        s.handle(Action::NotesDeleteArm, &api, &tx).await;
        assert!(s.delete_armed);
        s.handle(Action::NotesCloseDetail, &api, &tx).await;
        assert!(!s.delete_armed);
        assert!(s.detail.is_none());
    }

    #[tokio::test]
    async fn unlink_a_loose_note_refuses_honestly() {
        let (mut s, api, tx) = setup();
        // A note with no book can't be detached — refuse rather than call.
        s.detail = Some(note(serde_json::json!({ "id": 5, "title": "loose" })));
        let out = s.handle(Action::NotesUnlinkSelected, &api, &tx).await;
        assert!(matches!(out, Some((Level::Warning, _))));
        // The note stays open (nothing was severed).
        assert!(s.detail.is_some());
    }

    // --- offline write seam (#111): archive / unlink queue through QueuedClient ---

    /// A base URL nothing listens on — reqwest fails before any response, which
    /// is exactly `ApiError::Transport`, the offline seam's trigger.
    fn dead_api() -> ApiClient {
        ApiClient::with_token(
            url::Url::parse("http://127.0.0.1:1/").unwrap(),
            "tok".into(),
        )
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "engineer-notes-screen-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Wait for the spawned offline write's Notify (its enqueue completes just
    /// before it), so the queue read below is race-free.
    async fn await_notify(rx: &mut mpsc::UnboundedReceiver<Action>) -> Action {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a notify within 5s")
            .expect("an action")
    }

    #[tokio::test]
    async fn offline_archive_queues_a_note_archive_intent() {
        let dir = scratch("archive");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Notes {
            items: vec![note(serde_json::json!({ "id": 42, "title": "shelve me" }))],
            queue_paths: Some((dir.join("queue.json"), dir.join("timer-cache.json"))),
            ..Notes::default()
        };
        s.handle(Action::NotesArchiveSelected, &dead_api(), &tx)
            .await;

        // Offline → the Provisional arm fires an Info "queued" notify.
        let got = await_notify(&mut rx).await;
        assert!(matches!(
            got,
            Action::Notify {
                level: Level::Info,
                ..
            }
        ));

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "archive");
        assert_eq!(intents[0].stream, "note:42");
    }

    #[tokio::test]
    async fn offline_unlink_queues_a_note_unlink_intent() {
        let dir = scratch("unlink");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Notes {
            detail: Some(note(
                serde_json::json!({ "id": 4, "title": "linked", "book_id": 11 }),
            )),
            queue_paths: Some((dir.join("queue.json"), dir.join("timer-cache.json"))),
            ..Notes::default()
        };
        s.handle(Action::NotesUnlinkSelected, &dead_api(), &tx)
            .await;

        let got = await_notify(&mut rx).await;
        assert!(matches!(
            got,
            Action::Notify {
                level: Level::Info,
                ..
            }
        ));

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "unlink");
        assert_eq!(intents[0].stream, "note:4");
    }
}
