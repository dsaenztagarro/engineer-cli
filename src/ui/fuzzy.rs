//! A small subsequence fuzzy matcher — the rank behind the shared [`super::picker`].

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
        assert!(score("adp", "pad").is_none());
    }

    #[test]
    fn word_boundary_hits_outrank_mid_word_hits() {
        let boundary = score("dd", "Data Design").unwrap();
        let midword = score("dd", "muddled").unwrap();
        assert!(boundary > midword, "{boundary} !> {midword}");
    }

    #[test]
    fn consecutive_run_outranks_a_gap() {
        // Neither haystack is all word-initials, which would outrank a run on
        // its own.
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
