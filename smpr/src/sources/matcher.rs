//! Pure matching logic: normalize catalog text, score a query against a source
//! hit, and apply the confidence gate. No I/O.
//!
//! Two-questions rule (do not conflate): this module answers only "did we match
//! the right track?" (text + duration confidence). Whether the matched hit's
//! advisory verdict is correct is a separate question, validated against trusted
//! ground truth elsewhere - never against lyrics output.

use crate::sources::{Match, SourceHit, TrackQuery};
use std::sync::LazyLock;

/// Parenthesized/bracketed feat-or-remaster chunks, and a trailing
/// " - <...remaster...>" suffix. Removed before scoring so they do not defeat
/// character-level similarity.
static NOISE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // Keywords are `\b`-anchored so only STANDALONE feat/remaster/mono/stereo
    // tokens strip - an incidental substring (the "ft" in "Gift", the "mono" in
    // "Monolith") must not delete a distinguishing parenthetical.
    regex::Regex::new(
        r"(?ix)
          \s*[\(\[][^\)\]]*\b(?:featuring|feat|ft|remastered?|mono|stereo)\b[^\)\]]*[\)\]]
        | \s*-\s*[^-]*\bremastered?\b[^-]*$
        ",
    )
    .expect("static noise regex compiles")
});

/// Any run of non-alphanumeric characters, collapsed to a single space.
static NON_ALNUM: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^\p{Alphabetic}\p{Number}]+").expect("compiles"));

/// Lower-case, strip feat/remaster noise and punctuation, collapse whitespace.
/// Removes the differences that defeat character-level similarity before
/// scoring; residual differences (diacritics, minor edits) are left to
/// Jaro-Winkler.
pub fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase();
    let denoised = NOISE.replace_all(&lowered, "");
    let spaced = NON_ALNUM.replace_all(denoised.trim(), " ");
    spaced.trim().to_string()
}

/// Text confidence in [0,1]: a weighted Jaro-Winkler of normalized title and
/// artist (title dominates). When either side lacks an artist, title similarity
/// stands alone rather than penalizing the match.
pub fn text_confidence(query: &TrackQuery, hit: &SourceHit) -> f64 {
    let qt = normalize(&query.title);
    let ht = normalize(&hit.title);
    // A title that reduces to nothing (pure noise, e.g. "(feat. X)") is not
    // matchable: `jaro_winkler("", "")` is 1.0, which would clear the gate.
    if qt.is_empty() || ht.is_empty() {
        return 0.0;
    }
    let title_sim = strsim::jaro_winkler(&qt, &ht);
    match (&query.artist, &hit.artist) {
        (Some(qa), Some(ha)) => {
            let artist_sim = strsim::jaro_winkler(&normalize(qa), &normalize(ha));
            0.65 * title_sim + 0.35 * artist_sim
        }
        _ => title_sim,
    }
}

/// Absolute duration difference in seconds, or `None` if either side is unknown.
pub fn duration_delta(query: &TrackQuery, hit: &SourceHit) -> Option<i64> {
    match (query.duration_s, hit.duration_s) {
        (Some(a), Some(b)) => Some((a - b).abs()),
        _ => None,
    }
}

/// Tuning knobs for the confidence gate (resolved from config in later milestones).
#[derive(Debug, Clone, Copy)]
pub struct MatchParams {
    pub min_confidence: f64,
    pub duration_tolerance_s: i64,
}

