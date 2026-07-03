//! Authoritative parental-advisory source adapters and shared types.
//!
//! Concrete adapters (iTunes, Spotify) arrive in later milestones; this module
//! defines the shared verdict/query/hit types and the `Source` trait, plus (via
//! `matcher`) the pure confidence-gated matching logic.

pub mod matcher;

use std::fmt;

/// A store's declared advisory verdict for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceVerdict {
    /// Explicitly flagged (Parental Advisory). Maps to rating "R".
    Explicit,
    /// A clean/radio edit of an explicit original. Not treated as R.
    Cleaned,
    /// No advisory flag. Falls through to lyrics.
    NotExplicit,
}

impl SourceVerdict {
    /// Stable string form persisted in the store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Cleaned => "cleaned",
            Self::NotExplicit => "not_explicit",
        }
    }

    /// Parse the persisted string form; `None` on an unrecognized value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "explicit" => Some(Self::Explicit),
            "cleaned" => Some(Self::Cleaned),
            "not_explicit" => Some(Self::NotExplicit),
            _ => None,
        }
    }
}

/// A local track to match against a source's catalog.
#[derive(Debug, Clone)]
pub struct TrackQuery {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: String,
    pub duration_s: Option<i64>,
}

/// A candidate match returned by a source adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceHit {
    pub source: String,
    pub source_track_id: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: String,
    pub duration_s: Option<i64>,
    pub verdict: SourceVerdict,
}

/// A confidence-gated match of a query to a source hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub hit: SourceHit,
    pub confidence: f64,
    pub duration_delta_s: Option<i64>,
}

/// Errors an authoritative source adapter can return. Variants are exercised by
/// the concrete adapters in later milestones.
#[derive(Debug)]
pub enum SourceError {
    /// Network/transport failure talking to the source.
    Network(String),
    /// The source responded but the body could not be parsed.
    Parse(String),
    /// The source is configured but disabled (e.g. missing credentials).
    Disabled,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "source network error: {m}"),
            Self::Parse(m) => write!(f, "source parse error: {m}"),
            Self::Disabled => write!(f, "source is disabled"),
        }
    }
}

impl std::error::Error for SourceError {}

/// An authoritative advisory source. Concrete adapters (iTunes, Spotify) land in
/// later milestones; this defines the contract so the matcher can be written and
/// tested against it.
pub trait Source {
    /// Stable identifier, e.g. "itunes".
    fn name(&self) -> &str;
    /// Return candidate hits for a query (unranked; the matcher gates them).
    fn lookup(&self, query: &TrackQuery) -> Result<Vec<SourceHit>, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_hit_carries_verdict() {
        let hit = SourceHit {
            source: "itunes".to_string(),
            source_track_id: Some("1".to_string()),
            artist: Some("A".to_string()),
            album: None,
            title: "T".to_string(),
            duration_s: Some(200),
            verdict: SourceVerdict::Explicit,
        };
        assert_eq!(hit.verdict, SourceVerdict::Explicit);
    }

    #[test]
    fn verdict_str_round_trips() {
        for v in [
            SourceVerdict::Explicit,
            SourceVerdict::Cleaned,
            SourceVerdict::NotExplicit,
        ] {
            assert_eq!(SourceVerdict::parse(v.as_str()), Some(v));
        }
        assert_eq!(SourceVerdict::parse("bogus"), None);
    }
}
