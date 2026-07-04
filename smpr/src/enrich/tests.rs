use super::*;
use crate::server::types::AudioItemView;
use crate::sources::{SourceError, SourceHit, SourceVerdict};
use std::collections::HashMap;

/// A `Source` that returns a fixed hit list, for testing the enrich core with no
/// network.
struct MockSource {
    name: String,
    hits: Vec<SourceHit>,
}

impl Source for MockSource {
    fn name(&self) -> &str {
        &self.name
    }
    fn lookup(&self, _query: &TrackQuery) -> Result<Vec<SourceHit>, SourceError> {
        Ok(self.hits.clone())
    }
}

fn item(name: Option<&str>, path: Option<&str>) -> AudioItemView {
    AudioItemView {
        id: "id-1".to_string(),
        name: name.map(str::to_string),
        path: path.map(str::to_string),
        official_rating: None,
        album_artist: Some("Some Artist".to_string()),
        album: Some("Some Album".to_string()),
        genres: vec![],
        run_time_ticks: Some(2_000_000_000), // 200s
        provider_ids: None,
        date_created: None,
    }
}

fn hit(title: &str, artist: &str, dur: i64, verdict: SourceVerdict) -> SourceHit {
    SourceHit {
        source: "mock".to_string(),
        source_track_id: Some("t1".to_string()),
        artist: Some(artist.to_string()),
        album: None,
        title: title.to_string(),
        duration_s: Some(dur),
        verdict,
    }
}

fn sm(verdict: SourceVerdict, confidence: f64) -> SourceMatch {
    SourceMatch {
        source: "mock".to_string(),
        hit: hit("T", "A", 200, verdict),
        confidence,
        duration_delta_s: Some(0),
    }
}

const PARAMS: MatchParams = MatchParams {
    min_confidence: 0.85,
    duration_tolerance_s: 3,
};

#[test]
fn track_query_requires_a_title() {
    assert!(track_query_from_item(&item(Some("Song"), None)).is_some());
    assert!(track_query_from_item(&item(None, None)).is_none());
    assert!(track_query_from_item(&item(Some("   "), None)).is_none());
}

#[test]
fn track_query_carries_artist_album_duration() {
    let q = track_query_from_item(&item(Some("Song"), None)).unwrap();
    assert_eq!(q.title, "Song");
    assert_eq!(q.artist.as_deref(), Some("Some Artist"));
    assert_eq!(q.album.as_deref(), Some("Some Album"));
    assert_eq!(q.duration_s, Some(200)); // 2_000_000_000 ticks / 1e7
}

#[test]
fn track_key_prefers_mbid_then_path_then_id() {
    let mut it = item(Some("Song"), Some("C:\\Music\\Song.flac"));
    // No provider ids -> normalized path.
    assert_eq!(track_key_for_item(&it), "c:/music/song.flac");

    // MBID present -> wins over path.
    it.provider_ids = Some(HashMap::from([(
        "MusicBrainzTrack".to_string(),
        "mb-xyz".to_string(),
    )]));
    assert_eq!(track_key_for_item(&it), "mb-xyz");

    // Neither mbid nor path -> opaque id.
    let bare = item(Some("Song"), None);
    assert_eq!(track_key_for_item(&bare), "id-1");
}

#[test]
fn reconcile_positive_wins_over_higher_confidence_clean() {
    // A lower-confidence Explicit beats a higher-confidence NotExplicit.
    let chosen = reconcile(vec![
        sm(SourceVerdict::NotExplicit, 0.99),
        sm(SourceVerdict::Explicit, 0.90),
    ])
    .unwrap();
    assert_eq!(chosen.hit.verdict, SourceVerdict::Explicit);
}

#[test]
fn reconcile_without_explicit_takes_highest_confidence() {
    let chosen = reconcile(vec![
        sm(SourceVerdict::NotExplicit, 0.90),
        sm(SourceVerdict::Cleaned, 0.97),
    ])
    .unwrap();
    assert_eq!(chosen.hit.verdict, SourceVerdict::Cleaned);
    assert_eq!(chosen.confidence, 0.97);
}

#[test]
fn reconcile_empty_is_none() {
    assert!(reconcile(vec![]).is_none());
}

#[test]
fn match_track_gates_and_labels_source() {
    let q = TrackQuery {
        artist: Some("Some Artist".to_string()),
        album: None,
        title: "Song".to_string(),
        duration_s: Some(200),
    };
    let sources: Vec<Box<dyn Source>> = vec![Box::new(MockSource {
        name: "mock".to_string(),
        hits: vec![
            hit("Song", "Some Artist", 201, SourceVerdict::Explicit), // matches
            hit("Totally Different", "Nobody", 60, SourceVerdict::Explicit), // rejected
        ],
    })];
    let matches = match_track(&q, &sources, &PARAMS);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].source, "mock");
    assert_eq!(matches[0].hit.title, "Song");
    assert_eq!(matches[0].hit.verdict, SourceVerdict::Explicit);
}

#[test]
fn match_track_empty_when_nothing_clears_the_gate() {
    let q = TrackQuery {
        artist: Some("Some Artist".to_string()),
        album: None,
        title: "Song".to_string(),
        duration_s: Some(200),
    };
    let sources: Vec<Box<dyn Source>> = vec![Box::new(MockSource {
        name: "mock".to_string(),
        hits: vec![hit(
            "Wrong Duration",
            "Some Artist",
            400,
            SourceVerdict::Explicit,
        )],
    })];
    assert!(match_track(&q, &sources, &PARAMS).is_empty());
}

#[test]
fn watermark_advances_only_on_clean_unscoped_write_run() {
    use super::should_advance_watermark;
    // Eligible: write mode, unscoped, no persist failure.
    assert!(should_advance_watermark(false, false, false));
    // A swallowed upsert failure must hold the watermark back (issue #257 review).
    assert!(!should_advance_watermark(false, false, true));
    // Scoped runs never advance the server-wide watermark.
    assert!(!should_advance_watermark(false, true, false));
    // Report-only never advances.
    assert!(!should_advance_watermark(true, false, false));
}
