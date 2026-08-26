//! Spool durability tests (ticket metsuke-4zo.4): nothing is lost across
//! restarts or server downtime, and rows leave only through ACK.

use std::time::Duration;

use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig, UncarriableReport};
use metsuke_wire::envelope::Sample;
use proptest::prelude::*;
use time::OffsetDateTime;

/// Wide enough that `outstanding` returns everything spooled: the byte budget
/// is the caller's, and a test about durability is not a test about it.
const WHOLE_SPOOL: u64 = 64 * 1024 * 1024;

/// Nothing here has a second connection to contend with, so a write that has
/// to wait is a bug in the test rather than the lock wait being too short.
const NO_CONTENTION: Duration = Duration::from_secs(1);

fn sample_at(unix_secs: i64) -> Sample {
    Sample {
        sampled_at: OffsetDateTime::from_unix_timestamp(unix_secs).unwrap(),
        block_height: Some(unix_secs as u64),
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}

/// What one row costs against a cap or a budget: its own bytes plus the one
/// the framing spends separating it from the row before it (`spool` says why
/// the column holds that).
fn row_bytes(text: &str) -> u64 {
    text.len() as u64 + 1
}

/// The same for one `sample_at` row, so a cap can be stated in rows a test can
/// count without any test naming the serializer's byte count.
fn sample_bytes() -> u64 {
    row_bytes(&serde_json::to_string(&sample_at(1)).unwrap())
}

fn temp_config(dir: &tempfile::TempDir, max_bytes: u64) -> SpoolConfig {
    SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes,
        busy_timeout: NO_CONTENTION,
    }
}

fn temp_log_config(dir: &tempfile::TempDir, max_bytes: u64) -> LogSpoolConfig {
    LogSpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes,
        busy_timeout: NO_CONTENTION,
    }
}

#[test]
fn undelivered_rows_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    let samples = [sample_at(1), sample_at(2)];
    {
        let mut spool = Spool::open(&config).unwrap();
        for sample in &samples {
            spool.push(sample).unwrap();
        }
    }
    let mut reopened = Spool::open(&config).unwrap();
    let rows = reopened.outstanding(WHOLE_SPOOL).unwrap();
    let offered: Vec<_> = rows.into_iter().map(|row| row.sample).collect();
    assert_eq!(offered, samples);
}

#[test]
fn ack_deletes_only_the_acked_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for secs in 1..=3 {
        spool.push(&sample_at(secs)).unwrap();
    }
    let rows = spool.outstanding(WHOLE_SPOOL).unwrap();
    spool.ack(&[rows[0].id, rows[1].id]).unwrap();
    let remaining = spool.outstanding(WHOLE_SPOOL).unwrap();
    assert_eq!(remaining, vec![rows[2].clone()]);
}

// ADR 0004: an unreachable server drops oldest data, never fills the disk.
#[test]
fn size_cap_drops_oldest_on_push() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 3 * sample_bytes())).unwrap();
    for secs in 1..=5 {
        spool.push(&sample_at(secs)).unwrap();
    }
    let offered: Vec<_> = spool
        .outstanding(WHOLE_SPOOL)
        .unwrap()
        .into_iter()
        .map(|row| row.sample)
        .collect();
    assert_eq!(offered, [sample_at(3), sample_at(4), sample_at(5)]);
}

// The cap is in bytes, not rows: rows of different sizes have to be counted by
// what they cost. Two long node_version strings fill a cap that holds many
// short rows.
#[test]
fn the_cap_counts_bytes_rather_than_rows() {
    let long = Sample {
        node_version: Some("v".repeat(1024)),
        ..sample_at(1)
    };
    let long_bytes = row_bytes(&serde_json::to_string(&long).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 2 * long_bytes)).unwrap();
    for _ in 0..5 {
        spool.push(&long).unwrap();
    }
    assert_eq!(spool.outstanding(WHOLE_SPOOL).unwrap().len(), 2);
}

// Data the agent threw away has an account: silence would leave an operator
// reading a short series with no way to tell it from a quiet node.
#[test]
fn push_reports_the_rows_the_cap_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 2 * sample_bytes())).unwrap();
    assert_eq!(spool.push(&sample_at(1)).unwrap(), 0);
    assert_eq!(spool.push(&sample_at(2)).unwrap(), 0);
    assert_eq!(spool.push(&sample_at(3)).unwrap(), 1);
}

