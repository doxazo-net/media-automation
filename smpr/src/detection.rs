use crate::config::DetectionConfig;
use regex::Regex;
use std::sync::LazyLock;

static WORD_TOKENIZER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z']+").unwrap());

pub struct DetectionEngine {
    r_stems: Vec<String>,
    r_exact_patterns: Vec<(String, Regex)>,
    pg13_stems: Vec<String>,
    pg13_exact_patterns: Vec<(String, Regex)>,
    false_positives: Vec<String>,
    g_genres: Vec<String>,
    deny_genres: Vec<String>,
}

fn compile_exact_patterns(words: &[String]) -> Vec<(String, Regex)> {
    words
        .iter()
        .map(|w| {
            let pattern = format!(r"(?i)\b{}\b", regex::escape(w));
            (w.clone(), Regex::new(&pattern).unwrap())
        })
        .collect()
}

fn detect_stems(word_tokens: &[&str], stems: &[String], false_positives: &[String]) -> Vec<String> {
    let mut matched = Vec::new();
    for stem in stems {
        for &word in word_tokens {
            if word.contains(stem.as_str()) {
                let is_fp = false_positives.iter().any(|fp| word.contains(fp.as_str()));
                if !is_fp {
                    matched.push(word.to_string());
                    break;
                }
            }
        }
    }
    matched
}

fn detect_exact(text: &str, patterns: &[(String, Regex)]) -> Vec<String> {
    patterns
        .iter()
        .filter(|(_, pat)| pat.is_match(text))
        .map(|(word, _)| word.clone())
        .collect()
}

fn dedup_matched(stem_hits: Vec<String>, exact_hits: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for word in stem_hits.into_iter().chain(exact_hits) {
        if !result.contains(&word) {
            result.push(word);
        }
    }
    result
}

