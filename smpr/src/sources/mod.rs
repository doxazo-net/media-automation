//! Authoritative parental-advisory source adapters and shared types.
//!
//! The `Source` trait and concrete adapters (iTunes, Spotify) arrive in later
//! milestones; for now this module defines the shared verdict type that
//! `crate::store` persists.

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

#[cfg(test)]
mod tests {
    use super::*;

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
