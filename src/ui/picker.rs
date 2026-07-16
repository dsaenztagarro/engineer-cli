//! A reusable Telescope-flavoured fuzzy picker overlay (`cross-cutting.brief.md` §B).
//!
//! One source-agnostic widget any screen mounts over its content: `j`/`k` move,
//! type to filter, `⏎` picks, `Esc` cancels — the neovim grammar the footer
//! already advertises. It ranks with [`super::fuzzy`] (a subsequence match, not
//! the substring narrow the lists use), and renders from shipped atoms only
//! (`bordered()`, `▌` selection, dim-vs-bright). A module picks a *source* — a
//! local slice of books / repos / domains / activities, or a candidate stream —
//! not a bespoke screen, so every pick feels identical.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::ui::{fuzzy, layout::bordered, theme};

/// One choice: what the user sees (`label`, matched fuzzily) and what the caller
/// gets back on pick (`value`).
pub struct PickerItem<T> {
    pub label: String,
    pub value: T,
}

impl<T> PickerItem<T> {
    pub fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }
}

/// A modal fuzzy picker over a fixed set of `T`. The owning screen holds it as an
/// `Option<Picker<T>>`, routes keys to it while open, and reads [`selected`] on
/// `⏎`.
pub struct Picker<T> {
    title: String,
    items: Vec<PickerItem<T>>,
    query: String,
    /// Cursor into the *filtered* view (reset to 0 whenever the query changes).
    cursor: usize,
}

impl<T> Picker<T> {
    pub fn new(title: impl Into<String>, items: Vec<PickerItem<T>>) -> Self {
        Self {
            title: title.into(),
            items,
            query: String::new(),
            cursor: 0,
        }
    }

    pub fn input(&mut self, c: char) {
        self.query.push(c);
        self.cursor = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.cursor = 0;
    }

    pub fn move_cursor(&mut self, delta: i32) {
        let n = self.ranked().len() as i32;
        if n > 0 {
            self.cursor = (self.cursor as i32 + delta).clamp(0, n - 1) as usize;
        }
    }

    /// Indices into `items` that match the query, best score first. The sort is
    /// stable, so equal-scoring matches keep their input order.
    fn ranked(&self) -> Vec<usize> {
        let mut scored: Vec<(usize, i32)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy::score(&self.query, &it.label).map(|s| (i, s)))
            .collect();
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// The value under the cursor, or `None` when the query filters everything out.
    pub fn selected(&self) -> Option<&T> {
        let ranked = self.ranked();
        ranked.get(self.cursor).map(|&i| &self.items[i].value)
    }

    /// The label under the cursor — for callers that echo the chosen row.
    pub fn selected_label(&self) -> Option<&str> {
        let ranked = self.ranked();
        ranked
            .get(self.cursor)
            .map(|&i| self.items[i].label.as_str())
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let overlay = centered_rect(60, 60, area);
        let ranked = self.ranked();
        let title = format!(
            "{}  ·  {} match{}",
            self.title,
            ranked.len(),
            if ranked.len() == 1 { "" } else { "es" }
        );
        let block = bordered(title);
        let inner = block.inner(overlay);
        frame.render_widget(Clear, overlay);
        frame.render_widget(block, overlay);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);

        let query_line = Line::from(vec![
            Span::styled("> ", theme::focused()),
            Span::raw(self.query.clone()),
            Span::styled("█", theme::muted()),
        ]);
        frame.render_widget(Paragraph::new(query_line), chunks[0]);

        if ranked.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled("no matches", theme::muted())),
                chunks[1],
            );
            return;
        }

        let rows: Vec<ListItem> = ranked
            .iter()
            .map(|&i| ListItem::new(self.items[i].label.clone()))
            .collect();
        let list = List::new(rows)
            .highlight_style(theme::selection())
            .highlight_symbol("▌ ");
        let mut state = ListState::default();
        state.select(Some(self.cursor));
        frame.render_stateful_widget(list, chunks[1], &mut state);
    }
}

/// A rect `pct_x` × `pct_y` percent of `area`, centered — the modal footprint.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn books() -> Picker<i64> {
        Picker::new(
            "book",
            vec![
                PickerItem::new("Designing Data-Intensive Applications", 1),
                PickerItem::new("Structure and Interpretation of Computer Programs", 2),
                PickerItem::new("The Rust Programming Language", 3),
            ],
        )
    }

    #[test]
    fn empty_query_keeps_all_in_input_order() {
        let p = books();
        assert_eq!(p.ranked(), vec![0, 1, 2]);
        assert_eq!(p.selected(), Some(&1));
    }

    #[test]
    fn typing_fuzzy_filters_and_reranks() {
        let mut p = books();
        for c in "dda".chars() {
            p.input(c);
        }
        let ranked = p.ranked();
        // "dda" is a subsequence of the DDIA title only.
        assert_eq!(ranked.len(), 1);
        assert_eq!(p.selected(), Some(&1));
    }

    #[test]
    fn cursor_moves_within_filtered_and_clamps() {
        let mut p = books();
        p.move_cursor(1);
        assert_eq!(p.selected(), Some(&2));
        p.move_cursor(50);
        assert_eq!(p.selected(), Some(&3), "clamped to the last row");
        p.move_cursor(-50);
        assert_eq!(p.selected(), Some(&1), "clamped to the first row");
    }

    #[test]
    fn query_change_resets_cursor_and_backspace_restores() {
        let mut p = books();
        p.move_cursor(2); // on "Rust"
        p.input('r'); // matches DDIA, SICP (interpretation), Rust... reranks, cursor -> 0
        assert!(p.selected().is_some());
        // Narrow to only Rust, then widen again.
        for c in "ust".chars() {
            p.input(c);
        }
        assert_eq!(p.selected(), Some(&3));
        p.backspace(); // "rus"
        assert_eq!(p.selected(), Some(&3));
    }

    #[test]
    fn no_matches_selects_nothing() {
        let mut p = books();
        for c in "zzzz".chars() {
            p.input(c);
        }
        assert!(p.ranked().is_empty());
        assert_eq!(p.selected(), None);
    }
}
