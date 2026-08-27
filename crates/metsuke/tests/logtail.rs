//! The agent-side trace path end to end, on the recorded stream: a journalctl
//! stand-in replaying it, the drain that selects and spools, and the sealed
//! batch opened back. What a rule keeps is tests/logselect.rs; this is that the
//! four parts hand the same bytes along.

use std::time::Duration;

use metsuke::delivery::Delivery;
use metsuke::logselect::{Selection, select};
use metsuke::logtail;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig};
use metsuke_wire::envelope::{self, SigningKey, TraceLine};
use time::OffsetDateTime;

mod support;
use support::{
    TEST_LIMITS, following, recording, replaying_journalctl, shipped_rules, test_provenance,
};

const LEIOS_RECORDING: &str = "leios-node-traces.log";
const LEIOS_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces.log");

/// Wide enough that no spool or batch cap fires here.
const UNBOUNDED: u64 = 64 * 1024 * 1024;

const NO_CONTENTION: Duration = Duration::from_secs(1);

// A whole journalctl's worth of a real node's traces, through the thread body
// the binary runs, out as the envelope the server opens: every selected line
// and nothing else, in the order the node wrote them and field for field.
#[test]
fn the_recorded_stream_reaches_a_sealed_batch_as_the_lines_the_rules_selected() {
    let dir = tempfile::tempdir().unwrap();
    let rules = shipped_rules();
    let mut source = following(replaying_journalctl(&dir, &recording(LEIOS_RECORDING)));
    let mut lines = LogSpool::open(&LogSpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap();

    logtail::drain(&mut source, &rules, &mut lines).unwrap();

    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = Delivery::new(
        Spool::open(&SpoolConfig {
            path: dir.path().join("spool.sqlite"),
            max_bytes: UNBOUNDED,
            busy_timeout: NO_CONTENTION,
            provenance: test_provenance(),
        })
        .unwrap(),
        key.clone(),
        0,
        UNBOUNDED,
    );
    let batch = delivery
        .take_line_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .expect("the recording holds lines the shipped rules select");
    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_LIMITS,
    )
    .unwrap();
    let lines = opened
        .trace_lines()
        .expect("a trace-line batch carries lines");

    let selected: Vec<TraceLine> = LEIOS_WINDOW
        .lines()
        .filter_map(|line| match select(&rules, line) {
            Selection::Ship(line) => Some(line),
            _ => None,
        })
        .collect();
    assert!(!selected.is_empty());
    assert_eq!(lines, selected);
}

// A spool that cannot be written is not a stream that ended: the drain hands
// the failure back so the caller can end the process, rather than respawning
// journalctl over a spool nothing will ever reach.
#[test]
fn a_spool_that_refuses_a_write_ends_the_drain_with_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spool.sqlite");
    let mut lines = LogSpool::open(&LogSpoolConfig {
        path: path.clone(),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap();
    // Not contention, which waiting clears: the table the writes go to is gone
    // for good, which is what a spool nothing can be written to looks like
    // from inside the drain.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE log_lines")
        .unwrap();
    let mut source = following(replaying_journalctl(&dir, &recording(LEIOS_RECORDING)));

    let err = logtail::drain(&mut source, &shipped_rules(), &mut lines).unwrap_err();

    assert!(
        err.to_string().contains("log_lines"),
        "the failure must name what could not be written, got: {err}"
    );
}
