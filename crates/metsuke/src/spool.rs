//! The agent's only durability layer (ADR 0004): scrapes, trace lines,
//! delivery state, and schema migrations in one SQLite file. A row leaves on
//! server ACK, as the oldest row past its stream's byte cap, or for being
//! larger than a whole batch on its own (`outstanding_rows`); everything else is
//! offered again at startup and every upload interval.
//!
//! A row is stored as the line it will be on the wire
//! (`envelope::PayloadLine`), stamped on the way in; nothing here reads one back
//! as the schema it came from. What that buys is ADR 0010.
//!
//! Both caps are in bytes rather than rows because a trace line and a scrape
//! are not the same size and a trace stream's rate is not the scrape tick's; a
//! row count bounds neither the file nor the memory a batch costs.

use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use metsuke_wire::envelope::{PayloadLine, Provenance, Scrape, SealError, TraceLine};

pub struct SpoolConfig {
    pub path: PathBuf,
    /// Newest scrape bytes kept when the server is unreachable; oldest beyond
    /// this are dropped on push (ADR 0004: degrade, don't fill the disk).
    pub max_bytes: u64,
    /// How long a write waits for the other connection to this file. The
    /// trace-line writer and the upload loop are separate threads.
    pub busy_timeout: Duration,
    /// What every row this spool stores is stamped with (`Spool::provenance`).
    pub provenance: Provenance,
}

pub struct Spool {
    conn: Connection,
    max_bytes: u64,
    provenance: Provenance,
    /// Accumulated rather than returned per call, because the caller that has
    /// to say it is the upload loop and the deletes happen while a batch is
    /// being taken.
    uncarriable_since_report: UncarriableReport,
}

/// What taking a batch deleted since the last report: rows no batch could
/// carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UncarriableReport {
    /// Rows over a whole batch's budget on their own (`outstanding_rows`).
    pub oversized: u64,
    /// The largest row dropped, so the operator is told the budget a row has
    /// to clear rather than only that rows were dropped.
    pub largest_bytes: u64,
}

impl UncarriableReport {
    fn record_oversized(&mut self, bytes: u64) {
        self.oversized += 1;
        self.largest_bytes = self.largest_bytes.max(bytes);
    }
}

