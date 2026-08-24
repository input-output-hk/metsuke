//! The rebuildable index (ADR 0005), holding two things the archive alone
//! answers too slowly: the highest counter accepted per pool (ADR 0002), and
//! one row per stored object for the developer listing. A reservation holds
//! the counter write open until its caller has stored the batch, so of two
//! submissions racing on one counter exactly one both stores and records.
//!
//! A submission row is its object key and nothing else. Everything a listing
//! reports is encoded in that key, so a row written at ingest and one written
//! by `rebuild` cannot disagree, and the envelope metadata stays where ADR
//! 0005 puts it — on the object.

use std::num::NonZeroU32;
use std::path::Path;

use metsuke_wire::envelope::PoolId;
use rusqlite::Connection;
use time::OffsetDateTime;

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

/// What a submitted counter is worth against the pool's recorded state.
pub enum Reservation<'a> {
    Reserved(Reserved<'a>),
    /// The counter did not advance: a replay, or a client whose spool was
    /// restored from an older copy.
    Replayed {
        last: u64,
    },
}

/// A counter claimed but not yet spent. Committing is the caller's last
/// step, so a counter can only be spent by a submission that got that far
/// (ADR 0002).
pub struct Reserved<'a> {
    transaction: rusqlite::Transaction<'a>,
}

impl Reserved<'_> {
    pub fn commit(self) -> Result<(), IndexError> {
        Ok(self.transaction.commit()?)
    }

    /// Record the stored object inside the reservation. The ingest path's way
    /// in; `Index::record` is every other caller's.
    pub fn record(&self, name: &ObjectName) -> Result<(), IndexError> {
        insert(&self.transaction, name)
    }
}

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE counters (
        pool_id TEXT PRIMARY KEY,
        last_counter INTEGER NOT NULL,
        -- Unix seconds. Part of the ADR-0002 counter state: the operator's
        -- evidence of when a pool last got through.
        last_seen INTEGER NOT NULL
    );",
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

    /// Claim `counter` for `pool` if it is past everything accepted so far.
    /// The upsert's WHERE clause is the monotonicity check: the row is
    /// written only when the new counter wins. The write stays uncommitted
    /// until the returned `Reservation` is committed, and SQLite holds the
    /// table against other writers meanwhile, so a second submission on the
    /// same counter waits and then loses. A reservation dropped uncommitted
    /// rolls back, which is what leaves a failed store's counter unspent.
    /// The lock is held for as long as the caller takes to store, so
    /// accepted uploads commit one at a time: far from binding at the
    /// hourly per-pool cadence, and per-pool locking if that ever changes.
    pub fn reserve(
        &mut self,
        pool: PoolId,
        counter: u64,
        seen_at: OffsetDateTime,
    ) -> Result<Reservation<'_>, IndexError> {
        let transaction = self.conn.transaction()?;
        let written = transaction.execute(
            "INSERT INTO counters (pool_id, last_counter, last_seen) VALUES (?1, ?2, ?3)
             ON CONFLICT(pool_id) DO UPDATE SET last_counter = excluded.last_counter,
                 last_seen = excluded.last_seen
             WHERE excluded.last_counter > counters.last_counter",
            rusqlite::params![
                pool.to_bech32(),
                // SQLite integers are i64; a counter past i64::MAX would take
                // 2^63 uploads from one pool, and a wrapped value would only
                // fail to advance.
                counter as i64,
                seen_at.unix_timestamp()
            ],
        )?;
        if written == 1 {
            return Ok(Reservation::Reserved(Reserved { transaction }));
        }
        let last: i64 = transaction.query_row(
            "SELECT last_counter FROM counters WHERE pool_id = ?1",
            [pool.to_bech32()],
            |row| row.get(0),
        )?;
        Ok(Reservation::Replayed { last: last as u64 })
    }

    /// Record a stored object. Called outside a reservation — `rebuild` walks
    /// a whole listing — where `Reserved::record` is the ingest path's.
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

    /// The highest counter accepted for `pool`, `None` before its first
    /// submission.
    pub fn last_counter(&self, pool: PoolId) -> Result<Option<u64>, IndexError> {
        let last: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_counter FROM counters WHERE pool_id = ?1",
                [pool.to_bech32()],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(last.map(|last| last as u64))
    }
}
