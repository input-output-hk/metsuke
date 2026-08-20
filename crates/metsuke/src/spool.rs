//! The agent's only durability layer (ADR 0004): samples, delivery state,
//! and schema migrations in one SQLite file. A sample leaves only on server
//! ACK or as the oldest row past the size cap; everything else is offered
//! again at startup and every upload interval.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::envelope::Sample;

pub struct SpoolConfig {
    pub path: PathBuf,
    /// Newest samples kept when the server is unreachable; oldest beyond
    /// this are dropped on push (ADR 0004: degrade, don't fill the disk).
    pub max_samples: u64,
}

pub struct Spool {
    conn: Connection,
    max_samples: u64,
}

/// A spooled sample with the row id `ack` needs to delete it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpooledSample {
    pub id: i64,
    pub sample: Sample,
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sample row {id} does not deserialize: {source}")]
    Corrupt {
        id: i64,
        #[source]
        source: serde_json::Error,
    },
    #[error("sample does not serialize: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// One entry per released schema version; `user_version` records how many
/// have run, so opening an old DB applies exactly the missing suffix.
const MIGRATIONS: &[&str] = &["CREATE TABLE samples (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        sample TEXT NOT NULL
    );
    CREATE TABLE delivery (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        counter INTEGER NOT NULL
    );
    INSERT INTO delivery (id, counter) VALUES (1, 0);"];

fn migrate(conn: &Connection) -> Result<(), SpoolError> {
    let applied: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (version, migration) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        conn.execute_batch(migration)?;
        conn.pragma_update(None, "user_version", version as u32 + 1)?;
    }
    Ok(())
}

impl Spool {
    /// Open (creating if absent) and migrate the spool database.
    pub fn open(config: &SpoolConfig) -> Result<Self, SpoolError> {
        let conn = Connection::open(&config.path)?;
        migrate(&conn)?;
        Ok(Spool {
            conn,
            max_samples: config.max_samples,
        })
    }

    /// Append a sample, dropping the oldest rows beyond `max_samples`.
    /// One transaction, so a crash never leaves the cap overshot.
    pub fn push(&mut self, sample: &Sample) -> Result<(), SpoolError> {
        let json = serde_json::to_string(sample).map_err(SpoolError::Serialize)?;
        let transaction = self.conn.transaction()?;
        transaction.execute("INSERT INTO samples (sample) VALUES (?1)", [&json])?;
        transaction.execute(
            "DELETE FROM samples WHERE id NOT IN
                (SELECT id FROM samples ORDER BY id DESC LIMIT ?1)",
            // SQLite integers are i64; a cap past i64::MAX can never be
            // reached (rowids are i64), so clamping changes nothing.
            [i64::try_from(self.max_samples).unwrap_or(i64::MAX)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every undelivered sample, oldest first.
    pub fn outstanding(&self) -> Result<Vec<SpooledSample>, SpoolError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, sample FROM samples ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, json) = row?;
            let sample =
                serde_json::from_str(&json).map_err(|source| SpoolError::Corrupt { id, source })?;
            Ok(SpooledSample { id, sample })
        })
        .collect()
    }

    /// The next replay counter value, persisted before it is returned so a
    /// value handed out is never handed out again, across restarts (ADR 0002
    /// needs it monotonic per pool; this spool is one pool's state).
    pub fn next_counter(&mut self) -> Result<u64, SpoolError> {
        let counter: i64 = self.conn.query_row(
            "UPDATE delivery SET counter = counter + 1 WHERE id = 1 RETURNING counter",
            [],
            |row| row.get(0),
        )?;
        Ok(counter as u64)
    }

    /// Delete rows the server ACK'd. One transaction: an ACK is applied
    /// whole or not at all.
    pub fn ack(&mut self, ids: &[i64]) -> Result<(), SpoolError> {
        let transaction = self.conn.transaction()?;
        {
            let mut statement = transaction.prepare("DELETE FROM samples WHERE id = ?1")?;
            for id in ids {
                statement.execute([id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}