/// The best gated match among `hits`, or `None` if none clears the gate.
///
/// A hit qualifies only if its text confidence >= `min_confidence` AND, when
/// both durations are known, `|Δduration| <= duration_tolerance_s`. A missing
/// duration on either side skips the duration gate (text-only, weaker). Among
/// qualifiers, the highest confidence wins; ties break toward the smallest
/// duration delta (an unknown delta sorts last).
pub fn best_match(query: &TrackQuery, hits: &[SourceHit], params: &MatchParams) -> Option<Match> {
    hits.iter()
        .filter_map(|hit| {
            // Cheap gates first, so a hit that will be rejected anyway never pays
            // for normalization + Jaro-Winkler (matters on large result sets).
            let delta = duration_delta(query, hit);
            if let Some(d) = delta
                && d > params.duration_tolerance_s
            {
                return None;
            }
            // Corroboration floor: a high title score alone is too weak for this
            // paramount-risk gate (common titles like "Intro" collide across
            // artists). Require a second signal - artist present on both sides,
            // or a known duration within tolerance.
            let artist_corroborated = query.artist.is_some() && hit.artist.is_some();
            let duration_corroborated = delta.is_some();
            if !artist_corroborated && !duration_corroborated {
                return None;
            }
            // Expensive text similarity only for hits that survived the cheap gates.
            let confidence = text_confidence(query, hit);
            if confidence < params.min_confidence {
                return None;
            }
            Some(Match {
                hit: hit.clone(),
                confidence,
                duration_delta_s: delta,
            })
        })
        .max_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    // Smaller delta is better; an unknown delta (None) sorts last.
                    let ad = a.duration_delta_s.unwrap_or(i64::MAX);
                    let bd = b.duration_delta_s.unwrap_or(i64::MAX);
                    bd.cmp(&ad)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SourceVerdict;

    fn q(title: &str, artist: Option<&str>, dur: Option<i64>) -> TrackQuery {
        TrackQuery {
            artist: artist.map(str::to_string),
            album: None,
            title: title.to_string(),
            duration_s: dur,
        }
    }
    fn h(title: &str, artist: Option<&str>, dur: Option<i64>) -> SourceHit {
        SourceHit {
            source: "itunes".to_string(),
            source_track_id: None,
            artist: artist.map(str::to_string),
            album: None,
            title: title.to_string(),
            duration_s: dur,
            verdict: SourceVerdict::NotExplicit,
        }
    }

    const PARAMS: MatchParams = MatchParams {
        min_confidence: 0.85,
        duration_tolerance_s: 3,
    };

    #[test]
    fn normalize_strips_feat_and_punctuation() {
        assert_eq!(normalize("Song (feat. Someone)"), "song");
        assert_eq!(normalize("Song!  -  Remastered 2011"), "song");
        assert_eq!(normalize("  Héllo, World?? "), "héllo world");
        assert_eq!(normalize("Song (Mono)"), "song");
    }

    #[test]
    fn normalize_keeps_incidental_keyword_substrings() {
        // A parenthetical that merely CONTAINS a keyword's letters is a real,
        // distinguishing part of the title and must survive.
        assert_eq!(normalize("Song (Gift)"), "song gift");
        assert_eq!(normalize("Song (Aftermath)"), "song aftermath");
        assert_eq!(normalize("Song (Monolith)"), "song monolith");
    }

    #[test]
    fn noise_only_title_is_zero_confidence() {
        // Both titles reduce to "" - must not read as a perfect match.
        let c = text_confidence(&q("(feat. X)", None, None), &h("(Remastered)", None, None));
        assert_eq!(c, 0.0);
    }

    #[test]
    fn rejects_title_only_without_corroboration() {
        // No artist on either side and no known duration: title alone is too weak.
        let hits = vec![h("Intro", None, None)];
        assert!(best_match(&q("Intro", None, None), &hits, &PARAMS).is_none());
    }

    #[test]
    fn tie_break_prefers_smaller_duration_delta() {
        // Both hits clear the gate at equal confidence; the smaller delta wins.
        let hits = vec![
            h("Song", Some("Artist"), Some(203)), // delta 3
            h("Song", Some("Artist"), Some(201)), // delta 1
        ];
        let m = best_match(&q("Song", Some("Artist"), Some(200)), &hits, &PARAMS).unwrap();
        assert_eq!(m.hit.duration_s, Some(201));
        assert_eq!(m.duration_delta_s, Some(1));
    }

    #[test]
    fn exact_match_is_high_confidence() {
        let c = text_confidence(
            &q("Song", Some("Artist"), None),
            &h("Song", Some("Artist"), None),
        );
        assert!(c > 0.99, "got {c}");
    }

    #[test]
    fn feat_suffix_still_matches_after_normalize() {
        let c = text_confidence(
            &q("Song (feat. X)", Some("Artist"), None),
            &h("Song", Some("Artist"), None),
        );
        assert!(c > 0.99, "got {c}");
    }

    #[test]
    fn different_song_is_low_confidence() {
        let c = text_confidence(
            &q("Alpha", Some("Artist"), None),
            &h("Omega Beta Gamma", Some("Artist"), None),
        );
        assert!(c < 0.85, "got {c}");
    }

    #[test]
    fn duration_delta_absolute_or_none() {
        assert_eq!(
            duration_delta(&q("s", None, Some(200)), &h("s", None, Some(203))),
            Some(3)
        );
        assert_eq!(
            duration_delta(&q("s", None, None), &h("s", None, Some(203))),
            None
        );
    }

    #[test]
    fn accepts_exact_match_within_duration() {
        let hits = vec![h("Song", Some("Artist"), Some(200))];
        let m = best_match(&q("Song", Some("Artist"), Some(201)), &hits, &PARAMS).unwrap();
        assert_eq!(m.hit.title, "Song");
        assert_eq!(m.duration_delta_s, Some(1));
    }

    #[test]
    fn rejects_same_title_wrong_duration() {
        let hits = vec![h("Song", Some("Artist"), Some(230))];
        assert!(best_match(&q("Song", Some("Artist"), Some(200)), &hits, &PARAMS).is_none());
    }

    #[test]
    fn picks_duration_correct_among_same_title_collisions() {
        let hits = vec![
            h("Song", Some("Artist"), Some(230)), // wrong length
            h("Song", Some("Artist"), Some(201)), // right length
        ];
        let m = best_match(&q("Song", Some("Artist"), Some(200)), &hits, &PARAMS).unwrap();
        assert_eq!(m.hit.duration_s, Some(201));
    }

    #[test]
    fn rejects_sub_threshold_text() {
        let hits = vec![h("Completely Different", Some("Artist"), Some(200))];
        assert!(best_match(&q("Song", Some("Artist"), Some(200)), &hits, &PARAMS).is_none());
    }

    #[test]
    fn allows_text_only_when_duration_unknown() {
        let hits = vec![h("Song", Some("Artist"), None)];
        let m = best_match(&q("Song", Some("Artist"), None), &hits, &PARAMS).unwrap();
        assert_eq!(m.duration_delta_s, None);
    }
}