// The batch budget bounds what one upload costs in memory and on the wire, so
// `outstanding` stops at a prefix rather than reading the whole spool.
#[test]
fn outstanding_stops_before_exceeding_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for secs in 1..=5 {
        spool.push(&sample_at(secs)).unwrap();
    }
    let offered: Vec<_> = spool
        .outstanding(2 * sample_bytes())
        .unwrap()
        .into_iter()
        .map(|row| row.sample)
        .collect();
    assert_eq!(offered, [sample_at(1), sample_at(2)]);
}

#[test]
fn outstanding_drops_the_head_row_over_budget_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    spool.push(&sample_at(1)).unwrap();
    spool.push(&sample_at(2)).unwrap();

    assert!(spool.outstanding(1).unwrap().is_empty());
    let report = spool.take_uncarriable_report();
    assert_eq!(report.rows, 1);
    assert_eq!(report.largest_bytes, sample_bytes());
    assert_eq!(
        spool.take_uncarriable_report(),
        UncarriableReport::default()
    );

    let surviving: Vec<_> = spool
        .outstanding(WHOLE_SPOOL)
        .unwrap()
        .into_iter()
        .map(|row| row.sample)
        .collect();
    assert_eq!(surviving, [sample_at(2)]);
}

#[test]
fn an_uncarriable_row_does_not_stall_the_rows_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    let mut lines = LogSpool::open(&temp_log_config(&dir, WHOLE_SPOOL)).unwrap();
    let carriable = "a short line";
    let oversized = "x".repeat(4 * row_bytes(carriable) as usize);
    lines.push(&oversized).unwrap();
    lines.push(carriable).unwrap();

    let offered = spool.outstanding_lines(2 * row_bytes(carriable)).unwrap();

    assert_eq!(
        offered
            .iter()
            .map(|row| row.line.as_str())
            .collect::<Vec<_>>(),
        [carriable]
    );
    assert_eq!(spool.take_uncarriable_report().rows, 1);
}

// A counter value handed out must never be handed out again, even across a
// restart — the server rejects reuse as replay (ADR 0002).
#[test]
fn replay_counter_is_monotonic_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    let before_restart = {
        let mut spool = Spool::open(&config).unwrap();
        let first = spool.next_counter().unwrap();
        let second = spool.next_counter().unwrap();
        assert!(second > first);
        second
    };
    let mut reopened = Spool::open(&config).unwrap();
    assert!(reopened.next_counter().unwrap() > before_restart);
}

// Migrations run on open: a version-0 SQLite file someone else created (here
// a raw empty DB) comes out with the working schema, and its recorded schema
// version is 2 — the two released migrations.
#[test]
fn open_migrates_a_fresh_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    rusqlite::Connection::open(&config.path).unwrap();
    let mut spool = Spool::open(&config).unwrap();
    spool.push(&sample_at(1)).unwrap();
    assert_eq!(spool.outstanding(WHOLE_SPOOL).unwrap().len(), 1);
    let raw = rusqlite::Connection::open(&config.path).unwrap();
    let version: u32 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

// A spool an already-deployed agent wrote holds rows with no recorded size.
// The cap counts bytes, so the migration has to give those rows theirs, or the
// first push after an upgrade evicts a spool it thinks is empty.
#[test]
fn migrating_a_v1_spool_gives_its_rows_their_byte_count() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    {
        let conn = rusqlite::Connection::open(&config.path).unwrap();
        conn.execute_batch(
            "CREATE TABLE samples (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sample TEXT NOT NULL
            );
            CREATE TABLE delivery (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                counter INTEGER NOT NULL
            );
            INSERT INTO delivery (id, counter) VALUES (1, 0);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1u32).unwrap();
        for secs in 1..=3 {
            conn.execute(
                "INSERT INTO samples (sample) VALUES (?1)",
                [serde_json::to_string(&sample_at(secs)).unwrap()],
            )
            .unwrap();
        }
    }
    let mut spool = Spool::open(&config).unwrap();
    assert_eq!(spool.outstanding(WHOLE_SPOOL).unwrap().len(), 3);
    // The budget admits two of the three, which it can only do from the byte
    // counts the migration backfilled.
    assert_eq!(spool.outstanding(2 * sample_bytes()).unwrap().len(), 2);
}

