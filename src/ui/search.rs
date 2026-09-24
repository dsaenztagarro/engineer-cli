//! The `/` search state the list screens share; it never touches the network.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme;

#[derive(Debug, Default, Clone)]
pub struct SearchBox {
    pub query: String,
    pub active: bool,
}

impl SearchBox {
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
    }

    pub fn input(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn backspace(&mut self) {
        self.query.pop();
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
    }

    pub fn apply(&mut self) {
        self.active = false;
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }
}

pub fn title_with_query(base: &str, sb: &SearchBox) -> String {
    if sb.query.is_empty() && !sb.active {
        return base.to_string();
    }
    let caret = if sb.active { "▊" } else { "" };
    format!("{base} · /{}{}", sb.query, caret)
}

pub fn highlight(label: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(label.to_string(), base)];
    }
    let match_style = Style::default().bg(theme::ACCENT).fg(Color::Black);
    let hay = label.to_lowercase();
    let needle = query.to_lowercase();
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Byte offsets shared by `label` and `hay` — valid only while lowercasing
    // keeps byte lengths, which holds for ASCII.
    let mut cursor = 0usize;
    while let Some(rel) = hay[cursor..].find(&needle) {
        let start = cursor + rel;
        let end = start + needle.len();
        if start > cursor {
            spans.push(Span::styled(label[cursor..start].to_string(), base));
        }
        spans.push(Span::styled(label[start..end].to_string(), match_style));
        cursor = end;
    }
    if cursor < label.len() {
        spans.push(Span::styled(label[cursor..].to_string(), base));
    }
    spans
}

pub fn match_indices<'a>(labels: impl Iterator<Item = &'a str>, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    labels
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

pub fn step_match(matches: &[usize], current: usize, dir: i32) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if dir >= 0 {
        Some(
            matches
                .iter()
                .find(|&&m| m > current)
                .copied()
                .unwrap_or(matches[0]),
        )
    } else {
        Some(
            matches
                .iter()
                .rev()
                .find(|&&m| m < current)
                .copied()
                .unwrap_or(*matches.last().unwrap()),
        )
    }
}

pub fn search_hints() -> Line<'static> {
    Line::styled(
        "type to search · ↵ apply · n/N next match · Esc cancel",
        theme::muted(),
    )
}

pub fn no_matches_line(query: &str) -> Line<'static> {
    Line::styled(format!("no other matches for \"{query}\""), theme::muted())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_grows_a_caret_while_capturing() {
        let mut sb = SearchBox::default();
        assert_eq!(title_with_query("Books · all", &sb), "Books · all");
        sb.open();
        sb.input('r');
        sb.input('s');
        assert_eq!(title_with_query("Books · all", &sb), "Books · all · /rs▊");
        sb.apply();
        assert_eq!(title_with_query("Books · all", &sb), "Books · all · /rs");
        sb.cancel();
        assert_eq!(title_with_query("Books · all", &sb), "Books · all");
    }

    #[test]
    fn highlight_paints_case_insensitive_runs() {
        let base = Style::default();
        let spans = highlight("The Rust Book", "rust", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content, "Rust");
        assert_eq!(spans[1].style.bg, Some(theme::ACCENT));
    }

    #[test]
    fn match_indices_and_stepping_wrap() {
        let labels = ["rust book", "go book", "rust guide", "python"];
        let m = match_indices(labels.iter().copied(), "rust");
        assert_eq!(m, vec![0, 2]);
        assert_eq!(step_match(&m, 0, 1), Some(2));
        assert_eq!(step_match(&m, 2, 1), Some(0));
        assert_eq!(step_match(&m, 2, -1), Some(0));
        assert_eq!(step_match(&m, 0, -1), Some(2));
        assert_eq!(step_match(&[], 0, 1), None);
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(match_indices(["a", "b"].iter().copied(), "").is_empty());
    }
}
