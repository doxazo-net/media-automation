use std::fmt;

/// Error type for source-verdict store operations. Wraps the SQLite backend so
/// the store's public API does not leak `rusqlite` into the rest of the crate
/// (mirrors `MediaServerError` / `ConfigError` / `RatingError`).
#[derive(Debug)]
pub enum StoreError {
    /// Underlying SQLite error.
    Sqlite(rusqlite::Error),
    /// A stored verdict string was not a recognized value (data drift, or a
    /// row that bypassed the schema CHECK constraint). `column` is the 0-based
    /// result-set index the value came from.
    InvalidVerdict { column: usize, value: String },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "store error: {e}"),
            Self::InvalidVerdict { column, value } => {
                write!(f, "invalid stored verdict at column {column}: {value:?}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::InvalidVerdict { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}