// The trace-line half is written by its own connection and read by the upload
// loop's, against one file (ADR 0004: one durability layer).
#[test]
fn trace_lines_written_by_one_connection_are_read_by_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = LogSpool::open(&temp_log_config(&dir, WHOLE_SPOOL)).unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for line in ["first", "second"] {
        lines.push(line).unwrap();
    }
    let outstanding = spool.outstanding_lines(WHOLE_SPOOL).unwrap();
    assert_eq!(
        outstanding
            .iter()
            .map(|row| row.line.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    spool.ack_lines(&[outstanding[0].id]).unwrap();
    let remaining = spool.outstanding_lines(WHOLE_SPOOL).unwrap();
    assert_eq!(remaining, vec![outstanding[1].clone()]);
}

// A reader does not stop the trace-line stream. The upload loop reads the same
// file the trace thread appends to, and under rollback journalling its read
// lock makes every push wait out the busy timeout and then fail.
#[test]
fn a_reader_holding_the_file_does_not_block_a_trace_line_push() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool.sqlite");
    let mut lines = LogSpool::open(&LogSpoolConfig {
        path: path.clone(),
        max_bytes: WHOLE_SPOOL,
        // Short on purpose: a push that has to wait this out fails here rather
        // than making the test hang.
        busy_timeout: Duration::from_millis(100),
    })
    .unwrap();
    let reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT count(*) FROM log_lines", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();

    lines
        .push("a line written while the upload loop reads")
        .unwrap();
}

// The two caps are independent: a trace stream filling its own does not evict
// a sample, and both live in one file.
#[test]
fn the_trace_line_cap_is_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    spool.push(&sample_at(1)).unwrap();
    let line = |index: u32| format!("{index}: one trace line");
    let mut lines = LogSpool::open(&temp_log_config(&dir, row_bytes(&line(0)))).unwrap();
    for index in 0..5 {
        lines.push(&line(index)).unwrap();
    }
    assert_eq!(spool.outstanding(WHOLE_SPOOL).unwrap().len(), 1);
    assert_eq!(spool.outstanding_lines(WHOLE_SPOOL).unwrap().len(), 1);
}

proptest! {
    // ADR 0004: rows leave only through ACK, and an ACK deletes exactly the
    // acked rows. After random push/partial-ack interleavings, acking every
    // outstanding row leaves zero sample rows — checked through the API and
    // again on the raw file, so a lying `outstanding()` can't hide orphans.
    #[test]
    fn write_ack_delete_leaves_no_orphan_rows(
        batches in prop::collection::vec((1usize..8, any::<prop::sample::Index>()), 1..10),
        cap_rows in 1u64..50,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let config = temp_config(&dir, cap_rows * sample_bytes());
        let mut spool = Spool::open(&config).unwrap();
        let mut pushed = 0i64;
        for (count, ack_pick) in batches {
            for _ in 0..count {
                spool.push(&sample_at(pushed)).unwrap();
                pushed += 1;
            }
            let rows = spool.outstanding(WHOLE_SPOOL).unwrap();
            let acked: Vec<i64> = rows[..ack_pick.index(rows.len() + 1)]
                .iter()
                .map(|row| row.id)
                .collect();
            spool.ack(&acked).unwrap();
            let remaining = spool.outstanding(WHOLE_SPOOL).unwrap();
            prop_assert!(remaining.iter().all(|row| !acked.contains(&row.id)));
        }
        let all: Vec<i64> = spool.outstanding(WHOLE_SPOOL).unwrap().iter().map(|row| row.id).collect();
        spool.ack(&all).unwrap();
        prop_assert!(spool.outstanding(WHOLE_SPOOL).unwrap().is_empty());
        drop(spool);
        let raw = rusqlite::Connection::open(&config.path).unwrap();
        let orphans: i64 = raw
            .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
            .unwrap();
        prop_assert_eq!(orphans, 0);
    }
}

#[test]
fn pushed_sample_is_outstanding() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    let sample = sample_at(1_000);
    spool.push(&sample).unwrap();
    let rows = spool.outstanding(WHOLE_SPOOL).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sample, sample);
}
