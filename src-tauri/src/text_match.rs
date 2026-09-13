//! Shared substring matching for search and retrieval: an ASCII case-insensitive
//! find that mirrors SQLite's `LIKE` case folding (so a row found by a `LIKE`
//! query is always found again here), plus excerpt helpers built on it.
use crate::SearchPiece;

/// Escapes `\`, `%` and `_` (in that order) for a `LIKE ... ESCAPE '\'` pattern,
/// so a query containing those characters matches them literally instead of
/// as SQL wildcards.
pub(crate) fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Byte offset of the first ASCII-case-insensitive match of `needle` in
/// `haystack`, or `None`. Non-ASCII bytes must match exactly, so the returned
/// offset (and the match's end) is always a char boundary.
pub(crate) fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return Some(0);
    }
    if n.len() > h.len() {
        return None;
    }
    'outer: for start in 0..=(h.len() - n.len()) {
        for (j, &b) in n.iter().enumerate() {
            let a = h[start + j];
            let equal = if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() {
                a.eq_ignore_ascii_case(&b)
            } else {
                a == b
            };
            if !equal {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// Splits `content` around its first match of `query` into the text before,
/// the matched span itself (original casing), and the text after, each
/// trimmed to at most `radius` characters with an ellipsis when truncated.
/// Pre-split so callers never need UTF-8/UTF-16 offset mapping or `v-html`.
pub(crate) fn piece(content: &str, query: &str, radius: usize) -> Option<SearchPiece> {
    let at = find_ascii_ci(content, query)?;
    let end = at + query.len();
    let from = content.floor_char_boundary(at.saturating_sub(radius));
    let to = content.ceil_char_boundary((end + radius).min(content.len()));
    let mut before = content[from..at].to_string();
    if from > 0 {
        before.insert(0, '…');
    }
    let matched = content[at..end].to_string();
    let mut after = content[end..to].to_string();
    if to < content.len() {
        after.push('…');
    }
    Some(SearchPiece {
        before,
        matched,
        after,
    })
}

/// A single flattened excerpt string (used for tool-retrieval results, which
/// have no separate "matched" styling).
pub(crate) fn excerpt(content: &str, query: &str, radius: usize) -> Option<String> {
    let p = piece(content, query, radius)?;
    Some(format!("{}{}{}", p.before, p.matched, p.after))
}

#[cfg(test)]
#[path = "../../test/text_match.rs"]
mod tests;
