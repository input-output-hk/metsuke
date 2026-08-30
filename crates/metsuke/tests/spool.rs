//! Spool durability tests (ticket metsuke-4zo.4): nothing is lost across
//! restarts or server downtime, and rows leave only through ACK.

use std::collections::BTreeMap;
use std::time::Duration;

use metsuke::spool::{LogSpool, LogSpoolConfig, RowBudget, Spool, SpoolConfig, UncarriableReport};
use metsuke_wire::envelope::{Metric, PayloadLine, Scrape, TraceLine};
use proptest::prelude::*;

mod support;
use support::{scrape_at, test_provenance, trace_line};

/// Wide enough that `outstanding` returns everything spooled: the byte budget
/// is the caller's, and a test about durability is not a test about it.
const WHOLE_SPOOL: u64 = 64 * 1024 * 1024;

fn budget(max_bytes: u64) -> RowBudget {
    RowBudget { max_bytes }
}

/// Wide enough to offer everything spooled.
fn whole_spool() -> RowBudget {
    budget(WHOLE_SPOOL)
}

/// Nothing here has a second connection to contend with, so a write that has
/// to wait is a bug in the test rather than the lock wait being too short.
const NO_CONTENTION: Duration = Duration::from_secs(1);

/// What one row costs this file, as `spool::push_capped` stores it. For a line
/// the spool wrote that is `PayloadLine::wire_bytes`; stated as text here for
/// the tests that put a row in past `push`.
fn row_bytes(text: &str) -> u64 {
    text.len() as u64 + 1
}

/// A row as the spool stores it: the line it will be on the wire. What a cap or
/// a budget is stated in here.
fn stamped(scrape: &Scrape) -> PayloadLine {
    PayloadLine::scrape(scrape, &test_provenance()).unwrap()
}

fn stamped_line(line: &TraceLine) -> PayloadLine {
    PayloadLine::trace_line(line, &test_provenance()).unwrap()
}

/// One `scrape_at` row's cost, so a cap can be stated in rows a test can count
/// without any test naming the serializer's byte count.
fn scrape_bytes() -> u64 {
    stamped(&scrape_at(1)).wire_bytes()
}

fn line_bytes(line: &TraceLine) -> u64 {
    stamped_line(line).wire_bytes()
}

fn temp_config(dir: &tempfile::TempDir, max_bytes: u64) -> SpoolConfig {
    SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    }
}

fn temp_log_config(dir: &tempfile::TempDir, max_bytes: u64) -> LogSpoolConfig {
    LogSpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    }
}

#[test]
fn undelivered_rows_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    let scrapes = [scrape_at(1), scrape_at(2)];
    {
        let mut spool = Spool::open(&config).unwrap();
        for scrape in &scrapes {
            spool.push(scrape).unwrap();
        }
    }
    let mut reopened = Spool::open(&config).unwrap();
    let rows = reopened.outstanding(whole_spool()).unwrap();
    let offered: Vec<_> = rows.into_iter().map(|row| row.line).collect();
    assert_eq!(offered, scrapes.map(|scrape| stamped(&scrape)));
}

#[test]
fn ack_deletes_only_the_acked_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for secs in 1..=3 {
        spool.push(&scrape_at(secs)).unwrap();
    }
    let rows = spool.outstanding(whole_spool()).unwrap();
    spool.ack(&[rows[0].id, rows[1].id]).unwrap();
    let remaining = spool.outstanding(whole_spool()).unwrap();
    assert_eq!(remaining, vec![rows[2].clone()]);
}

// ADR 0004: an unreachable server drops oldest data, never fills the disk.
#[test]
fn size_cap_drops_oldest_on_push() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 3 * scrape_bytes())).unwrap();
    for secs in 1..=5 {
        spool.push(&scrape_at(secs)).unwrap();
    }
    let offered: Vec<_> = spool
        .outstanding(whole_spool())
        .unwrap()
        .into_iter()
        .map(|row| row.line)
        .collect();
    assert_eq!(
        offered,
        [scrape_at(3), scrape_at(4), scrape_at(5)].map(|scrape| stamped(&scrape))
    );
}

// The cap is in bytes, not rows: rows of different sizes have to be counted by
// what they cost. A node exposing many metrics fills a cap that holds many
// scrapes of a node exposing one.
#[test]
fn the_cap_counts_bytes_rather_than_rows() {
    let mut long = scrape_at(1);
    long.metrics = (0..64)
        .map(|index| Metric {
            name: format!("cardano_node_metrics_metric_{index}_int"),
            labels: BTreeMap::new(),
            value: index.into(),
            declared_type: None,
        })
        .collect();
    let long_bytes = stamped(&long).wire_bytes();
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 2 * long_bytes)).unwrap();
    for _ in 0..5 {
        spool.push(&long).unwrap();
    }
    assert_eq!(spool.outstanding(whole_spool()).unwrap().len(), 2);
}

