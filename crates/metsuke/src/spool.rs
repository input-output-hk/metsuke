//! The agent's only durability layer (ADR 0004): samples, trace lines,
//! delivery state, and schema migrations in one SQLite file. A row leaves on
//! server ACK, as the oldest row past its stream's byte cap, or for being
//! larger than a whole batch on its own (`outstanding_rows`); everything else
//! is offered again at startup and every upload interval.
//!
//! Both caps are in bytes rather than rows because a trace line and a sample
//! are not the same size and a trace stream's rate is not the sampler's; a row
//! count bounds neither the file nor the memory a batch costs.

use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use metsuke_wire::envelope::Sample;

pub struct SpoolConfig {
    pub path: PathBuf,
    /// Newest sample bytes kept when the server is unreachable; oldest beyond
    /// this are dropped on push (ADR 0004: degrade, don't fill the disk).
    pub max_bytes: u64,
    /// How long a write waits for the other connection to this file — the
    /// trace-line writer and the upload loop are separate threads.
    pub busy_timeout: Duration,
}

pub struct Spool {
    conn: Connection,
    max_bytes: u64,
    /// Accumulated rather than returned per call, because the caller that has
    /// to say it is the upload loop and the deletes happen while a batch is
    /// being taken.
    uncarriable_since_report: UncarriableReport,
}

/// What the head-row deletes in `outstanding_rows` cost since the last report.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UncarriableReport {
    pub rows: u64,
    /// The largest row dropped, so the operator is told the budget a row has
    /// to clear rather than only that rows were dropped.
    pub largest_bytes: u64,
}

impl UncarriableReport {
    fn record(&mut self, bytes: u64) {
        self.rows += 1;
        self.largest_bytes = self.largest_bytes.max(bytes);
    }
}

/// A spooled sample with the row id `ack` needs to delete it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpooledSample {
    pub id: i64,
    pub sample: Sample,
}

/// A spooled trace line, verbatim as the node emitted it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpooledLine {
    pub id: i64,
    pub line: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migrate(#[from] metsuke_wire::sqlite::MigrateError),
    #[error("sample row {id} does not deserialize: {source}")]
    Corrupt {
        id: i64,
        #[source]
        source: serde_json::Error,
    },
    #[error("sample does not serialize: {0}")]
    Serialize(#[source] serde_json::Error),
}

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE samples (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        sample TEXT NOT NULL
    );
    CREATE TABLE delivery (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        counter INTEGER NOT NULL
    );
    INSERT INTO delivery (id, counter) VALUES (1, 0);",
    "ALTER TABLE samples ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0;
    UPDATE samples SET bytes = length(CAST(sample AS BLOB)) + 1;
    CREATE TABLE log_lines (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        line TEXT NOT NULL,
        bytes INTEGER NOT NULL
    );",
];

/// The two streams, as the schema names them. Interpolated into SQL, so they
/// are `&'static str` from here and never anything a caller supplies.
struct Stream {
    table: &'static str,
    payload: &'static str,
}

const SAMPLES: Stream = Stream {
    table: "samples",
    payload: "sample",
};

const LINES: Stream = Stream {
    table: "log_lines",
    payload: "line",
};

/// Open, migrate, and set the lock wait every connection to this file shares.
///
/// WAL because the file has two writers: under rollback journalling a reader
/// takes a shared lock that blocks the trace-line writer, so the upload loop
/// reading a batch would stall the stream for as long as it takes.
fn open_spool(path: &PathBuf, busy_timeout: Duration) -> Result<Connection, SpoolError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(busy_timeout)?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    metsuke_wire::sqlite::migrate(&conn, MIGRATIONS)?;
    Ok(conn)
}

