//! The agent's only durability layer (ADR 0004): scrapes, trace lines,
//! delivery state, and schema migrations in one SQLite file. A row leaves on
//! server ACK, as the oldest row past its stream's byte cap, or for being
//! larger than a whole submission on its own (`outstanding_rows`); everything else is
//! offered again at startup and every upload interval.
//!
//! A row is stored as the line it will be on the wire
//! (`envelope::PayloadLine`), stamped on the way in; nothing here reads one back
//! as the schema it came from. What that buys is ADR 0010.
//!
//! Both caps are in bytes rather than rows because a trace line and a scrape
//! are not the same size and a trace stream's rate is not the scrape tick's; a
//! row count bounds neither the file nor the memory a submission costs.

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
    /// to say it is the upload loop and the deletes happen while a submission is
    /// being taken.
    uncarriable_since_report: UncarriableReport,
}

/// What taking a submission deleted since the last report: rows no submission could
/// carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UncarriableReport {
    /// Rows over a whole submission's budget on their own (`outstanding_rows`).
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
    /// The default path is the systemd state directory, so this is what an
    /// operator running the agent any other way meets, and the setting is
    /// what the message has to name.
    #[error(
        "its directory {path} is not there and cannot be created: {source}; set spool_path somewhere this user can write"
    )]
    Directory {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migrate(#[from] metsuke_wire::sqlite::MigrateError),
    /// A row is stamped before it is stored, so a value that has no wire line
    /// is refused here rather than reaching the file.
    #[error("row does not render as a payload line: {0}")]
    Stamp(#[from] SealError),
}

/// One entry per released schema, never rewritten, which is what
/// `sqlite::migrate` counts in `user_version`. The second is appended rather
/// than folded into the first because agents are already running against
/// version 1 files, and those have to gain the table rather than be left
/// without it.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE scrapes (
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
    INSERT INTO delivery (id, counter) VALUES (1, 0);",
    // Seeded from the tables themselves, so an existing spool starts correct.
    // The one scan of each stream this costs is the last one either takes.
    "CREATE TABLE stream_bytes (
        stream TEXT PRIMARY KEY,
        total INTEGER NOT NULL
    );
    INSERT INTO stream_bytes (stream, total)
        SELECT 'scrapes', COALESCE(SUM(bytes), 0) FROM scrapes;
    INSERT INTO stream_bytes (stream, total)
        SELECT 'log_lines', COALESCE(SUM(bytes), 0) FROM log_lines;",
];

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
/// reading a submission would stall the stream for as long as it takes.
fn open_spool(path: &PathBuf, busy_timeout: Duration) -> Result<Connection, SpoolError> {
    // sqlite creates the file and not the directory over it. The systemd
    // shapes get theirs from StateDirectory; a shell or a container run has
    // whatever the operator named in `spool_path`.
    if let Some(directory) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(directory).map_err(|source| SpoolError::Directory {
            path: directory.display().to_string(),
            source,
        })?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(busy_timeout)?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    metsuke_wire::sqlite::migrate(&conn, MIGRATIONS)?;
    Ok(conn)
}

/// What a stream currently holds, as `stream_bytes` records it. Kept in the
/// file rather than in memory because both connections move it: the trace
/// thread appends and evicts, the upload loop acks.
fn total_bytes(conn: &Connection, stream: &Stream) -> Result<i64, SpoolError> {
    Ok(conn.query_row(
        "SELECT total FROM stream_bytes WHERE stream = ?1",
        [stream.table],
        |row| row.get(0),
    )?)
}

fn set_total(conn: &Connection, stream: &Stream, total: i64) -> Result<(), SpoolError> {
    conn.execute(
        "UPDATE stream_bytes SET total = ?2 WHERE stream = ?1",
        rusqlite::params![stream.table, total],
    )?;
    Ok(())
}

fn add_total(conn: &Connection, stream: &Stream, delta: i64) -> Result<(), SpoolError> {
    conn.execute(
        "UPDATE stream_bytes SET total = total + ?2 WHERE stream = ?1",
        rusqlite::params![stream.table, delta],
    )?;
    Ok(())
}

/// Drop the oldest row and answer what it cost, or `None` for an empty table.
/// One row rather than a computed set: shedding exactly what the cap asks for
/// is what keeps the survivors the newest suffix that fits.
fn evict_oldest(conn: &Connection, stream: &Stream) -> Result<Option<i64>, SpoolError> {
    let table = stream.table;
    Ok(conn
        .prepare_cached(&format!(
            "DELETE FROM {table} WHERE id = (SELECT MIN(id) FROM {table}) RETURNING bytes"
        ))?
        .query_row([], |row| row.get(0))
        .optional()?)
}

