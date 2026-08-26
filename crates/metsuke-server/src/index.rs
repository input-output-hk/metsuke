//! The rebuildable index (ADR 0005), holding the one thing the archive alone
//! answers too slowly: one row per stored object, for the developer listing.
//!
//! A submission row is its object key and nothing else. Everything a listing
//! reports is encoded in that key, so a row written at ingest and one written
//! by `rebuild` cannot disagree, and the envelope metadata stays where ADR
//! 0005 puts it — on the object.

use std::num::NonZeroU32;
use std::path::Path;

use rusqlite::Connection;

use crate::archive::{ObjectName, ObjectNameError};

pub struct Index {
    conn: Connection,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migrate(#[from] metsuke_wire::sqlite::MigrateError),
    /// A row nothing can parse back: the index was written by something other
    /// than `record`, and reporting the key as a submission would hand a
    /// developer a key the archive does not hold.
    #[error("the index holds a row that is not an object key: {0}")]
    ObjectName(#[from] ObjectNameError),
}

const MIGRATIONS: &[&str] = &[
    // One column, because the key is the whole row (see the module header).
    // Its primary-key index is what orders and pages the listing.
    "CREATE TABLE submissions (object_key TEXT PRIMARY KEY);",
];

/// Write one submission row. Re-recording a key is not an error and changes
/// nothing: the key determines the row, so `rebuild` over an index that
/// already holds it has nothing to correct.
fn insert(conn: &Connection, name: &ObjectName) -> Result<(), IndexError> {
    conn.execute(
        "INSERT INTO submissions (object_key) VALUES (?1) ON CONFLICT DO NOTHING",
        [name.to_key()],
    )?;
    Ok(())
}

/// One page of the archive as the index holds it.
#[derive(Debug)]
pub struct Listing {
    pub objects: Vec<ObjectName>,
    /// The bound cut the answer off, so there is more after the last key.
    /// Reported rather than implied: a caller that reads a short page as the
    /// whole archive would silently miss everything past it.
    pub truncated: bool,
}

impl Index {
    /// Open (creating if absent) and migrate the index.
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        let conn = Connection::open(path)?;
        metsuke_wire::sqlite::migrate(&conn, MIGRATIONS)?;
        Ok(Index { conn })
    }

    /// Record a stored object, at ingest or over a whole listing (`rebuild`).
    pub fn record(&self, name: &ObjectName) -> Result<(), IndexError> {
        insert(&self.conn, name)
    }

    /// Whether the archive holds this key, as far as the index knows. What the
    /// download route asks before it reaches for the object: a key nobody
    /// stored is answered without a bucket round trip, and a fetch that then
    /// fails is the archive being unavailable rather than the key being wrong.
    pub fn holds(&self, key: &str) -> Result<bool, IndexError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM submissions WHERE object_key = ?1)",
            [key],
            |row| row.get(0),
        )?)
    }

    /// The objects whose key starts with `prefix` and sorts after `after`, in
    /// key order, at most `limit` of them. Both filters read the key, which is
    /// what makes them a pool-and-day filter and a page cursor at once: an
    /// empty `prefix` is the whole archive and an empty `after` its start.
    pub fn submissions(
        &self,
        prefix: &str,
        after: &str,
        limit: NonZeroU32,
    ) -> Result<Listing, IndexError> {
        // One past the bound, so a full page is told from a cut-off one
        // without a second count(*) over the same rows. `i64` is what SQLite
        // binds, and a `NonZeroU32` plus one always fits.
        let asked = i64::from(limit.get()) + 1;
        let mut statement = self.conn.prepare(
            "SELECT object_key FROM submissions
             WHERE object_key > ?1 AND object_key GLOB ?2
             ORDER BY object_key LIMIT ?3",
        )?;
        // GLOB rather than LIKE: LIKE would read `_` and `%` in a prefix a
        // client chose as wildcards. `[` is GLOB's own, so it is refused
        // before it reaches here (`developer::Filters`).
        let rows = statement.query_map(
            rusqlite::params![after, format!("{prefix}*"), asked],
            |row| row.get::<_, String>(0),
        )?;
        let mut keys = Vec::new();
        for key in rows {
            keys.push(key?);
        }
        let truncated = keys.len() as i64 == asked;
        if truncated {
            keys.pop();
        }
        let objects = keys
            .iter()
            .map(|key| ObjectName::parse(key))
            .collect::<Result<Vec<ObjectName>, ObjectNameError>>()?;
        Ok(Listing { objects, truncated })
    }
}
