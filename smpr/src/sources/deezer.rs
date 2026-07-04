//! Deezer API adapter: an authoritative advisory source.
//!
//! Like the iTunes adapter, this is a public HTTP GET - no account, key, or
//! auth. Deezer differs from iTunes in three ways that shape this adapter:
//!   1. It exposes a granular `explicit_content_lyrics` integer code (plus an
//!      `explicit_lyrics` bool fallback), and - unlike the iTunes Search API,
//!      which never returns an "explicit" flag in practice - it actually flags
//!      labeled explicit catalog.
//!   2. `duration` is already whole seconds (no milliseconds division).
//!   3. Rate-limiting is signaled with an HTTP 200 body carrying an `error`
//!      object (`code == 4`, quota), NOT a 429/403 status - so the body must be
//!      inspected, not just the status.
//!
//! The HTTP fetch and the response parsing are separated so the parsing (verdict
//! mapping, duration/id extraction, error detection) is unit-tested against
//! fixtures with no live network in CI.

use crate::sources::{Source, SourceError, SourceHit, SourceVerdict, TrackQuery};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SEARCH_ENDPOINT: &str = "https://api.deezer.com/search";
/// Deezer allows ~50 requests / 5s for unauthenticated callers; 200ms (5/s) is
/// comfortably under that.
const MIN_INTERVAL: Duration = Duration::from_millis(200);
const MAX_RETRIES: u32 = 3;
/// Deezer's quota-exceeded error code (returned in a 200-status body).
const DEEZER_QUOTA_CODE: i64 = 4;

/// Deezer search API source.
pub struct DeezerSource {
    agent: ureq::Agent,
    min_interval: Duration,
    /// Earliest instant the next request may fire. Callers reserve their slot
    /// under the lock, then sleep after releasing it.
    next_allowed: Mutex<Instant>,
}

impl Default for DeezerSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DeezerSource {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_per_call(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self {
            agent,
            min_interval: MIN_INTERVAL,
            next_allowed: Mutex::new(Instant::now()),
        }
    }

    /// The `q` value: "artist title" when an artist is known, else the title.
    /// Free-text (not Deezer's `artist:"" track:""` advanced syntax) so the
    /// matcher receives candidates to gate rather than relying on an exact
    /// catalog string match.
    fn search_term(query: &TrackQuery) -> String {
        match &query.artist {
            Some(a) if !a.trim().is_empty() => format!("{} {}", a, query.title),
            _ => query.title.clone(),
        }
    }

    /// Space out calls to stay within Deezer's unauthenticated rate limit.
    /// Reserves the next slot under the lock, then sleeps after releasing it, so
    /// a concurrent caller can compute its own (later) slot without blocking on
    /// this one's sleep. Recovers from a poisoned lock rather than panicking.
    fn throttle(&self) {
        let wait = {
            let mut next = self
                .next_allowed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let slot = (*next).max(now);
            *next = slot + self.min_interval;
            slot.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

impl Source for DeezerSource {
    fn name(&self) -> &str {
        "deezer"
    }

    fn lookup(&self, query: &TrackQuery) -> Result<Vec<SourceHit>, SourceError> {
        let term = Self::search_term(query);
        let mut attempt = 0;
        loop {
            // Throttle every attempt (not just the first) so retries still honor
            // the rate cap.
            self.throttle();
            let resp = self
                .agent
                .get(SEARCH_ENDPOINT)
                .query("q", &term)
                .call()
                .map_err(|e| SourceError::Network(e.to_string()))?;
            let status = resp.status().as_u16();
            if status >= 400 {
                // Deezer generally signals quota via a 200 body (handled in
                // parse), but surface a genuine HTTP error with a body snippet.
                let body = resp.into_body().read_to_string().unwrap_or_default();
                let snippet = if body.len() > 512 {
                    format!("{}...", &body[..body.floor_char_boundary(512)])
                } else {
                    body
                };
                return Err(SourceError::Network(format!(
                    "Deezer HTTP {status}: {snippet}"
                )));
            }
            let body = resp
                .into_body()
                .read_to_string()
                .map_err(|e| SourceError::Network(format!("read Deezer body: {e}")))?;
            match parse_deezer_results(&body) {
                // A quota error arrives with HTTP 200; back off and retry.
                Err(SourceError::Network(msg)) if is_quota_error(&msg) && attempt < MAX_RETRIES => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_secs(2 * u64::from(attempt)));
                    continue;
                }
                other => return other,
            }
        }
    }
}

/// Marker embedded in the quota-error message so `lookup` can distinguish a
/// retriable quota error from a non-retriable API error without threading a
/// dedicated error variant through the pure parser.
const QUOTA_MARKER: &str = "Deezer quota exceeded";

fn is_quota_error(msg: &str) -> bool {
    msg.contains(QUOTA_MARKER)
}

#[derive(serde::Deserialize)]
struct DeezerResponse {
    #[serde(default)]
    data: Vec<DeezerTrack>,
    error: Option<DeezerError>,
}

#[derive(serde::Deserialize)]
struct DeezerError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(serde::Deserialize)]
struct DeezerTrack {
    id: Option<i64>,
    title: Option<String>,
    /// Whole seconds (Deezer returns duration in seconds, not milliseconds).
    duration: Option<i64>,
    explicit_lyrics: Option<bool>,
    explicit_content_lyrics: Option<i64>,
    artist: Option<DeezerArtist>,
    album: Option<DeezerAlbum>,
}

