//! Local SQLite store: cache + provenance + curation for authoritative
//! source verdicts. Keyed by a stable per-track key (MBID when present,
//! else the normalized path).

mod error;
pub use error::StoreError;

#[cfg(test)]
mod tests;

use crate::sources::SourceVerdict;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::time::Duration;

/// Wait up to this long for a competing writer's SQLite lock before returning
/// `SQLITE_BUSY`. Belt-and-suspenders alongside the enrich single-instance lock
/// (issue #256): even if two processes touch the store, writes serialize
/// gracefully instead of erroring immediately.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// One persisted verdict row with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictRecord {
    pub track_key: String,
    pub mbid: Option<String>,
    pub server_name: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub duration_s: Option<i64>,
    pub source: String,
    pub source_track_id: Option<String>,
    pub source_verdict: SourceVerdict,
    pub match_confidence: f64,
    pub duration_delta_s: Option<i64>,
    pub curated_override: Option<SourceVerdict>,
    pub notes: Option<String>,
}

pub struct SourceStore {
    conn: Connection,
}

impl SourceStore {
    /// Open (creating if absent) a store at `path` and ensure the schema.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    fn init(conn: &Connection) -> Result<(), StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS source_verdicts (
                track_key        TEXT PRIMARY KEY,
                mbid             TEXT,
                server_name      TEXT,
                artist           TEXT,
                album            TEXT,
                title            TEXT,
                duration_s       INTEGER,
                source           TEXT NOT NULL,
                source_track_id  TEXT,
                source_verdict   TEXT NOT NULL
                    CHECK (source_verdict IN ('explicit', 'cleaned', 'not_explicit')),
                match_confidence REAL NOT NULL,
                duration_delta_s INTEGER,
                matched_at       TEXT NOT NULL DEFAULT (datetime('now')),
                curated_override TEXT
                    CHECK (curated_override IN ('explicit', 'cleaned', 'not_explicit')),
                notes            TEXT
            );",
        )?;
        Ok(())
    }

    /// Insert or update a verdict. Deliberately does NOT overwrite
    /// `curated_override`: user curation survives re-enrichment.
    pub fn upsert(&self, r: &VerdictRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO source_verdicts
                (track_key, mbid, server_name, artist, album, title, duration_s,
                 source, source_track_id, source_verdict, match_confidence,
                 duration_delta_s, curated_override, notes, matched_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14, datetime('now'))
             ON CONFLICT(track_key) DO UPDATE SET
                mbid=excluded.mbid, server_name=excluded.server_name,
                artist=excluded.artist, album=excluded.album, title=excluded.title,
                duration_s=excluded.duration_s, source=excluded.source,
                source_track_id=excluded.source_track_id,
                source_verdict=excluded.source_verdict,
                match_confidence=excluded.match_confidence,
                duration_delta_s=excluded.duration_delta_s,
                notes=excluded.notes, matched_at=datetime('now')",
            params![
                r.track_key,
                r.mbid,
                r.server_name,
                r.artist,
                r.album,
                r.title,
                r.duration_s,
                r.source,
                r.source_track_id,
                r.source_verdict.as_str(),
                r.match_confidence,
                r.duration_delta_s,
                r.curated_override.map(|v| v.as_str()),
                r.notes,
            ],
        )?;
        Ok(())
    }

    /// Fetch a stored row by key.
    pub fn get(&self, track_key: &str) -> Result<Option<VerdictRecord>, StoreError> {
        let raw = self
            .conn
            .query_row(
                "SELECT track_key, mbid, server_name, artist, album, title,
                        duration_s, source, source_track_id, source_verdict,
                        match_confidence, duration_delta_s, curated_override, notes
                 FROM source_verdicts WHERE track_key = ?1",
                params![track_key],
                RawRow::from_row,
            )
            .optional()?;
        // SQLite mapping (rusqlite) is separate from verdict validation
        // (StoreError): parse the verdict strings only after the row is loaded.
        raw.map(RawRow::into_record).transpose()
    }

    /// The verdict that should drive rating: the curated override if present,
    /// else the source verdict. `None` if the track is not in the store.
    pub fn effective_verdict(&self, track_key: &str) -> Result<Option<SourceVerdict>, StoreError> {
        Ok(self
            .get(track_key)?
            .map(|r| r.curated_override.unwrap_or(r.source_verdict)))
    }

    /// Set or clear the user curation override for a track.
    ///
    /// Returns `true` when a row was updated, `false` when no track matched
    /// `track_key` (so a caller can surface a missing-track curation instead of
    /// treating the silent no-op as success).
    pub fn set_curated(
        &self,
        track_key: &str,
        verdict: Option<SourceVerdict>,
    ) -> Result<bool, StoreError> {
        let affected = self.conn.execute(
            "UPDATE source_verdicts SET curated_override = ?2 WHERE track_key = ?1",
            params![track_key, verdict.map(|v| v.as_str())],
        )?;
        Ok(affected > 0)
    }
}

/// A row as loaded from SQLite, with the verdict columns still as raw strings.
/// Keeps the rusqlite row-mapping closure (`from_row`) infallible-of-StoreError
/// so verdict validation happens separately in `into_record`.
struct RawRow {
    track_key: String,
    mbid: Option<String>,
    server_name: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    title: Option<String>,
    duration_s: Option<i64>,
    source: String,
    source_track_id: Option<String>,
    source_verdict: String,
    match_confidence: f64,
    duration_delta_s: Option<i64>,
    curated_override: Option<String>,
    notes: Option<String>,
}

impl RawRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            track_key: row.get(0)?,
            mbid: row.get(1)?,
            server_name: row.get(2)?,
            artist: row.get(3)?,
            album: row.get(4)?,
            title: row.get(5)?,
            duration_s: row.get(6)?,
            source: row.get(7)?,
            source_track_id: row.get(8)?,
            source_verdict: row.get(9)?,
            match_confidence: row.get(10)?,
            duration_delta_s: row.get(11)?,
            curated_override: row.get(12)?,
            notes: row.get(13)?,
        })
    }

    fn into_record(self) -> Result<VerdictRecord, StoreError> {
        Ok(VerdictRecord {
            track_key: self.track_key,
            mbid: self.mbid,
            server_name: self.server_name,
            artist: self.artist,
            album: self.album,
            title: self.title,
            duration_s: self.duration_s,
            source: self.source,
            source_track_id: self.source_track_id,
            // Fail loud on an unrecognized stored value rather than silently
            // coercing it to a verdict (which would either downgrade explicit
            // content or fabricate an R rating). A CHECK constraint prevents
            // invalid values from being stored; this is the read-side backstop.
            source_verdict: parse_verdict_column(9, &self.source_verdict)?,
            match_confidence: self.match_confidence,
            duration_delta_s: self.duration_delta_s,
            curated_override: self
                .curated_override
                .map(|s| parse_verdict_column(12, &s))
                .transpose()?,
            notes: self.notes,
        })
    }
}

/// Parse a stored verdict string, returning `StoreError::InvalidVerdict` (not a
/// silent default) when the value is not a recognized verdict. `column` is the
/// 0-based result-set index, for the error message.
fn parse_verdict_column(column: usize, value: &str) -> Result<SourceVerdict, StoreError> {
    SourceVerdict::parse(value).ok_or_else(|| StoreError::InvalidVerdict {
        column,
        value: value.to_string(),
    })
}
