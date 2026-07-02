use regex::Regex;
use std::sync::LazyLock;

static LRC_TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\d{1,3}:\d{2}(?:\.\d{1,3})?\]").unwrap());

static LRC_METADATA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mi)^\[[a-z]{2,}:.*\]$").unwrap());

/// Extract the leaf directory name from a path (handles both `/` and `\` separators).
/// Returns the original path if the leaf would be empty (e.g., "/" or "\\").
pub fn location_leaf(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }
    match trimmed.rfind(['/', '\\']) {
        Some(pos) => &trimmed[pos + 1..],
        None => trimmed,
    }
}

/// Normalize a path for comparison: backslashes to forward slashes, lowercased.
/// Shared by location filtering, per-item force-rating resolution, and per-song
/// override matching so every path comparison uses one canonical form.
pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Remove LRC timestamp tags and metadata lines from lyrics text.
pub fn strip_lrc_tags(text: &str) -> String {
    let text = LRC_TIMESTAMP.replace_all(text, "");
    let text = LRC_METADATA.replace_all(&text, "");
    text.to_string()
}

/// The marker the upstream lyrics tool writes as the sole content of an
/// instrumental sidecar (a `.txt`/`.lrc` external subtitle stream). See
/// `is_instrumental_marker`.
pub const INSTRUMENTAL_MARKER: &str = "♪ Instrumental ♪";

/// Returns true when `text` is *only* the instrumental marker, tolerating
/// surrounding whitespace, letter case, and the musical-note glyphs. Used so a
/// marker-only lyrics stream is treated as "no lyrics" (the track is
/// instrumental) rather than as clean, evaluated lyrics.
///
/// Returns false when any genuine lyric content sits alongside the marker, so a
/// real song is never suppressed.
pub fn is_instrumental_marker(text: &str) -> bool {
    let without_notes: String = text
        .chars()
        .filter(|c| !matches!(c, '♪' | '♫' | '♬' | '♩'))
        .collect();
    without_notes.trim().eq_ignore_ascii_case("instrumental")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_timestamps() {
        let input = "[00:15.30]Hello world\n[00:20.00]Second line";
        let result = strip_lrc_tags(input);
        assert_eq!(result, "Hello world\nSecond line");
    }

    #[test]
    fn strip_metadata_lines() {
        let input = "[ar:Artist Name]\n[ti:Song Title]\nActual lyrics here";
        let result = strip_lrc_tags(input);
        assert!(result.contains("Actual lyrics here"));
        assert!(!result.contains("[ar:"));
        assert!(!result.contains("[ti:"));
    }

    #[test]
    fn passthrough_plain_text() {
        let input = "Just plain text lyrics\nNo tags at all";
        let result = strip_lrc_tags(input);
        assert_eq!(result, input);
    }

    #[test]
    fn empty_input() {
        assert_eq!(strip_lrc_tags(""), "");
    }

    #[test]
    fn instrumental_marker_exact() {
        assert!(is_instrumental_marker(INSTRUMENTAL_MARKER));
    }

    #[test]
    fn instrumental_marker_whitespace_and_case() {
        assert!(is_instrumental_marker("  \n♪ INSTRUMENTAL ♪\n  "));
        assert!(is_instrumental_marker("instrumental"));
        assert!(is_instrumental_marker("♫ Instrumental ♬"));
    }

    #[test]
    fn instrumental_marker_rejects_real_lyrics() {
        // The word appears, but alongside genuine lyric lines -> not a marker.
        assert!(!is_instrumental_marker(
            "This is an instrumental break\nbut the song has words"
        ));
        assert!(!is_instrumental_marker("Hello world"));
        assert!(!is_instrumental_marker(""));
    }

    #[test]
    fn mixed_timestamps_and_text() {
        let input = "[01:23.45]Line one\nPlain line\n[02:00.00]Line three";
        let result = strip_lrc_tags(input);
        assert_eq!(result, "Line one\nPlain line\nLine three");
    }

    #[test]
    fn location_leaf_simple_path() {
        assert_eq!(super::location_leaf("/mnt/music/Classical"), "Classical");
    }

    #[test]
    fn location_leaf_windows_path() {
        assert_eq!(super::location_leaf("C:\\Music\\Classical"), "Classical");
    }

    #[test]
    fn location_leaf_trailing_slash() {
        assert_eq!(super::location_leaf("/mnt/music/Classical/"), "Classical");
    }

    #[test]
    fn location_leaf_no_separator() {
        assert_eq!(super::location_leaf("Classical"), "Classical");
    }

    #[test]
    fn location_leaf_empty_returns_original() {
        assert_eq!(super::location_leaf(""), "");
    }

    #[test]
    fn location_leaf_only_slashes_returns_original() {
        assert_eq!(super::location_leaf("///"), "///");
    }

    use proptest::prelude::*;

    proptest! {
        // Never panics on arbitrary (incl. Unicode / nested-bracket) input.
        #[test]
        fn strip_lrc_never_panics(s in ".*") {
            let _ = strip_lrc_tags(&s);
        }

        // Stripping is stable: a second pass over already-stripped text is a no-op.
        #[test]
        fn strip_lrc_is_idempotent(s in ".*") {
            let once = strip_lrc_tags(&s);
            prop_assert_eq!(strip_lrc_tags(&once), once);
        }
    }
}
