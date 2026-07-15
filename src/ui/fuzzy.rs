//! A small subsequence fuzzy matcher — the rank behind the shared [`super::picker`].
//!
//! `dda` finds "**D**esigning **D**ata-Intensive **A**pplications": every query
//! char must appear in the text in order (case-insensitive), and the score
//! rewards matches that are consecutive or land on a word boundary, so tighter,
//! more word-aligned hits sort first. This is the "fuzzy over navigate" rank the
//! kit asks for — deliberately not the substring `contains` the lists narrow
//! with today.

/// Score `text` against `query`, or `None` if `query` is not a subsequence of it.
/// An empty query matches everything with a neutral score, so the picker shows
/// the full set before the user types.
pub fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let needle: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let hay: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();

    let mut qi = 0;
    let mut total = 0i32;
    let mut last_hit: Option<usize> = None;

    for (hi, &hc) in hay.iter().enumerate() {
        if qi >= needle.len() {
            break;
        }
        if hc == needle[qi] {
            total += 1; // base for any hit
            if last_hit == Some(hi.wrapping_sub(1)) {
                total += 5; // consecutive run
            }
            let at_boundary = hi == 0 || !hay[hi - 1].is_alphanumeric();
            if at_boundary {
                total += 10; // start of a word
            }
            last_hit = Some(hi);
            qi += 1;
        }
    }

    if qi == needle.len() {
        // Gently prefer shorter haystacks (a tight match over a sprawling one).
        total -= (hay.len() as i32 - needle.len() as i32) / 4;
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn subsequence_matches_case_insensitively() {
        assert!(score("dda", "Designing Data-Intensive Applications").is_some());
        assert!(score("SICP", "sicp").is_some());
        assert!(score("ch3", "Chapter 3").is_some());
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert!(score("xyz", "Designing Data-Intensive Applications").is_none());
        // Right letters, wrong order.
        assert!(score("adp", "pad").is_none());
    }

    #[test]
    fn word_boundary_hits_outrank_mid_word_hits() {
        // "dd" as two word-initials beats "dd" buried inside one word.
        let boundary = score("dd", "Data Design").unwrap();
        let midword = score("dd", "muddled").unwrap();
        assert!(boundary > midword, "{boundary} !> {midword}");
    }

    #[test]
    fn consecutive_run_outranks_a_gap() {
        // Same-length haystacks, neither all word-initials: the consecutive match
        // wins. (Word-initial hits are also strong — "s i c" outscoring "sicp" is
        // correct, since every letter there starts a word.)
        let run = score("ab", "xabx").unwrap();
        let gapped = score("ab", "xaxb").unwrap();
        assert!(run > gapped, "{run} !> {gapped}");
    }

    #[test]
    fn shorter_haystack_preferred_on_equal_shape() {
        let short = score("sys", "systems").unwrap();
        let long = score("sys", "systems programming and architecture").unwrap();
        assert!(short > long, "{short} !> {long}");
    }
}