impl DetectionEngine {
    pub fn new(config: &DetectionConfig) -> Self {
        Self {
            r_stems: config.r_stems.iter().map(|s| s.to_lowercase()).collect(),
            r_exact_patterns: compile_exact_patterns(&config.r_exact),
            pg13_stems: config.pg13_stems.iter().map(|s| s.to_lowercase()).collect(),
            pg13_exact_patterns: compile_exact_patterns(&config.pg13_exact),
            false_positives: config
                .false_positives
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            g_genres: config.g_genres.iter().map(|s| s.to_lowercase()).collect(),
            deny_genres: config
                .deny_genres
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
    }

    pub fn match_g_genre<'a>(&self, genres: &'a [String]) -> Option<&'a str> {
        for genre in genres {
            if self.g_genres.contains(&genre.to_lowercase()) {
                return Some(genre.as_str());
            }
        }
        None
    }

    /// Returns the first genre present in the deny list (case-insensitive), if
    /// any. A denied genre vetoes the genre-G fallback even when `match_g_genre`
    /// also matches - e.g. a film OST tagged both `Soundtrack` and `Classical`
    /// should not be blind-rated G.
    pub fn denied_genre<'a>(&self, genres: &'a [String]) -> Option<&'a str> {
        genres
            .iter()
            .find(|g| self.deny_genres.contains(&g.to_lowercase()))
            .map(|g| g.as_str())
    }

    pub fn classify_lyrics(&self, text: &str) -> (Option<&'static str>, Vec<String>) {
        if text.trim().is_empty() {
            return (None, vec![]);
        }

        let lowered = text.to_lowercase();
        let word_tokens: Vec<&str> = WORD_TOKENIZER
            .find_iter(&lowered)
            .map(|m| m.as_str())
            .collect();

        // Check R tier first
        let r_stem_hits = detect_stems(&word_tokens, &self.r_stems, &self.false_positives);
        let r_exact_hits = detect_exact(text, &self.r_exact_patterns);
        if !r_stem_hits.is_empty() || !r_exact_hits.is_empty() {
            return (Some("R"), dedup_matched(r_stem_hits, r_exact_hits));
        }

        // Then PG-13
        let pg13_stem_hits = detect_stems(&word_tokens, &self.pg13_stems, &self.false_positives);
        let pg13_exact_hits = detect_exact(text, &self.pg13_exact_patterns);
        if !pg13_stem_hits.is_empty() || !pg13_exact_hits.is_empty() {
            return (
                Some("PG-13"),
                dedup_matched(pg13_stem_hits, pg13_exact_hits),
            );
        }

        (None, vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DetectionConfig {
        DetectionConfig {
            r_stems: vec!["fuck".into(), "shit".into()],
            r_exact: vec!["blowjob".into()],
            pg13_stems: vec!["bitch".into()],
            pg13_exact: vec!["hoe".into()],
            false_positives: vec!["Cocktail".into()],
            g_genres: vec!["Classical".into(), "Ambient".into()],
            deny_genres: vec![],
        }
    }

    #[test]
    fn stems_basic_match() {
        let tokens: Vec<&str> = vec!["motherfucker", "is", "a", "word"];
        let stems = vec!["fuck".into()];
        let fp: Vec<String> = vec![];
        let result = detect_stems(&tokens, &stems, &fp);
        assert_eq!(result, vec!["motherfucker"]);
    }

    #[test]
    fn stems_false_positive_filtered() {
        let tokens: Vec<&str> = vec!["cocktail", "party"];
        let stems = vec!["cock".into()];
        let fp = vec!["cocktail".into()];
        let result = detect_stems(&tokens, &stems, &fp);
        assert!(result.is_empty());
    }

    #[test]
    fn stems_no_matches() {
        let tokens: Vec<&str> = vec!["hello", "world"];
        let stems = vec!["fuck".into()];
        let fp: Vec<String> = vec![];
        let result = detect_stems(&tokens, &stems, &fp);
        assert!(result.is_empty());
    }

    #[test]
    fn stems_one_match_per_stem() {
        let tokens: Vec<&str> = vec!["fuck", "fucker", "fucking"];
        let stems = vec!["fuck".into()];
        let fp: Vec<String> = vec![];
        let result = detect_stems(&tokens, &stems, &fp);
        assert_eq!(result, vec!["fuck"]); // first match only
    }

    #[test]
    fn stems_multiple_stems() {
        let tokens: Vec<&str> = vec!["bullshit", "fucker"];
        let stems = vec!["shit".into(), "fuck".into()];
        let fp: Vec<String> = vec![];
        let result = detect_stems(&tokens, &stems, &fp);
        assert_eq!(result, vec!["bullshit", "fucker"]);
    }

    #[test]
    fn stems_case_handling() {
        // stems are pre-lowercased by DetectionEngine::new; verify via the engine
        let mut config = test_config();
        config.r_stems = vec!["SHIT".into()]; // uppercase in config
        let engine = DetectionEngine::new(&config);
        // constructor lowercases stems, so classify_lyrics should still match
        let (tier, words) = engine.classify_lyrics("shitty behavior");
        assert_eq!(tier, Some("R"));
        assert!(words.contains(&"shitty".to_string()));
    }

    #[test]
    fn exact_word_boundary() {
        let patterns = compile_exact_patterns(&vec!["hoe".into()]);
        let result = detect_exact("garden hoe for sale", &patterns);
        assert_eq!(result, vec!["hoe"]);
    }

    #[test]
    fn exact_no_partial_match() {
        let patterns = compile_exact_patterns(&vec!["hoe".into()]);
        let result = detect_exact("nice shoes", &patterns);
        assert!(result.is_empty());
    }

    #[test]
    fn exact_case_insensitive() {
        let patterns = compile_exact_patterns(&vec!["blowjob".into()]);
        let result = detect_exact("a BLOWJOB reference", &patterns);
        assert_eq!(result, vec!["blowjob"]); // returns original word, not match text
    }

    #[test]
    fn exact_multiple_matches() {
        let patterns = compile_exact_patterns(&vec!["hoe".into(), "piss".into()]);
        let result = detect_exact("hoe and piss", &patterns);
        assert_eq!(result, vec!["hoe", "piss"]);
    }

    #[test]
    fn exact_no_matches() {
        let patterns = compile_exact_patterns(&vec!["blowjob".into()]);
        let result = detect_exact("clean text here", &patterns);
        assert!(result.is_empty());
    }

    #[test]
    fn classify_r_tier() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("this is fucking great");
        assert_eq!(tier, Some("R"));
        assert!(words.contains(&"fucking".to_string()));
    }

    #[test]
    fn classify_pg13_tier() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("you are a bitch");
        assert_eq!(tier, Some("PG-13"));
        assert!(words.contains(&"bitch".to_string()));
    }

    #[test]
    fn classify_r_priority_over_pg13() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, _words) = engine.classify_lyrics("fuck that bitch");
        assert_eq!(tier, Some("R")); // R takes priority, PG-13 not checked
    }

    #[test]
    fn classify_clean() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("beautiful sunny day");
        assert_eq!(tier, None);
        assert!(words.is_empty());
    }

    #[test]
    fn classify_empty() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("");
        assert_eq!(tier, None);
        assert!(words.is_empty());
    }

    #[test]
    fn classify_whitespace_only() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("   \n\t  ");
        assert_eq!(tier, None);
        assert!(words.is_empty());
    }

    #[test]
    fn classify_exact_match_r() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("that was a blowjob");
        assert_eq!(tier, Some("R"));
        assert!(words.contains(&"blowjob".to_string()));
    }

    #[test]
    fn classify_mixed_dedup() {
        // "fuck" matches as stem AND "fuck" could match as exact if it were in the list
        // Test that stem+exact results are deduped
        let config = DetectionConfig {
            r_stems: vec!["fuck".into()],
            r_exact: vec!["fuck".into()], // same word in both lists
            pg13_stems: vec![],
            pg13_exact: vec![],
            false_positives: vec![],
            g_genres: vec![],
            deny_genres: vec![],
        };
        let engine = DetectionEngine::new(&config);
        let (_tier, words) = engine.classify_lyrics("fuck this");
        // "fuck" should appear only once despite matching both stem and exact
        assert_eq!(words.iter().filter(|w| *w == "fuck").count(), 1);
    }

    #[test]
    fn classify_false_positive_no_trigger() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("enjoy a cocktail tonight");
        assert_eq!(tier, None);
        assert!(words.is_empty());
    }

    fn default_engine() -> DetectionEngine {
        use crate::config::defaults;
        let to_vec = |s: &[&str]| s.iter().map(|w| w.to_string()).collect();
        DetectionEngine::new(&DetectionConfig {
            r_stems: to_vec(defaults::R_STEMS),
            r_exact: to_vec(defaults::R_EXACT),
            pg13_stems: to_vec(defaults::PG13_STEMS),
            pg13_exact: to_vec(defaults::PG13_EXACT),
            false_positives: to_vec(defaults::FALSE_POSITIVES),
            g_genres: vec![],
            deny_genres: vec![],
        })
    }

    #[test]
    fn default_config_ignores_substring_false_positives() {
        let engine = default_engine();
        // Innocent words that contain cum/cock/pussy as substrings - including
        // Latin liturgical text ("Dominus tecum" from the Ave Maria) - must NOT
        // be flagged. Real-library false positives observed on an 82k track set.
        for clean in [
            "dominus tecum benedicta tu",
            "the scum of the earth",
            "a proud peacock strutting",
            "the pussycat sang a song",
            "our cockapoo puppy",
            "down by lake cumberland",
            "they were encumbered",
            "we must circumvent it",
        ] {
            assert_eq!(
                engine.classify_lyrics(clean).0,
                None,
                "false positive on {clean:?}"
            );
        }
    }

    #[test]
    fn default_config_still_flags_standalone_profanity() {
        let engine = default_engine();
        // The genuine standalone words must STILL classify as R.
        for dirty in ["cum", "cock", "pussy", "cumming", "cocks", "pussies"] {
            assert_eq!(
                engine.classify_lyrics(dirty).0,
                Some("R"),
                "missed genuine profanity {dirty:?}"
            );
        }
    }

    #[test]
    fn default_config_exact_handles_punctuation_and_case() {
        let engine = default_engine();
        // Retiered exact words must still fire across casing and adjacent
        // punctuation - the (?i)\b...\b pattern handles both. (Codoki #212.)
        for dirty in ["that Pussy.", "COCK!", "(cum)", "what a cock?", "PUSSY"] {
            assert_eq!(
                engine.classify_lyrics(dirty).0,
                Some("R"),
                "expected R for {dirty:?}"
            );
        }
    }

    #[test]
    fn default_config_hyphen_is_a_word_boundary() {
        let engine = default_engine();
        // Documents the boundary semantics: a hyphen is a word boundary, so
        // "pussy-cat" matches \bpussy\b, while the solid compound "pussycat"
        // does not (no boundary before "cat") and stays clean.
        assert_eq!(engine.classify_lyrics("pussy-cat").0, Some("R"));
        assert_eq!(engine.classify_lyrics("the pussycat sang").0, None);
    }

    #[test]
    fn default_config_diacritics_do_not_reintroduce_false_positives() {
        let engine = default_engine();
        // Latin liturgical text with diacritics still must not flag - "tecum"
        // never matches \bcum\b regardless of accents elsewhere in the line.
        assert_eq!(
            engine.classify_lyrics("Dóminus tecum, benedícta tu").0,
            None
        );
    }

    #[test]
    fn classify_non_ascii_passthrough() {
        let engine = DetectionEngine::new(&test_config());
        let (tier, words) = engine.classify_lyrics("café résumé naïve");
        assert_eq!(tier, None);
        assert!(words.is_empty());
    }

    #[test]
    fn genre_match() {
        let engine = DetectionEngine::new(&test_config());
        let genres = vec!["Classical".into(), "Rock".into()];
        assert_eq!(engine.match_g_genre(&genres), Some("Classical"));
    }

    #[test]
    fn genre_no_match() {
        let engine = DetectionEngine::new(&test_config());
        let genres = vec!["Rock".into(), "Metal".into()];
        assert_eq!(engine.match_g_genre(&genres), None);
    }

    #[test]
    fn genre_empty_genres() {
        let engine = DetectionEngine::new(&test_config());
        let genres: Vec<String> = vec![];
        assert_eq!(engine.match_g_genre(&genres), None);
    }

    #[test]
    fn genre_empty_g_genres() {
        let config = DetectionConfig {
            r_stems: vec![],
            r_exact: vec![],
            pg13_stems: vec![],
            pg13_exact: vec![],
            false_positives: vec![],
            g_genres: vec![], // no genres configured
            deny_genres: vec![],
        };
        let engine = DetectionEngine::new(&config);
        let genres = vec!["Classical".into()];
        assert_eq!(engine.match_g_genre(&genres), None);
    }

    #[test]
    fn genre_first_match_wins() {
        let engine = DetectionEngine::new(&test_config());
        // Both "Ambient" and "Classical" are in g_genres
        let genres = vec!["Ambient".into(), "Classical".into()];
        assert_eq!(engine.match_g_genre(&genres), Some("Ambient")); // first in item's list
    }

    #[test]
    fn denied_genre_vetoes_even_when_allow_list_also_matches() {
        let mut config = test_config(); // g_genres: Classical, Ambient
        config.deny_genres = vec!["Soundtrack".into()];
        let engine = DetectionEngine::new(&config);
        // A film OST tagged both an allow-listed and a denied genre: the
        // allow-list still matches, but the deny list flags it for veto.
        let genres = vec!["Classical".into(), "Soundtrack".into()];
        assert_eq!(engine.match_g_genre(&genres), Some("Classical"));
        assert_eq!(engine.denied_genre(&genres), Some("Soundtrack"));
        // Case-insensitive.
        assert_eq!(
            engine.denied_genre(&["soundtrack".into()]),
            Some("soundtrack")
        );
        // No denied genre present.
        assert_eq!(engine.denied_genre(&["Classical".into()]), None);
    }

    #[test]
    fn denied_genre_none_when_deny_list_empty() {
        let engine = DetectionEngine::new(&test_config()); // deny_genres empty
        assert_eq!(engine.denied_genre(&["Soundtrack".into()]), None);
    }

    #[test]
    fn engine_construction() {
        let config = test_config();
        let engine = DetectionEngine::new(&config);
        assert_eq!(engine.r_stems.len(), 2);
        assert_eq!(engine.r_exact_patterns.len(), 1);
        assert_eq!(engine.pg13_stems.len(), 1);
        assert_eq!(engine.pg13_exact_patterns.len(), 1);
        // false positives should be lowercased
        assert_eq!(engine.false_positives, vec!["cocktail"]);
        // g_genres should be lowercased
        assert_eq!(engine.g_genres, vec!["classical", "ambient"]);
    }

    use proptest::prelude::*;

    fn arb_words() -> impl Strategy<Value = Vec<String>> {
        // Min length 1: real config word lists never contain empty strings, and an
        // empty stem makes word.contains("") trivially true for every token.
        prop::collection::vec("[a-zA-Z']{1,8}", 0..6)
    }

    // Words drawn from the full printable-ASCII range so they include regex
    // metacharacters ( ) [ ] \ . * + ? etc., to genuinely stress regex::escape
    // in compile_exact_patterns (whose Regex::new(...).unwrap() relies on it).
    fn arb_pattern_words() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("[\\x20-\\x7e]{1,8}", 0..6)
    }

    prop_compose! {
        fn arb_detection_config()(
            r_stems in arb_words(), r_exact in arb_words(),
            pg13_stems in arb_words(), pg13_exact in arb_words(),
            false_positives in arb_words(), g_genres in arb_words(),
            deny_genres in arb_words(),
        ) -> DetectionConfig {
            DetectionConfig {
                r_stems, r_exact, pg13_stems, pg13_exact, false_positives, g_genres,
                deny_genres,
            }
        }
    }

    proptest! {
        // Never panics on arbitrary config + arbitrary (incl. Unicode) lyrics,
        // and the returned tier is always one of the three valid values.
        #[test]
        fn classify_never_panics_and_tier_is_valid(
            cfg in arb_detection_config(), text in ".*",
        ) {
            let engine = DetectionEngine::new(&cfg);
            let (tier, _matched) = engine.classify_lyrics(&text);
            prop_assert!(matches!(tier, None | Some("R") | Some("PG-13")));
        }

        #[test]
        fn classify_is_deterministic(cfg in arb_detection_config(), text in ".*") {
            let engine = DetectionEngine::new(&cfg);
            prop_assert_eq!(
                engine.classify_lyrics(&text), engine.classify_lyrics(&text));
        }

        #[test]
        fn classify_blank_text_is_none(
            cfg in arb_detection_config(), ws in "[ \\t\\r\\n]*",
        ) {
            let engine = DetectionEngine::new(&cfg);
            let (tier, matched) = engine.classify_lyrics(&ws);
            prop_assert!(tier.is_none());
            prop_assert!(matched.is_empty());
        }

        // Stem hits are always drawn from the input tokens.
        #[test]
        fn detect_stems_returns_only_input_tokens(
            tokens in arb_words(), stems in arb_words(), fps in arb_words(),
        ) {
            let refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();
            for hit in detect_stems(&refs, &stems, &fps) {
                prop_assert!(tokens.contains(&hit));
            }
        }

        // Exact hits are always one of the configured pattern words; pattern words
        // carrying regex metacharacters must not break compilation (regex::escape)
        // or panic. Uses the metacharacter-bearing generator to make that real.
        #[test]
        fn detect_exact_returns_only_pattern_words(
            words in arb_pattern_words(), text in ".*",
        ) {
            let patterns = compile_exact_patterns(&words);
            for hit in detect_exact(&text, &patterns) {
                prop_assert!(words.contains(&hit));
            }
        }

        #[test]
        fn match_g_genre_returns_an_input_genre(
            cfg in arb_detection_config(), genres in arb_words(),
        ) {
            let engine = DetectionEngine::new(&cfg);
            if let Some(g) = engine.match_g_genre(&genres) {
                prop_assert!(genres.iter().any(|x| x.as_str() == g));
            }
        }

        // Positive direction: a genre present in the allow-list (in any case) is
        // always found - exercises the case-insensitive match, not just the miss.
        #[test]
        fn match_g_genre_matches_case_insensitively(
            cfg in arb_detection_config()
                .prop_filter("needs a non-empty allow-list", |c| !c.g_genres.is_empty()),
            extra in arb_words(),
        ) {
            let engine = DetectionEngine::new(&cfg);
            let mut candidates = extra;
            candidates.push(cfg.g_genres[0].to_uppercase());
            prop_assert!(engine.match_g_genre(&candidates).is_some());
        }

        // Core business invariant: if the R lists alone classify as R, the full
        // config must also classify as R (R is checked before PG-13, never after).
        #[test]
        fn r_tier_takes_priority_over_pg13(
            cfg in arb_detection_config(), text in ".*",
        ) {
            let r_only = DetectionConfig {
                r_stems: cfg.r_stems.clone(),
                r_exact: cfg.r_exact.clone(),
                pg13_stems: Vec::new(),
                pg13_exact: Vec::new(),
                false_positives: cfg.false_positives.clone(),
                g_genres: cfg.g_genres.clone(),
                deny_genres: cfg.deny_genres.clone(),
            };
            let r_engine = DetectionEngine::new(&r_only);
            if r_engine.classify_lyrics(&text).0 == Some("R") {
                let full_engine = DetectionEngine::new(&cfg);
                prop_assert_eq!(full_engine.classify_lyrics(&text).0, Some("R"));
            }
        }
    }
}