/// Append one row and evict the oldest beyond `max_bytes`, in one transaction
/// so a crash never leaves the cap overshot. Returns how many rows the cap
/// dropped.
///
/// A row's `bytes` is what it costs in a sealed payload: its own text plus the
/// newline terminating it (`envelope::payload_lines`). The delivery budget sums
/// this column, so what it bounds is what the server decompresses.
fn push_capped(
    conn: &mut Connection,
    stream: &Stream,
    text: &str,
    max_bytes: u64,
) -> Result<u64, SpoolError> {
    let Stream { table, payload } = *stream;
    let transaction = conn.transaction()?;
    transaction.execute(
        &format!("INSERT INTO {table} ({payload}, bytes) VALUES (?1, ?2)"),
        rusqlite::params![text, text.len() as i64 + 1],
    )?;
    let dropped = transaction.execute(
        // Newest first, so what survives is the newest suffix that fits.
        &format!(
            "DELETE FROM {table} WHERE id NOT IN (
                SELECT id FROM (
                    SELECT id, SUM(bytes) OVER (ORDER BY id DESC) AS running FROM {table}
                ) WHERE running <= ?1
            )"
        ),
        [clamp(max_bytes)],
    )?;
    transaction.commit()?;
    Ok(dropped as u64)
}

/// The oldest rows whose bytes fit in `max_bytes`, and what taking them cost.
struct Outstanding {
    rows: Vec<(i64, String)>,
    uncarriable_bytes: Option<u64>,
}

/// Oldest first, up to `max_bytes`.
///
/// A row over `max_bytes` on its own cannot be sealed into any batch bounded by
/// it, so offering it only seals a body the server refuses; and because every
/// later row's running sum starts at its bytes, leaving it at the head stalls
/// the whole stream behind it until the spool's own cap evicts it. Deleting it
/// is the same trade the byte cap makes (ADR 0004), so it is accounted the same
/// way — `Spool::take_uncarriable_report`.
///
/// Only the head, never every row over the budget: a budget under one row's
/// size is a misconfiguration, and the one that deletes by size empties the
/// spool for as long as it stands. One row a call bounds what a wrong budget
/// costs to what a fixed one can still find.
fn outstanding_rows(
    conn: &Connection,
    stream: &Stream,
    max_bytes: u64,
) -> Result<Outstanding, SpoolError> {
    let Stream { table, payload } = *stream;
    let uncarriable: Option<i64> = conn
        .query_row(
            &format!(
                "DELETE FROM {table}
                 WHERE id = (SELECT MIN(id) FROM {table}) AND bytes > ?1
                 RETURNING bytes"
            ),
            [clamp(max_bytes)],
            |row| row.get(0),
        )
        .optional()?;
    let mut statement = conn.prepare(&format!(
        "SELECT id, {payload} FROM (
            SELECT id, {payload}, SUM(bytes) OVER (ORDER BY id) AS running FROM {table}
        ) WHERE running <= ?1
        ORDER BY id"
    ))?;
    let rows = statement.query_map([clamp(max_bytes)], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(Outstanding {
        rows: rows.collect::<Result<Vec<_>, _>>()?,
        uncarriable_bytes: uncarriable.map(|bytes| bytes as u64),
    })
}

