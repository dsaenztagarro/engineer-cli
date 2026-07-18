//! The search-state contract (design-system.dc.html §NOTIFICATION & SEARCH
//! STATES): the active query rides in the panel title with a caret
//! (`Books · all · /rust▊`), matched substrings highlight, `n`/`N` step between
//! matches, and an exhausted search reads `no other matches for "rust"`.
//!
//! The server/client split is resolved here: `/` stays whatever it already is
//! (server-side re-query for Books/Notes, in-place filter for Activities) —
//! this atom never touches the network. `n`/`N` step the cursor over the rows
//! already on screen whose label contains the query, and [`highlight`] paints
//! the matched run. A screen keeps a [`SearchBox`] plus its own match cursor.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme;

/// The `/` search buffer: the typed query and whether the prompt is capturing
/// keys. `apply` keeps the query (so the title caret and `n`/`N` survive
/// submitting); `cancel` clears it.
#[derive(Debug, Default, Clone)]
pub struct SearchBox {
    pub query: String,
    pub active: bool,
}

impl SearchBox {
    /// `/` — begin capturing, from an empty query.
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

    /// `Esc` — abandon the search entirely.
    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
    }

    /// `↵` — stop capturing but keep the query live for highlight + `n`/`N`.
    pub fn apply(&mut self) {
        self.active = false;
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }
}

/// `base` plus the query suffix and, while capturing, the caret block —
/// `"Books · all"` → `"Books · all · /rust▊"`. When there is no query the base
/// title is returned untouched.
pub fn title_with_query(base: &str, sb: &SearchBox) -> String {
    if sb.query.is_empty() && !sb.active {
        return base.to_string();
    }
    let caret = if sb.active { "▊" } else { "" };
    format!("{base} · /{}{}", sb.query, caret)
}

/// Split `label` into spans, painting each case-insensitive run of `query` with
/// the accent-background match style and the rest with `base`. An empty query
/// returns the label as one `base`-styled span.
pub fn highlight(label: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(label.to_string(), base)];
    }
    let match_style = Style::default().bg(theme::ACCENT).fg(Color::Black);
    let hay = label.to_lowercase();
    let needle = query.to_lowercase();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize; // byte offset into `label`/`hay` (both lowercased 1:1 for ascii; see note)
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

/// Indices of the rows whose label contains `query` (case-insensitive) — the
/// set `n`/`N` step through. Empty query → no matches (stepping is inert).
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

/// The next (`dir > 0`) or previous match strictly after/before `current`,
/// wrapping around. `None` when there are no matches.
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

/// The footer while a `/` prompt is capturing.
pub fn search_hints() -> Line<'static> {
    Line::styled(
        "type to search · ↵ apply · n/N next match · Esc cancel",
        theme::muted(),
    )
}

/// The muted inline line a screen shows when a live query has no (further)
/// matches — `no other matches for "rust"`. Adopted by the client-side-filter
/// screens (Activities/Notes), where matches sit among still-visible rows.
#[allow(dead_code)]
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
        // "The " + "Rust" (accent) + " Book"
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content, "Rust");
        assert_eq!(spans[1].style.bg, Some(theme::ACCENT));
    }

    #[test]
    fn match_indices_and_stepping_wrap() {
        let labels = ["rust book", "go book", "rust guide", "python"];
        let m = match_indices(labels.iter().copied(), "rust");
        assert_eq!(m, vec![0, 2]);
        assert_eq!(step_match(&m, 0, 1), Some(2)); // forward from 0 → 2
        assert_eq!(step_match(&m, 2, 1), Some(0)); // wrap forward
        assert_eq!(step_match(&m, 2, -1), Some(0)); // back from 2 → 0
        assert_eq!(step_match(&m, 0, -1), Some(2)); // wrap back
        assert_eq!(step_match(&[], 0, 1), None);
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(match_indices(["a", "b"].iter().copied(), "").is_empty());
    }
}
