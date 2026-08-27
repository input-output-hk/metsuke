//! The trace-line thread: follow the node's journal, keep what the selection
//! rules want, spool it. It runs beside the binary's sample and upload ticks
//! rather than inside them, because a trace stream is continuous and a tick is
//! not. Nothing is shared across that boundary: this thread only appends to
//! `log_lines`, and the upload loop is the only reader.

use std::time::Duration;

use metsuke_wire::envelope::TraceLine;
use metsuke_wire::journal::{ERR, WARNING};

use crate::logselect::{SelectConfig, Selection, select};
use crate::logsource::{JournalConfig, JournalSource, LineSource};
use crate::spool::{LogSpool, SpoolError};

/// How many times one line's spool write is retried when SQLite answers busy.
/// The configured busy timeout is the first wait; this bounds how long the
/// stream stalls behind a contended write before the failure counts as the
/// spool being unwritable rather than momentarily locked.
const BUSY_RETRIES: u32 = 3;

/// Follow, select, spool, and start over after `respawn_backoff` whenever the
/// stream ends. `journalctl --follow` outlives the node's own restarts, so an
/// end here means the transport failed rather than that the node stopped.
///
/// Returns only on a spool that cannot be written, which the caller ends the
/// process on: `main` already treats a spool it cannot open as fatal, and a
/// full disk is the same condition found later. Respawning through it would
/// collect nothing and say so only in a line per backoff.
pub fn run(
    journal: JournalConfig,
    selection: SelectConfig,
    respawn_backoff: Duration,
    mut spool: LogSpool,
) -> SpoolError {
    loop {
        match JournalSource::spawn(&journal) {
            Err(error) => eprintln!("{ERR}trace lines not collected: {error}"),
            Ok(mut source) => {
                if let Err(error) = drain(&mut source, &selection, &mut spool) {
                    return error;
                }
                // A journalctl that exits at once — refused the journal, told
                // to follow a unit it cannot resolve — otherwise leaves this
                // loop respawning it forever with nothing in the journal to
                // say no line was ever read.
                eprintln!(
                    "{WARNING}the trace line stream from {} ended; \
                     no lines are collected until it is followed again",
                    journal.journal_unit,
                );
            }
        }
        std::thread::sleep(respawn_backoff);
    }
}

/// Whether a spool failure is the other connection holding the file, which
/// waiting can clear, or the spool being unwritable, which it cannot.
fn is_busy(error: &SpoolError) -> bool {
    matches!(
        error,
        SpoolError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked,
                ..
            },
            _
        ))
    )
}

/// Read until the stream ends, or return the failure that made the spool
/// unwritable. A busy spool is waited out; anything else is the caller's to
/// end the process on.
/// Why a drain stopped. The pipe caller ends the process on it, and only
/// `NodeExited` is a success: a stream that failed collected nothing more, and
/// exiting zero for it would tell systemd the run was done.
#[derive(Debug, PartialEq)]
pub enum DrainEnd {
    /// stdin read zero bytes, so the node closed its output.
    NodeExited,
    /// The source stopped without the node having exited.
    SourceFailed,
}

pub fn drain(
    source: &mut impl LineSource,
    selection: &SelectConfig,
    spool: &mut LogSpool,
) -> Result<DrainEnd, SpoolError> {
    let mut dropped = 0u64;
    let mut reserved = 0u64;
    let mut end = DrainEnd::NodeExited;
    loop {
        let line = match source.next_line() {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                eprintln!("{ERR}trace line stream failed: {error}");
                end = DrainEnd::SourceFailed;
                break;
            }
        };
        let line = match select(selection, &line) {
            Selection::Ship(line) => line,
            Selection::ReservedKey => {
                if reserved == 0 {
                    eprintln!(
                        "{WARNING}the node wrote a line carrying metsuke's own reserved key; \
                         it cannot be shipped and is dropped"
                    );
                }
                reserved += 1;
                continue;
            }
            Selection::Skip => continue,
        };
        match push_waiting_out_contention(spool, &line) {
            Ok(0) => {}
            Ok(count) => {
                // The first one immediately, so an operator learns the cap is
                // biting without waiting for the stream to end; the total when
                // it does, so the loss has a number.
                if dropped == 0 {
                    eprintln!(
                        "{WARNING}the trace-line spool cap is dropping lines; \
                         uploads are not keeping up, or the server has been unreachable"
                    );
                }
                dropped += count;
            }
            Err(error) => {
                report_totals(dropped, reserved);
                return Err(error);
            }
        }
    }
    report_totals(dropped, reserved);
    Ok(end)
}

/// What a drain lost, each with its own remedy: the cap bites when uploads
/// cannot keep up, and a reserved key is the node writing a name metsuke has
/// taken.
fn report_totals(dropped: u64, reserved: u64) {
    if dropped > 0 {
        eprintln!("{WARNING}the trace-line spool cap dropped {dropped} lines in all");
    }
    if reserved > 0 {
        eprintln!("{WARNING}{reserved} node lines carried metsuke's reserved key and were dropped");
    }
}

/// Append one line, retrying while SQLite answers busy. The retry is the whole
/// difference between the two failures: a spool the upload loop happens to hold
/// costs this line a wait, and one that cannot be written at all is returned.
fn push_waiting_out_contention(spool: &mut LogSpool, line: &TraceLine) -> Result<u64, SpoolError> {
    let mut left = BUSY_RETRIES;
    loop {
        match spool.push(line) {
            Err(error) if is_busy(&error) && left > 0 => {
                left -= 1;
                eprintln!("{WARNING}the trace-line spool is busy, retrying: {error}");
            }
            outcome => return outcome,
        }
    }
}
