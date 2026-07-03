//! Local SQLite store: cache + provenance + curation for authoritative
//! source verdicts. Keyed by a stable per-track key (MBID when present,
//! else the normalized path).

#[cfg(test)]
mod tests;

use crate::sources::SourceVerdict;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

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
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    fn init(conn: &Connection) -> rusqlite::Result<()> {
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
        )
    }

    /// Insert or update a verdict. Deliberately does NOT overwrite
    /// `curated_override`: user curation survives re-enrichment.
    pub fn upsert(&self, r: &VerdictRecord) -> rusqlite::Result<()> {
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
    pub fn get(&self, track_key: &str) -> rusqlite::Result<Option<VerdictRecord>> {
        self.conn
            .query_row(
                "SELECT track_key, mbid, server_name, artist, album, title,
                        duration_s, source, source_track_id, source_verdict,
                        match_confidence, duration_delta_s, curated_override, notes
                 FROM source_verdicts WHERE track_key = ?1",
                params![track_key],
                Self::row_to_record,
            )
            .optional()
    }

    /// The verdict that should drive rating: the curated override if present,
    /// else the source verdict. `None` if the track is not in the store.
    pub fn effective_verdict(&self, track_key: &str) -> rusqlite::Result<Option<SourceVerdict>> {
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
    ) -> rusqlite::Result<bool> {
        let affected = self.conn.execute(
            "UPDATE source_verdicts SET curated_override = ?2 WHERE track_key = ?1",
            params![track_key, verdict.map(|v| v.as_str())],
        )?;
        Ok(affected > 0)
    }

    fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<VerdictRecord> {
        let verdict_s: String = row.get(9)?;
        let curated_s: Option<String> = row.get(12)?;
        Ok(VerdictRecord {
            track_key: row.get(0)?,
            mbid: row.get(1)?,
            server_name: row.get(2)?,
            artist: row.get(3)?,
            album: row.get(4)?,
            title: row.get(5)?,
            duration_s: row.get(6)?,
            source: row.get(7)?,
            source_track_id: row.get(8)?,
            // Fail loud on an unrecognized stored value rather than silently
            // coercing it to a verdict (which would either downgrade explicit
            // content or fabricate an R rating). A CHECK constraint prevents
            // invalid values from being stored; this is the read-side backstop.
            source_verdict: parse_verdict_column(9, &verdict_s)?,
            match_confidence: row.get(10)?,
            duration_delta_s: row.get(11)?,
            curated_override: curated_s
                .map(|s| parse_verdict_column(12, &s))
                .transpose()?,
            notes: row.get(13)?,
        })
    }
}

/// Parse a stored verdict string, returning a rusqlite conversion error (not a
/// silent default) when the value is not a recognized verdict. `column` is the
/// 0-based result-set index, for the error message.
fn parse_verdict_column(column: usize, value: &str) -> rusqlite::Result<SourceVerdict> {
    SourceVerdict::parse(value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            format!("invalid verdict value {value:?}").into(),
        )
    })
}
