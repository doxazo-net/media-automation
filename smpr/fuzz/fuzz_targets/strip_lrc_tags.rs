#![no_main]
//! Fuzz `strip_lrc_tags` - the LRC timestamp/metadata stripper. Pure string in,
//! string out: the property under test is simply "never panics" (no regex
//! catastrophe, no slice-on-non-char-boundary, no overflow) on arbitrary input.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The function takes &str; only valid UTF-8 is meaningful input. Invalid
    // byte sequences are skipped rather than lossily converted so the corpus
    // stays representative of real lyric text.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = smpr::strip_lrc_tags(text);
    }
});
