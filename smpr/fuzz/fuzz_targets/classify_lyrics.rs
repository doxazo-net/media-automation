#![no_main]
//! Fuzz `DetectionEngine::classify_lyrics` - the tiered explicit-content
//! classifier (stem + exact matching, false-positive filtering, R-over-PG-13
//! priority). The property under test is "never panics" on arbitrary input.
//!
//! The engine is built once from a fixed, representative `DetectionConfig`
//! (mirroring the `test_config()` pattern in detection.rs) and reused across
//! iterations - constructing it per-iteration would dominate the runtime and is
//! not what we are fuzzing.

use libfuzzer_sys::fuzz_target;
use smpr::{DetectionConfig, DetectionEngine};
use std::sync::OnceLock;

fn engine() -> &'static DetectionEngine {
    static ENGINE: OnceLock<DetectionEngine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        // Small but varied: exercises stem matching, exact (word-boundary)
        // matching, the false-positive filter ("shitake" must not trip the
        // "shit" stem), and R-tier-beats-PG-13 priority.
        let config = DetectionConfig {
            r_stems: vec!["fuck".into(), "shit".into()],
            r_exact: vec!["cunt".into()],
            pg13_stems: vec!["damn".into()],
            pg13_exact: vec!["hell".into()],
            false_positives: vec!["shitake".into()],
            g_genres: vec!["Children's Music".into()],
        };
        DetectionEngine::new(&config)
    })
}

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = engine().classify_lyrics(text);
    }
});