/// A spooled line with the row id `ack` needs to delete it. One type for both
/// streams: a stored row is a wire line whichever schema it belongs to, and
/// which stream it came from is what the caller asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct SpooledRow {
    pub id: i64,
    pub line: PayloadLine,
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migrate(#[from] metsuke_wire::sqlite::MigrateError),
    /// A row is stamped before it is stored, so a value that has no wire line
    /// is refused here rather than reaching the file.
    #[error("row does not render as a payload line: {0}")]
    Stamp(#[from] SealError),
}

/// One entry, and this is the release it ships with: a spool file only exists
/// where this build has run, and no build has been released, so every state an
/// earlier entry could have migrated from is one nothing can have written. Each
/// entry added after v1 ships is real and never rewritten. That is what
/// `sqlite::migrate` counts in `user_version`.
const MIGRATIONS: &[&str] = &["CREATE TABLE scrapes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        scrape TEXT NOT NULL,
        bytes INTEGER NOT NULL
    );
    CREATE TABLE log_lines (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        line TEXT NOT NULL,
        bytes INTEGER NOT NULL
    );
    CREATE TABLE delivery (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        counter INTEGER NOT NULL
    );
    INSERT INTO delivery (id, counter) VALUES (1, 0);"];

/// The two streams, as the schema names them. Interpolated into SQL, so they
/// are `&'static str` from here and never anything a caller supplies.
struct Stream {
    table: &'static str,
    payload: &'static str,
}

const SCRAPES: Stream = Stream {
    table: "scrapes",
    payload: "scrape",
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
/// The `bytes` column is `PayloadLine::wire_bytes`.
fn push_capped(
    conn: &mut Connection,
    stream: &Stream,
    line: &PayloadLine,
    max_bytes: u64,
) -> Result<u64, SpoolError> {
    let Stream { table, payload } = *stream;
    let transaction = conn.transaction()?;
    transaction
        .prepare_cached(&format!(
            "INSERT INTO {table} ({payload}, bytes) VALUES (?1, ?2)"
        ))?
        .execute(rusqlite::params![line.as_str(), clamp(line.wire_bytes())])?;
    let total: i64 = transaction
        .prepare_cached(&format!("SELECT COALESCE(SUM(bytes), 0) FROM {table}"))?
        .query_row([], |row| row.get(0))?;
    let dropped = if total > clamp(max_bytes) {
        transaction
            .prepare_cached(
                // Newest first, so what survives is the newest suffix that fits.
                &format!(
                    "DELETE FROM {table} WHERE id NOT IN (
                        SELECT id FROM (
                            SELECT id, SUM(bytes) OVER (ORDER BY id DESC) AS running FROM {table}
                        ) WHERE running <= ?1
                    )"
                ),
            )?
            .execute([clamp(max_bytes)])?
    } else {
        0
    };
    transaction.commit()?;
    Ok(dropped as u64)
}

/// What a batch may spend on rows. A row costs the `bytes` column and nothing
/// beside it (`envelope::PayloadLine::wire_bytes`).
#[derive(Debug, Clone, Copy)]
pub struct RowBudget {
    pub max_bytes: u64,
}

/// The oldest rows whose cost fits in the budget, and what taking them cost.
struct Outstanding {
    rows: Vec<(i64, String)>,
    uncarriable_bytes: Option<u64>,
}

/// Oldest first, up to `budget.max_bytes`. A reader unless there is actually an
/// oversized head row to drop, which WAL never blocks.
///
/// A row over the budget on its own cannot be sealed into any batch bounded by
/// it, so offering it only seals a body the server refuses; and because every
/// later row's running sum starts at its bytes, leaving it at the head stalls
/// the whole stream behind it until the spool's own cap evicts it. Deleting it
/// is the same trade the byte cap makes (ADR 0004), so it is accounted the same
/// way, in `Spool::take_uncarriable_report`.
///
/// Only the head, never every row over the budget: a budget under one row's
/// size is a misconfiguration, and the one that deletes by size empties the
/// spool for as long as it stands. One row a call bounds what a wrong budget
/// costs to what a fixed one can still find.
fn outstanding_rows(
    conn: &Connection,
    stream: &Stream,
    budget: RowBudget,
) -> Result<Outstanding, SpoolError> {
    let Stream { table, payload } = *stream;
    let params = [clamp(budget.max_bytes)];
    // Asked as a read first. SQLite takes the write lock for a DELETE whether
    // or not it matches, and the trace-line writer holds that lock, so a
    // leading DELETE made taking a batch fail against a busy stream: the one
    // thing that drains the spool could not run while the spool was filling.
    let oversized: Option<(i64, i64)> = conn
        .query_row(
            &format!(
                "SELECT id, bytes FROM {table}
                 WHERE id = (SELECT MIN(id) FROM {table}) AND bytes > ?1"
            ),
            params,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let uncarriable = match oversized {
        None => None,
        Some((id, bytes)) => {
            conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
            Some(bytes)
        }
    };
    let mut statement = conn.prepare(&format!(
        "SELECT id, {payload} FROM (
            SELECT id, {payload}, SUM(bytes) OVER (ORDER BY id) AS running FROM {table}
        ) WHERE running <= ?1
        ORDER BY id"
    ))?;
    let rows = statement.query_map(params, |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(Outstanding {
        rows: rows.collect::<Result<Vec<_>, _>>()?,
        uncarriable_bytes: uncarriable.map(|bytes| bytes as u64),
    })
}

/// Delete a set of rows in one transaction, so what an ACK covers is applied
/// whole or not at all.
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
            provenance: config.provenance.clone(),
            uncarriable_since_report: UncarriableReport::default(),
        })
    }

    /// Stamp a scrape and append it. Returns how many rows the cap dropped.
    pub fn push(&mut self, scrape: &Scrape) -> Result<u64, SpoolError> {
        let line = PayloadLine::scrape(scrape, &self.provenance)?;
        push_capped(&mut self.conn, &SCRAPES, &line, self.max_bytes)
    }

    /// Undelivered scrape lines, oldest first, within the budget. A head row
    /// that alone exceeds it is dropped here (`outstanding_rows`).
    pub fn outstanding(&mut self, budget: RowBudget) -> Result<Vec<SpooledRow>, SpoolError> {
        self.taken(&SCRAPES, budget)
    }

    /// The same for trace lines, which are written by the trace-line thread's
    /// own connection (`LogSpool`) and read here because only the upload loop
    /// seals.
    pub fn outstanding_lines(&mut self, budget: RowBudget) -> Result<Vec<SpooledRow>, SpoolError> {
        self.taken(&LINES, budget)
    }

    fn taken(&mut self, stream: &Stream, budget: RowBudget) -> Result<Vec<SpooledRow>, SpoolError> {
        let taken = outstanding_rows(&self.conn, stream, budget)?;
        if let Some(bytes) = taken.uncarriable_bytes {
            self.uncarriable_since_report.record_oversized(bytes);
        }
        Ok(taken
            .rows
            .into_iter()
            .map(|(id, text)| SpooledRow {
                id,
                line: PayloadLine::spooled(text),
            })
            .collect())
    }

    /// What every row in this file is stamped with, and therefore what a batch
    /// drawn from it has to name in its header (`delivery::Delivery::envelope`).
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        std::mem::take(&mut self.uncarriable_since_report)
    }

    /// The next counter (`envelope::Envelope::counter`), persisted
    /// before it is returned so a value handed out is never handed out again,
    /// across restarts. Both streams draw from it, so one agent's uploads share
    /// one counter.
    pub fn next_counter(&mut self) -> Result<u64, SpoolError> {
        let counter: i64 = self.conn.query_row(
            "UPDATE delivery SET counter = counter + 1 WHERE id = 1 RETURNING counter",
            [],
            |row| row.get(0),
        )?;
        Ok(counter as u64)
    }

    pub fn ack(&mut self, ids: &[i64]) -> Result<(), SpoolError> {
        delete_rows(&mut self.conn, &SCRAPES, ids)
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
    /// volume has nothing to do with the scrape tick's cadence.
    pub max_bytes: u64,
    pub busy_timeout: Duration,
    /// The same stamp `SpoolConfig` carries: both writers store wire lines, and
    /// which of them wrote a row is not something a reader has to know.
    pub provenance: Provenance,
}

/// The trace-line writer's half of the spool. Append-only: the upload loop
/// owns reading and acking, so nothing here can delete a line that was sealed.
pub struct LogSpool {
    conn: Connection,
    max_bytes: u64,
    provenance: Provenance,
}

impl LogSpool {
    pub fn open(config: &LogSpoolConfig) -> Result<Self, SpoolError> {
        Ok(LogSpool {
            conn: open_spool(&config.path, config.busy_timeout)?,
            max_bytes: config.max_bytes,
            provenance: config.provenance.clone(),
        })
    }

    /// Stamp one selected line and append it. Returns how many rows the cap
    /// dropped.
    pub fn push(&mut self, line: &TraceLine) -> Result<u64, SpoolError> {
        let line = PayloadLine::trace_line(line, &self.provenance)?;
        push_capped(&mut self.conn, &LINES, &line, self.max_bytes)
    }
}