/// Delete the rows an ACK covers. One transaction: an ACK is applied whole or
/// not at all.
fn delete_rows(conn: &mut Connection, stream: &Stream, ids: &[i64]) -> Result<(), SpoolError> {
    let transaction = conn.transaction()?;
    {
        let mut statement =
            transaction.prepare(&format!("DELETE FROM {} WHERE id = ?1", stream.table))?;
        for id in ids {
            statement.execute([id])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// SQLite integers are i64. A byte cap past i64::MAX cannot be reached by a
/// file this process can write, so clamping changes nothing.
fn clamp(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

impl Spool {
    /// Open (creating if absent) and migrate the spool database.
    pub fn open(config: &SpoolConfig) -> Result<Self, SpoolError> {
        Ok(Spool {
            conn: open_spool(&config.path, config.busy_timeout)?,
            max_bytes: config.max_bytes,
            uncarriable_since_report: UncarriableReport::default(),
        })
    }

    /// Append a sample. Returns how many rows the cap dropped.
    pub fn push(&mut self, sample: &Sample) -> Result<u64, SpoolError> {
        let json = serde_json::to_string(sample).map_err(SpoolError::Serialize)?;
        push_capped(&mut self.conn, &SAMPLES, &json, self.max_bytes)
    }

    /// Undelivered samples, oldest first, within `max_bytes`. A head row that
    /// alone exceeds it is dropped here (`outstanding_rows`).
    pub fn outstanding(&mut self, max_bytes: u64) -> Result<Vec<SpooledSample>, SpoolError> {
        let taken = outstanding_rows(&self.conn, &SAMPLES, max_bytes)?;
        if let Some(bytes) = taken.uncarriable_bytes {
            self.uncarriable_since_report.record(bytes);
        }
        taken
            .rows
            .into_iter()
            .map(|(id, json)| {
                let sample = serde_json::from_str(&json)
                    .map_err(|source| SpoolError::Corrupt { id, source })?;
                Ok(SpooledSample { id, sample })
            })
            .collect()
    }

    /// The same for trace lines, which are written by the trace-line thread's
    /// own connection (`LogSpool`) and read here because only the upload loop
    /// seals.
    pub fn outstanding_lines(&mut self, max_bytes: u64) -> Result<Vec<SpooledLine>, SpoolError> {
        let taken = outstanding_rows(&self.conn, &LINES, max_bytes)?;
        if let Some(bytes) = taken.uncarriable_bytes {
            self.uncarriable_since_report.record(bytes);
        }
        Ok(taken
            .rows
            .into_iter()
            .map(|(id, line)| SpooledLine { id, line })
            .collect())
    }

    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        std::mem::take(&mut self.uncarriable_since_report)
    }

    /// The next replay counter value, persisted before it is returned so a
    /// value handed out is never handed out again, across restarts (ADR 0002
    /// needs it monotonic per pool; this spool is one pool's state). Both
    /// streams draw from it, so one pool's uploads stay one sequence.
    pub fn next_counter(&mut self) -> Result<u64, SpoolError> {
        let counter: i64 = self.conn.query_row(
            "UPDATE delivery SET counter = counter + 1 WHERE id = 1 RETURNING counter",
            [],
            |row| row.get(0),
        )?;
        Ok(counter as u64)
    }

    pub fn ack(&mut self, ids: &[i64]) -> Result<(), SpoolError> {
        delete_rows(&mut self.conn, &SAMPLES, ids)
    }

    pub fn ack_lines(&mut self, ids: &[i64]) -> Result<(), SpoolError> {
        delete_rows(&mut self.conn, &LINES, ids)
    }
}

pub struct LogSpoolConfig {
    /// The same file `SpoolConfig` names: ADR 0004's durability layer is one
    /// database, not one per producer.
    pub path: PathBuf,
    /// Newest trace-line bytes kept. Its own cap, because the trace stream's
    /// volume has nothing to do with the sampler's cadence.
    pub max_bytes: u64,
    pub busy_timeout: Duration,
}

/// The trace-line writer's half of the spool. Append-only: the upload loop
/// owns reading and acking, so nothing here can delete a line that was sealed.
pub struct LogSpool {
    conn: Connection,
    max_bytes: u64,
}

impl LogSpool {
    pub fn open(config: &LogSpoolConfig) -> Result<Self, SpoolError> {
        Ok(LogSpool {
            conn: open_spool(&config.path, config.busy_timeout)?,
            max_bytes: config.max_bytes,
        })
    }

    /// Append one selected line verbatim. Returns how many rows the cap
    /// dropped.
    pub fn push(&mut self, line: &str) -> Result<u64, SpoolError> {
        push_capped(&mut self.conn, &LINES, line, self.max_bytes)
    }
}
