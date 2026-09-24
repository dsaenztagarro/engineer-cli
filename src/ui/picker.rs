//! The one fuzzy picker overlay a screen mounts over its content, ranked by [`super::fuzzy`].

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::ui::{fuzzy, layout::bordered, theme};

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

pub struct Picker<T> {
    title: String,
    items: Vec<PickerItem<T>>,
    query: String,
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

    pub fn selected(&self) -> Option<&T> {
        let ranked = self.ranked();
        ranked.get(self.cursor).map(|&i| &self.items[i].value)
    }

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
        p.move_cursor(2);
        p.input('r');
        assert!(p.selected().is_some());
        for c in "ust".chars() {
            p.input(c);
        }
        assert_eq!(p.selected(), Some(&3));
        p.backspace();
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
