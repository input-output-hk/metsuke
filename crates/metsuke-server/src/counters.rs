//! Replay state: the highest counter accepted per pool (ADR 0002). A
//! reservation holds the write open until its caller has stored the batch,
//! so of two submissions racing on one counter exactly one both stores and
//! records.

use std::path::Path;

use metsuke::envelope::PoolId;
use rusqlite::Connection;
use time::OffsetDateTime;

pub struct CounterStore {
    conn: Connection,
}

#[derive(Debug, thiserror::Error)]
pub enum CounterError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migrate(#[from] metsuke::sqlite::MigrateError),
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
    pub fn commit(self) -> Result<(), CounterError> {
        Ok(self.transaction.commit()?)
    }
}

const MIGRATIONS: &[&str] = &["CREATE TABLE counters (
        pool_id TEXT PRIMARY KEY,
        last_counter INTEGER NOT NULL,
        -- Unix seconds. Part of the ADR-0002 counter state: the operator's
        -- evidence of when a pool last got through.
        last_seen INTEGER NOT NULL
    );"];

impl CounterStore {
    /// Open (creating if absent) and migrate the counter database.
    pub fn open(path: &Path) -> Result<Self, CounterError> {
        let conn = Connection::open(path)?;
        metsuke::sqlite::migrate(&conn, MIGRATIONS)?;
        Ok(CounterStore { conn })
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
    ) -> Result<Reservation<'_>, CounterError> {
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

    /// The highest counter accepted for `pool`, `None` before its first
    /// submission.
    pub fn last_counter(&self, pool: PoolId) -> Result<Option<u64>, CounterError> {
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
