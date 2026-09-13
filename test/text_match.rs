use super::*;

#[test]
fn find_ascii_ci_matches_regardless_of_ascii_case() {
    assert_eq!(find_ascii_ci("Hello World", "world"), Some(6));
    assert_eq!(find_ascii_ci("Hello World", "WORLD"), Some(6));
    assert_eq!(find_ascii_ci("Hello World", "xyz"), None);
}

#[test]
fn find_ascii_ci_returns_char_boundary_offsets_on_multibyte_text() {
    let haystack = "café 日本語 test";
    // "本語" starts after "café 日" (5 ascii/latin1 chars + 1 CJK char); each CJK
    // char is 3 bytes in UTF-8, "é" is 2 bytes.
    let at = find_ascii_ci(haystack, "本語").unwrap();
    assert!(haystack.is_char_boundary(at));
    assert_eq!(&haystack[at..at + "本語".len()], "本語");
    // A query straddling a non-ASCII run must still match exactly.
    let at2 = find_ascii_ci(haystack, "TEST").unwrap();
    assert_eq!(&haystack[at2..], "test");
}

#[test]
fn find_ascii_ci_does_not_panic_on_length_changing_lowercase_characters() {
    // "İ" (U+0130) lowercases to "i̇" (two chars) in Unicode-aware lowercasing;
    // ASCII-only case folding must never attempt that and must not panic.
    let haystack = "İstanbul";
    assert_eq!(find_ascii_ci(haystack, "stanbul"), Some("İ".len()));
    assert_eq!(find_ascii_ci(haystack, "İstanbul"), Some(0));
}

#[test]
fn find_ascii_ci_empty_needle_matches_at_zero() {
    assert_eq!(find_ascii_ci("anything", ""), Some(0));
    assert_eq!(find_ascii_ci("", ""), Some(0));
}

#[test]
fn piece_splits_before_matched_after_with_radius() {
    let content = "the quick brown fox jumps over the lazy dog";
    // "brown" starts at byte 10; radius 5 windows to [5,10) before and [15,20) after.
    let piece = piece(content, "brown", 5).unwrap();
    assert_eq!(piece.matched, "brown");
    assert_eq!(piece.before, "…uick ");
    assert_eq!(piece.after, " fox …");
}

#[test]
fn piece_adds_ellipsis_only_when_truncated() {
    let content = "short text";
    let piece = piece(content, "short", 3).unwrap();
    assert!(!piece.before.starts_with('…'));
    assert!(piece.after.ends_with('…') || piece.after == " text");
}

#[test]
fn piece_returns_none_when_not_found() {
    assert!(piece("hello", "xyz", 10).is_none());
}

#[test]
fn excerpt_radius_is_respected_and_flattens_piece() {
    let content = "0123456789needle0123456789";
    let found = excerpt(content, "needle", 3).unwrap();
    assert!(found.contains("needle"));
    assert!(found.starts_with('…'));
    assert!(found.ends_with('…'));
}