// Data the agent threw away has an account: silence would leave an operator
// reading a short series with no way to tell it from a quiet node.
#[test]
fn push_reports_the_rows_the_cap_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 2 * scrape_bytes())).unwrap();
    assert_eq!(spool.push(&scrape_at(1)).unwrap(), 0);
    assert_eq!(spool.push(&scrape_at(2)).unwrap(), 0);
    assert_eq!(spool.push(&scrape_at(3)).unwrap(), 1);
}

// The batch budget bounds what one upload costs in memory and on the wire, so
// `outstanding` stops at a prefix rather than reading the whole spool.
#[test]
fn outstanding_stops_before_exceeding_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for secs in 1..=5 {
        spool.push(&scrape_at(secs)).unwrap();
    }
    let offered: Vec<_> = spool
        .outstanding(budget(2 * scrape_bytes()))
        .unwrap()
        .into_iter()
        .map(|row| row.line)
        .collect();
    assert_eq!(
        offered,
        [scrape_at(1), scrape_at(2)].map(|scrape| stamped(&scrape))
    );
}

// What the spool charges a row is what the row costs the wire, to the byte: a
// budget of exactly two rows' `wire_bytes` admits two, and one byte less admits
// one. Two accounts of that number is metsuke-jfb.9; there is one.
#[test]
fn a_row_costs_the_budget_what_it_costs_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    for secs in 1..=5 {
        spool.push(&scrape_at(secs)).unwrap();
    }
    assert_eq!(
        spool.outstanding(budget(2 * scrape_bytes())).unwrap().len(),
        2
    );
    assert_eq!(
        spool
            .outstanding(budget(2 * scrape_bytes() - 1))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn outstanding_drops_the_head_row_over_budget_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    spool.push(&scrape_at(1)).unwrap();
    spool.push(&scrape_at(2)).unwrap();

    assert!(spool.outstanding(budget(1)).unwrap().is_empty());
    let report = spool.take_uncarriable_report();
    assert_eq!(report.oversized, 1);
    // The budget the row had to clear, which is the whole line and its newline:
    // what the operator is asked to raise is measured the same way.
    assert_eq!(report.largest_bytes, scrape_bytes());
    assert_eq!(
        spool.take_uncarriable_report(),
        UncarriableReport::default()
    );

    let surviving: Vec<_> = spool
        .outstanding(whole_spool())
        .unwrap()
        .into_iter()
        .map(|row| row.line)
        .collect();
    assert_eq!(surviving, [stamped(&scrape_at(2))]);
}

#[test]
fn an_uncarriable_row_does_not_stall_the_rows_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    let mut lines = LogSpool::open(&temp_log_config(&dir, WHOLE_SPOOL)).unwrap();
    let carriable = trace_line(r#"{"ns":"Consensus.LeiosKernel"}"#);
    let oversized = trace_line(&format!(
        r#"{{"ns":"{}"}}"#,
        "x".repeat(4 * line_bytes(&carriable) as usize)
    ));
    lines.push(&oversized).unwrap();
    lines.push(&carriable).unwrap();

    let offered = spool
        .outstanding_lines(budget(2 * line_bytes(&carriable)))
        .unwrap();

    assert_eq!(
        offered.into_iter().map(|row| row.line).collect::<Vec<_>>(),
        [stamped_line(&carriable)]
    );
    assert_eq!(spool.take_uncarriable_report().oversized, 1);
}

// A row is stored as the line it will be on the wire, so what fields it holds
// is not the spool's business: a row a later build's `Scrape` would refuse
// still travels.
#[test]
fn a_stored_row_travels_whatever_its_fields_are() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    let mut spool = Spool::open(&config).unwrap();
    // Past `push`, which is the only thing here that knows what a `Scrape` is:
    // this stands for a row some other build of that struct wrote.
    let alien = r#"{"measured_by_another_build":true}"#;
    let file = rusqlite::Connection::open(&config.path).unwrap();
    file.execute(
        "INSERT INTO scrapes (scrape, bytes) VALUES (?1, ?2)",
        rusqlite::params![alien, row_bytes(alien) as i64],
    )
    .unwrap();

    let offered = spool.outstanding(whole_spool()).unwrap();

    assert_eq!(offered.len(), 1);
    assert_eq!(
        spool.take_uncarriable_report(),
        UncarriableReport::default()
    );
}

// A counter value handed out must never be handed out again, even across a
// restart: a gap in one agent's run of it is how a consumer sees a batch the
// archive never got.
#[test]
fn the_counter_is_monotonic_across_restart() {
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
// version is the number of released migrations.
#[test]
fn open_migrates_a_fresh_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, WHOLE_SPOOL);
    rusqlite::Connection::open(&config.path).unwrap();
    let mut spool = Spool::open(&config).unwrap();
    spool.push(&scrape_at(1)).unwrap();
    assert_eq!(spool.outstanding(whole_spool()).unwrap().len(), 1);
    let raw = rusqlite::Connection::open(&config.path).unwrap();
    let version: u32 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