/// Append one row and evict the oldest beyond `max_bytes`, in one transaction
/// so a crash never leaves the cap overshot. Returns how many rows the cap
/// dropped.
///
/// The running total is read and written rather than summed. Summing meant a
/// scan of the whole table inside the write lock on every push, and a push is
/// one per selected trace line, so the cost of appending grew with what was
/// already spooled until the lock was never free (metsuke-4zo.101).
///
/// The `bytes` column is `PayloadLine::wire_bytes`.
fn push_capped(
    conn: &mut Connection,
    stream: &Stream,
    line: &PayloadLine,
    max_bytes: u64,
) -> Result<u64, SpoolError> {
    let Stream { table, payload } = *stream;
    let bytes = clamp(line.wire_bytes());
    let cap = clamp(max_bytes);
    let transaction = conn.transaction()?;
    transaction
        .prepare_cached(&format!(
            "INSERT INTO {table} ({payload}, bytes) VALUES (?1, ?2)"
        ))?
        .execute(rusqlite::params![line.as_str(), bytes])?;
    let mut total = total_bytes(&transaction, stream)? + bytes;
    let mut dropped = 0u64;
    // Oldest first, so what survives is the newest suffix that fits.
    while total > cap {
        let Some(shed) = evict_oldest(&transaction, stream)? else {
            break;
        };
        total -= shed;
        dropped += 1;
    }
    set_total(&transaction, stream, total)?;
    transaction.commit()?;
    Ok(dropped)
}

/// What a submission may spend on rows. A row costs the `bytes` column and nothing
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
/// A row over the budget on its own cannot be sealed into any submission bounded by
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
    conn: &mut Connection,
    stream: &Stream,
    budget: RowBudget,
) -> Result<Outstanding, SpoolError> {
    let Stream { table, payload } = *stream;
    let ceiling = clamp(budget.max_bytes);
    // Asked as a read first. SQLite takes the write lock for a DELETE whether
    // or not it matches, and the trace-line writer holds that lock, so a
    // leading DELETE made taking a submission fail against a busy stream: the one
    // thing that drains the spool could not run while the spool was filling.
    let oversized: Option<(i64, i64)> = conn
        .query_row(
            &format!(
                "SELECT id, bytes FROM {table}
                 WHERE id = (SELECT MIN(id) FROM {table}) AND bytes > ?1"
            ),
            [ceiling],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let uncarriable = match oversized {
        None => None,
        Some((id, _)) => {
            let transaction = conn.transaction()?;
            // The row was named by a read outside the write lock, so the cap may
            // have evicted it since. Only the bytes the DELETE actually removed
            // come off the total, or it decrements twice for one row.
            let removed: Option<i64> = transaction
                .query_row(
                    &format!("DELETE FROM {table} WHERE id = ?1 RETURNING bytes"),
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(bytes) = removed {
                add_total(&transaction, stream, -bytes)?;
            }
            transaction.commit()?;
            removed
        }
    };
    // Stopped at the first row past the budget rather than filtered on a window
    // sum, which SQLite computes over every row before discarding all but the
    // oldest prefix (metsuke-4zo.102). `id` is the rowid, so this walks the
    // table in order and stops.
    let mut statement = conn.prepare_cached(&format!(
        "SELECT id, {payload}, bytes FROM {table} ORDER BY id"
    ))?;
    let mut cursor = statement.query([])?;
    let mut rows = Vec::new();
    let mut running: i64 = 0;
    while let Some(row) = cursor.next()? {
        running += row.get::<_, i64>(2)?;
        if running > ceiling {
            break;
        }
        rows.push((row.get(0)?, row.get(1)?));
    }
    Ok(Outstanding {
        rows,
        uncarriable_bytes: uncarriable.map(|bytes| bytes as u64),
    })
}

/// Delete a set of rows in one transaction, so what an ACK covers is applied
/// whole or not at all, and the running total moves with it.
fn delete_rows(conn: &mut Connection, stream: &Stream, ids: &[i64]) -> Result<(), SpoolError> {
    let transaction = conn.transaction()?;
    let mut removed: i64 = 0;
    {
        let mut statement = transaction.prepare(&format!(
            "DELETE FROM {} WHERE id = ?1 RETURNING bytes",
            stream.table
        ))?;
        for id in ids {
            // Absent is not an error: only the rows this ack sealed are named,
            // and the cap may have dropped one out from under it.
            if let Some(bytes) = statement
                .query_row([id], |row| row.get::<_, i64>(0))
                .optional()?
            {
                removed += bytes;
            }
        }
    }
    add_total(&transaction, stream, -removed)?;
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

    /// What each stream is holding, off the running total rather than a scan,
    /// so the upload loop can ask after every submission. Acked rows are gone
    /// from it, so this is the remainder.
    pub fn pending_bytes(&self) -> Result<u64, SpoolError> {
        self.holding(&SCRAPES)
    }

    pub fn pending_line_bytes(&self) -> Result<u64, SpoolError> {
        self.holding(&LINES)
    }

    fn holding(&self, stream: &Stream) -> Result<u64, SpoolError> {
        Ok(total_bytes(&self.conn, stream)?.max(0) as u64)
    }

    fn taken(&mut self, stream: &Stream, budget: RowBudget) -> Result<Vec<SpooledRow>, SpoolError> {
        let taken = outstanding_rows(&mut self.conn, stream, budget)?;
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

    /// What every row in this file is stamped with, and therefore what a submission
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
