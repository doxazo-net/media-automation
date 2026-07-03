use super::*;
use crate::sources::SourceVerdict;

fn sample(key: &str, verdict: SourceVerdict) -> VerdictRecord {
    VerdictRecord {
        track_key: key.to_string(),
        mbid: Some("mb-123".to_string()),
        server_name: Some("home-emby".to_string()),
        artist: Some("Some Artist".to_string()),
        album: Some("Some Album".to_string()),
        title: Some("Some Title".to_string()),
        duration_s: Some(215),
        source: "itunes".to_string(),
        source_track_id: Some("it-999".to_string()),
        source_verdict: verdict,
        match_confidence: 0.93,
        duration_delta_s: Some(1),
        curated_override: None,
        notes: None,
    }
}

#[test]
fn upsert_then_get_round_trips() {
    let store = SourceStore::open_in_memory().unwrap();
    let rec = sample("k1", SourceVerdict::Explicit);
    store.upsert(&rec).unwrap();
    let got = store.get("k1").unwrap().unwrap();
    assert_eq!(got.source_verdict, SourceVerdict::Explicit);
    assert_eq!(got.match_confidence, 0.93);
    assert_eq!(got.duration_s, Some(215));
}

#[test]
fn get_missing_key_is_none() {
    let store = SourceStore::open_in_memory().unwrap();
    assert!(store.get("nope").unwrap().is_none());
}

#[test]
fn upsert_same_key_updates_not_duplicates() {
    let store = SourceStore::open_in_memory().unwrap();
    store
        .upsert(&sample("k1", SourceVerdict::NotExplicit))
        .unwrap();
    store
        .upsert(&sample("k1", SourceVerdict::Explicit))
        .unwrap();
    let got = store.get("k1").unwrap().unwrap();
    assert_eq!(got.source_verdict, SourceVerdict::Explicit);
}

#[test]
fn curated_override_wins_over_source_verdict() {
    let store = SourceStore::open_in_memory().unwrap();
    store
        .upsert(&sample("k1", SourceVerdict::NotExplicit))
        .unwrap();
    assert_eq!(
        store.effective_verdict("k1").unwrap(),
        Some(SourceVerdict::NotExplicit)
    );

    store
        .set_curated("k1", Some(SourceVerdict::Explicit))
        .unwrap();
    assert_eq!(
        store.effective_verdict("k1").unwrap(),
        Some(SourceVerdict::Explicit)
    );
}

#[test]
fn curation_survives_reenrich() {
    let store = SourceStore::open_in_memory().unwrap();
    store
        .upsert(&sample("k1", SourceVerdict::NotExplicit))
        .unwrap();
    store
        .set_curated("k1", Some(SourceVerdict::Explicit))
        .unwrap();
    // Re-enrich overwrites the source verdict but must not clobber curation.
    store.upsert(&sample("k1", SourceVerdict::Cleaned)).unwrap();
    assert_eq!(
        store.effective_verdict("k1").unwrap(),
        Some(SourceVerdict::Explicit)
    );
}

#[test]
fn effective_verdict_missing_is_none() {
    let store = SourceStore::open_in_memory().unwrap();
    assert_eq!(store.effective_verdict("nope").unwrap(), None);
}

#[test]
fn upsert_conflict_maps_duration_delta_not_duration() {
    // Guards against an ON CONFLICT column mis-map: the update must set
    // duration_delta_s from the new row's delta, never from duration_s.
    let store = SourceStore::open_in_memory().unwrap();
    let mut first = sample("k1", SourceVerdict::Explicit);
    first.duration_delta_s = Some(9);
    store.upsert(&first).unwrap();

    let mut second = sample("k1", SourceVerdict::Explicit);
    second.duration_s = Some(215);
    second.duration_delta_s = Some(1);
    store.upsert(&second).unwrap();

    let got = store.get("k1").unwrap().unwrap();
    assert_eq!(got.duration_delta_s, Some(1)); // the new delta, not 9 and not 215
    assert_eq!(got.duration_s, Some(215));
}

#[test]
fn set_curated_none_clears_override() {
    let store = SourceStore::open_in_memory().unwrap();
    store
        .upsert(&sample("k1", SourceVerdict::NotExplicit))
        .unwrap();
    store
        .set_curated("k1", Some(SourceVerdict::Explicit))
        .unwrap();
    assert_eq!(
        store.effective_verdict("k1").unwrap(),
        Some(SourceVerdict::Explicit)
    );

    let cleared = store.set_curated("k1", None).unwrap();
    assert!(cleared);
    assert_eq!(
        store.effective_verdict("k1").unwrap(),
        Some(SourceVerdict::NotExplicit)
    );
}

#[test]
fn set_curated_missing_key_returns_false() {
    let store = SourceStore::open_in_memory().unwrap();
    assert!(
        !store
            .set_curated("nope", Some(SourceVerdict::Explicit))
            .unwrap()
    );
}

#[test]
fn check_constraint_rejects_invalid_verdict() {
    // Bypass the typed API to attempt storing an invalid verdict; the schema
    // CHECK constraint must reject it, so an invalid value can never reach the
    // read path in the first place.
    let store = SourceStore::open_in_memory().unwrap();
    let result = store.conn.execute(
        "INSERT INTO source_verdicts (track_key, source, source_verdict, match_confidence)
         VALUES ('k1', 'itunes', 'bogus', 0.5)",
        [],
    );
    assert!(
        result.is_err(),
        "CHECK constraint should reject an invalid source_verdict"
    );
}
