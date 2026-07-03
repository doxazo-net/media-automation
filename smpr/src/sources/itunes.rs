//! iTunes Search API adapter: the v1 authoritative advisory source.
//!
//! No account, key, or auth - a public HTTP GET. The HTTP fetch and the
//! response parsing are separated so the parsing (verdict mapping, duration
//! extraction) is unit-tested against fixtures with no live network in CI.

use crate::sources::{Source, SourceError, SourceHit, SourceVerdict, TrackQuery};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SEARCH_ENDPOINT: &str = "https://itunes.apple.com/search";
/// ~20 requests/minute: iTunes throttles unauthenticated callers around there.
const MIN_INTERVAL: Duration = Duration::from_millis(3000);
const MAX_RETRIES: u32 = 3;

/// iTunes Search API source.
pub struct ItunesSource {
    agent: ureq::Agent,
    min_interval: Duration,
    /// Earliest instant the next request may fire. Callers reserve their slot
    /// under the lock, then sleep after releasing it.
    next_allowed: Mutex<Instant>,
}

impl Default for ItunesSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ItunesSource {
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

    /// The `term` value: "artist title" when an artist is known, else the title.
    fn search_term(query: &TrackQuery) -> String {
        match &query.artist {
            Some(a) if !a.trim().is_empty() => format!("{} {}", a, query.title),
            _ => query.title.clone(),
        }
    }

    /// Space out calls to stay within iTunes' unauthenticated rate limit.
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

impl Source for ItunesSource {
    fn name(&self) -> &str {
        "itunes"
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
                .query("term", &term)
                .query("entity", "song")
                .query("limit", "25")
                .call()
                .map_err(|e| SourceError::Network(e.to_string()))?;
            let status = resp.status().as_u16();
            // iTunes signals rate-limiting with 403/429; back off and retry.
            // A genuine hard 403 is conflated here and pays the retries before
            // failing non-fatally - acceptable for the enrich crawl's purposes.
            if (status == 429 || status == 403) && attempt < MAX_RETRIES {
                attempt += 1;
                std::thread::sleep(Duration::from_secs(2 * u64::from(attempt)));
                continue;
            }
            if status >= 400 {
                // Include a truncated body snippet so blocks/rate-limits are
                // actionable (mirrors the media-server client's error shape).
                let body = resp.into_body().read_to_string().unwrap_or_default();
                let snippet = if body.len() > 512 {
                    format!("{}...", &body[..body.floor_char_boundary(512)])
                } else {
                    body
                };
                return Err(SourceError::Network(format!(
                    "iTunes HTTP {status}: {snippet}"
                )));
            }
            let body = resp
                .into_body()
                .read_to_string()
                .map_err(|e| SourceError::Network(format!("read iTunes body: {e}")))?;
            return parse_itunes_results(&body);
        }
    }
}

#[derive(serde::Deserialize)]
struct ItunesResponse {
    #[serde(default)]
    results: Vec<ItunesResult>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesResult {
    track_id: Option<i64>,
    track_name: Option<String>,
    artist_name: Option<String>,
    collection_name: Option<String>,
    track_time_millis: Option<i64>,
    track_explicitness: Option<String>,
}

/// Map the iTunes `trackExplicitness` field to a verdict. Unknown/absent values
/// map to `NotExplicit` (the positive-only tier means only a definite "explicit"
/// pulls a rating up; an unrecognized value must not fabricate that).
fn map_explicitness(value: Option<&str>) -> SourceVerdict {
    match value {
        Some("explicit") => SourceVerdict::Explicit,
        Some("cleaned") => SourceVerdict::Cleaned,
        _ => SourceVerdict::NotExplicit,
    }
}

/// Parse an iTunes Search API response body into candidate hits. Results with no
/// track name are dropped (a title is required to match). Pure - no I/O.
pub fn parse_itunes_results(json: &str) -> Result<Vec<SourceHit>, SourceError> {
    let resp: ItunesResponse =
        serde_json::from_str(json).map_err(|e| SourceError::Parse(e.to_string()))?;
    let hits = resp
        .results
        .into_iter()
        .filter_map(|r| {
            let title = r.track_name?;
            Some(SourceHit {
                source: "itunes".to_string(),
                source_track_id: r.track_id.map(|id| id.to_string()),
                artist: r.artist_name,
                album: r.collection_name,
                title,
                // Milliseconds to whole seconds, rounded to nearest.
                // saturating_add guards a pathological near-i64::MAX value.
                duration_s: r.track_time_millis.map(|ms| ms.saturating_add(500) / 1000),
                verdict: map_explicitness(r.track_explicitness.as_deref()),
            })
        })
        .collect();
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "resultCount": 3,
      "results": [
        {
          "trackId": 111,
          "trackName": "First Track",
          "artistName": "Some Artist",
          "collectionName": "Some Album",
          "trackTimeMillis": 215400,
          "trackExplicitness": "explicit"
        },
        {
          "trackId": 222,
          "trackName": "Second Track",
          "artistName": "Some Artist",
          "trackTimeMillis": 180000,
          "trackExplicitness": "cleaned"
        },
        {
          "trackId": 333,
          "trackName": "Third Track",
          "artistName": "Some Artist",
          "trackTimeMillis": 200000,
          "trackExplicitness": "notExplicit"
        }
      ]
    }"#;

    #[test]
    fn parses_verdicts_durations_and_ids() {
        let hits = parse_itunes_results(FIXTURE).unwrap();
        assert_eq!(hits.len(), 3);

        assert_eq!(hits[0].title, "First Track");
        assert_eq!(hits[0].source_track_id.as_deref(), Some("111"));
        assert_eq!(hits[0].duration_s, Some(215)); // 215400ms -> 215s
        assert_eq!(hits[0].verdict, SourceVerdict::Explicit);
        assert_eq!(hits[0].album.as_deref(), Some("Some Album"));

        assert_eq!(hits[1].verdict, SourceVerdict::Cleaned);
        assert_eq!(hits[1].album, None); // no collectionName in the fixture
        assert_eq!(hits[2].verdict, SourceVerdict::NotExplicit);
        assert_eq!(hits[2].duration_s, Some(200));
    }

    #[test]
    fn drops_results_without_a_title() {
        let json = r#"{ "results": [ { "trackId": 1, "trackExplicitness": "explicit" } ] }"#;
        assert!(parse_itunes_results(json).unwrap().is_empty());
    }

    #[test]
    fn empty_results_is_empty_vec() {
        assert!(
            parse_itunes_results(r#"{ "resultCount": 0, "results": [] }"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_json_is_parse_error() {
        assert!(matches!(
            parse_itunes_results("not json"),
            Err(SourceError::Parse(_))
        ));
    }

    #[test]
    fn unknown_explicitness_maps_to_not_explicit() {
        assert_eq!(map_explicitness(Some("weird")), SourceVerdict::NotExplicit);
        assert_eq!(map_explicitness(None), SourceVerdict::NotExplicit);
    }

    #[test]
    fn search_term_uses_artist_when_present() {
        let with = TrackQuery {
            artist: Some("Artist".to_string()),
            album: None,
            title: "Song".to_string(),
            duration_s: None,
        };
        assert_eq!(ItunesSource::search_term(&with), "Artist Song");

        let without = TrackQuery {
            artist: None,
            album: None,
            title: "Song".to_string(),
            duration_s: None,
        };
        assert_eq!(ItunesSource::search_term(&without), "Song");
    }
}