// The trace-line half is written by its own connection and read by the upload
// loop's, against one file (ADR 0004: one durability layer).
#[test]
fn trace_lines_written_by_one_connection_are_read_by_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = LogSpool::open(&temp_log_config(&dir, WHOLE_SPOOL)).unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    let written: Vec<TraceLine> = [r#"{"ns":"first"}"#, r#"{"ns":"second"}"#]
        .into_iter()
        .map(trace_line)
        .collect();
    for line in &written {
        lines.push(line).unwrap();
    }
    let outstanding = spool.outstanding_lines(whole_spool()).unwrap();
    assert_eq!(
        outstanding
            .iter()
            .map(|row| row.line.clone())
            .collect::<Vec<_>>(),
        written.iter().map(stamped_line).collect::<Vec<_>>()
    );
    spool.ack_lines(&[outstanding[0].id]).unwrap();
    let remaining = spool.outstanding_lines(whole_spool()).unwrap();
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
        provenance: test_provenance(),
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
        .push(&trace_line(
            r#"{"ns":"written while the upload loop reads"}"#,
        ))
        .unwrap();
}

// And the other direction, which is the one that wedged an agent in the field:
// the trace thread holds the write lock for as long as its push takes, and
// taking a batch is what drains the spool it is filling. Taking one has to be a
// read, or the busier the stream gets the less the loop that relieves it runs.
#[test]
fn a_writer_holding_the_file_does_not_block_taking_a_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool.sqlite");
    let mut spool = Spool::open(&SpoolConfig {
        path: path.clone(),
        max_bytes: WHOLE_SPOOL,
        // Short on purpose: a take that waits on the lock fails here rather
        // than making the test hang.
        busy_timeout: Duration::from_millis(100),
        provenance: test_provenance(),
    })
    .unwrap();
    let mut lines = LogSpool::open(&temp_log_config(&dir, WHOLE_SPOOL)).unwrap();
    lines.push(&trace_line(r#"{"ns":"one"}"#)).unwrap();
    spool.push(&scrape_at(1)).unwrap();

    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.busy_timeout(Duration::from_millis(100)).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    assert_eq!(spool.outstanding(whole_spool()).unwrap().len(), 1);
    assert_eq!(spool.outstanding_lines(whole_spool()).unwrap().len(), 1);
}

// The two caps are independent: a trace stream filling its own does not evict
// a scrape, and both live in one file.
#[test]
fn the_trace_line_cap_is_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    spool.push(&scrape_at(1)).unwrap();
    let line = |index: u32| trace_line(&format!(r#"{{"ns":"line {index}"}}"#));
    let mut lines = LogSpool::open(&temp_log_config(&dir, line_bytes(&line(0)))).unwrap();
    for index in 0..5 {
        lines.push(&line(index)).unwrap();
    }
    assert_eq!(spool.outstanding(whole_spool()).unwrap().len(), 1);
    assert_eq!(spool.outstanding_lines(whole_spool()).unwrap().len(), 1);
}

proptest! {
    // Each case opens a fresh SQLite file in its own tempdir and drives a
    // random push/ack interleaving through it, so the cases cost file I/O
    // rather than CPU. The regression file replays what has already failed.
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    // ADR 0004: rows leave only through ACK, and an ACK deletes exactly the
    // acked rows. After random push/partial-ack interleavings, acking every
    // outstanding row leaves zero scrape rows, checked through the API and
    // again on the raw file, so a lying `outstanding()` can't hide orphans.
    #[test]
    fn write_ack_delete_leaves_no_orphan_rows(
        batches in prop::collection::vec((1usize..8, any::<prop::sample::Index>()), 1..10),
        cap_rows in 1u64..50,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let config = temp_config(&dir, cap_rows * scrape_bytes());
        let mut spool = Spool::open(&config).unwrap();
        let mut pushed = 0i64;
        for (count, ack_pick) in batches {
            for _ in 0..count {
                spool.push(&scrape_at(pushed)).unwrap();
                pushed += 1;
            }
            let rows = spool.outstanding(whole_spool()).unwrap();
            let acked: Vec<i64> = rows[..ack_pick.index(rows.len() + 1)]
                .iter()
                .map(|row| row.id)
                .collect();
            spool.ack(&acked).unwrap();
            let remaining = spool.outstanding(whole_spool()).unwrap();
            prop_assert!(remaining.iter().all(|row| !acked.contains(&row.id)));
        }
        let all: Vec<i64> = spool.outstanding(whole_spool()).unwrap().iter().map(|row| row.id).collect();
        spool.ack(&all).unwrap();
        prop_assert!(spool.outstanding(whole_spool()).unwrap().is_empty());
        drop(spool);
        let raw = rusqlite::Connection::open(&config.path).unwrap();
        let orphans: i64 = raw
            .query_row("SELECT count(*) FROM scrapes", [], |row| row.get(0))
            .unwrap();
        prop_assert_eq!(orphans, 0);
    }
}

#[test]
fn pushed_scrape_is_outstanding() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, WHOLE_SPOOL)).unwrap();
    let scrape = scrape_at(1_000);
    spool.push(&scrape).unwrap();
    let rows = spool.outstanding(whole_spool()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line, stamped(&scrape));
}