#[derive(serde::Deserialize)]
struct DeezerArtist {
    name: Option<String>,
}

#[derive(serde::Deserialize)]
struct DeezerAlbum {
    title: Option<String>,
}

/// Map Deezer's advisory signal to a verdict. Prefers the granular
/// `explicit_content_lyrics` code (1 = explicit, 3 = edited/clean version),
/// falling back to the `explicit_lyrics` bool when the code is absent.
///
/// Positive-only, like the iTunes mapping: only a definite "explicit" pulls a
/// rating up. Every other code (0 no-advice, 2 unknown, 4 partially explicit at
/// album level, 5 partially unknown, 6 no-advice-available) and a `false`/absent
/// bool map to `NotExplicit` so an ambiguous value never fabricates an R.
fn map_deezer_verdict(code: Option<i64>, explicit_lyrics: Option<bool>) -> SourceVerdict {
    match code {
        Some(1) => SourceVerdict::Explicit,
        Some(3) => SourceVerdict::Cleaned,
        Some(_) => SourceVerdict::NotExplicit,
        None => match explicit_lyrics {
            Some(true) => SourceVerdict::Explicit,
            _ => SourceVerdict::NotExplicit,
        },
    }
}

/// Parse a Deezer search response body into candidate hits. Results with no
/// title are dropped (a title is required to match). A quota error (code 4)
/// becomes a `Network` error carrying `QUOTA_MARKER` so `lookup` can retry;
/// any other populated `error` object is a non-retriable `Network` error. Pure -
/// no I/O.
///
/// A `/search` success envelope has no `error` key at all; only treat `error` as
/// an error when it actually carries a `code` or `message`, so a fieldless/empty
/// `error` value (serde deserializes a JSON `[]` into an all-`None` struct) is
/// not mistaken for a failure that would silently disable the source.
pub fn parse_deezer_results(json: &str) -> Result<Vec<SourceHit>, SourceError> {
    let resp: DeezerResponse =
        serde_json::from_str(json).map_err(|e| SourceError::Parse(e.to_string()))?;
    if let Some(err) = resp.error
        && (err.code.is_some() || err.message.is_some())
    {
        let msg = err
            .message
            .unwrap_or_else(|| "unknown Deezer error".to_string());
        if err.code == Some(DEEZER_QUOTA_CODE) {
            return Err(SourceError::Network(format!("{QUOTA_MARKER}: {msg}")));
        }
        return Err(SourceError::Network(format!("Deezer API error: {msg}")));
    }
    let hits = resp
        .data
        .into_iter()
        .filter_map(|t| {
            let title = t.title?;
            Some(SourceHit {
                source: "deezer".to_string(),
                source_track_id: t.id.map(|id| id.to_string()),
                artist: t.artist.and_then(|a| a.name),
                album: t.album.and_then(|a| a.title),
                title,
                duration_s: t.duration,
                verdict: map_deezer_verdict(t.explicit_content_lyrics, t.explicit_lyrics),
            })
        })
        .collect();
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "data": [
        {
          "id": 111,
          "title": "First Track",
          "duration": 215,
          "explicit_lyrics": true,
          "explicit_content_lyrics": 1,
          "artist": { "name": "Some Artist" },
          "album": { "title": "Some Album" }
        },
        {
          "id": 222,
          "title": "Second Track",
          "duration": 180,
          "explicit_lyrics": false,
          "explicit_content_lyrics": 3,
          "artist": { "name": "Some Artist" }
        },
        {
          "id": 333,
          "title": "Third Track",
          "duration": 200,
          "explicit_lyrics": false,
          "explicit_content_lyrics": 0,
          "artist": { "name": "Some Artist" }
        }
      ],
      "total": 3
    }"#;

    #[test]
    fn parses_verdicts_durations_and_ids() {
        let hits = parse_deezer_results(FIXTURE).unwrap();
        assert_eq!(hits.len(), 3);

        assert_eq!(hits[0].title, "First Track");
        assert_eq!(hits[0].source, "deezer");
        assert_eq!(hits[0].source_track_id.as_deref(), Some("111"));
        assert_eq!(hits[0].duration_s, Some(215)); // already seconds, no /1000
        assert_eq!(hits[0].verdict, SourceVerdict::Explicit);
        assert_eq!(hits[0].artist.as_deref(), Some("Some Artist"));
        assert_eq!(hits[0].album.as_deref(), Some("Some Album"));

        assert_eq!(hits[1].verdict, SourceVerdict::Cleaned); // code 3 = edited
        assert_eq!(hits[1].album, None); // no album in the fixture
        assert_eq!(hits[2].verdict, SourceVerdict::NotExplicit); // code 0
        assert_eq!(hits[2].duration_s, Some(200));
    }

    #[test]
    fn code_takes_precedence_over_bool() {
        // An explicit code wins even if the bool disagrees, and vice versa.
        assert_eq!(
            map_deezer_verdict(Some(1), Some(false)),
            SourceVerdict::Explicit
        );
        assert_eq!(
            map_deezer_verdict(Some(0), Some(true)),
            SourceVerdict::NotExplicit
        );
    }

    #[test]
    fn bool_fallback_when_code_absent() {
        assert_eq!(
            map_deezer_verdict(None, Some(true)),
            SourceVerdict::Explicit
        );
        assert_eq!(
            map_deezer_verdict(None, Some(false)),
            SourceVerdict::NotExplicit
        );
        assert_eq!(map_deezer_verdict(None, None), SourceVerdict::NotExplicit);
    }

    #[test]
    fn unknown_code_maps_to_not_explicit() {
        // Positive-only: only code 1 lifts a rating; 6 (no-advice) et al. do not.
        assert_eq!(
            map_deezer_verdict(Some(6), None),
            SourceVerdict::NotExplicit
        );
        assert_eq!(
            map_deezer_verdict(Some(2), None),
            SourceVerdict::NotExplicit
        );
    }

    #[test]
    fn drops_results_without_a_title() {
        let json = r#"{ "data": [ { "id": 1, "explicit_content_lyrics": 1 } ] }"#;
        assert!(parse_deezer_results(json).unwrap().is_empty());
    }

    #[test]
    fn empty_data_is_empty_vec() {
        assert!(
            parse_deezer_results(r#"{ "data": [], "total": 0 }"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn quota_error_is_retriable_network_error() {
        let json =
            r#"{ "error": { "type": "Exception", "message": "Quota limit exceeded", "code": 4 } }"#;
        let err = parse_deezer_results(json).unwrap_err();
        match err {
            SourceError::Network(msg) => {
                assert!(is_quota_error(&msg), "expected quota marker: {msg}")
            }
            other => panic!("expected Network quota error, got {other:?}"),
        }
    }

    #[test]
    fn empty_error_value_is_not_treated_as_failure() {
        // Deezer /search success has no `error` key, but a fieldless/empty
        // `error` (serde reads a JSON `[]` into an all-None struct) must not be
        // mistaken for a failure that silently disables the source.
        assert!(
            parse_deezer_results(r#"{ "data": [], "error": [] }"#)
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_deezer_results(r#"{ "data": [], "error": {} }"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn non_quota_api_error_is_not_retriable() {
        let json = r#"{ "error": { "type": "Exception", "message": "Invalid", "code": 500 } }"#;
        let err = parse_deezer_results(json).unwrap_err();
        match err {
            SourceError::Network(msg) => assert!(!is_quota_error(&msg), "must not be quota: {msg}"),
            other => panic!("expected Network error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_parse_error() {
        assert!(matches!(
            parse_deezer_results("not json"),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn search_term_uses_artist_when_present() {
        let with = TrackQuery {
            artist: Some("Artist".to_string()),
            album: None,
            title: "Song".to_string(),
            duration_s: None,
        };
        assert_eq!(DeezerSource::search_term(&with), "Artist Song");

        let without = TrackQuery {
            artist: None,
            album: None,
            title: "Song".to_string(),
            duration_s: None,
        };
        assert_eq!(DeezerSource::search_term(&without), "Song");
    }
}
