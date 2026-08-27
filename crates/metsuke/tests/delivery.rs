//! Delivery seam tests (ticket metsuke-4zo.4): spool state in, sealed bytes
//! out, ack shrinks the spool. The batch is checked by `open`ing it with the
//! verifying key.

use std::time::Duration;

use metsuke::delivery::Delivery;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig};
use metsuke_wire::envelope::{
    self, Limits, Payload, PayloadLine, PoolId, Provenance, Sample, SigningKey, TraceLine,
};
use time::OffsetDateTime;

mod support;
use support::{TEST_LIMITS, test_provenance, trace_line};

/// Wide enough that no cap in the spool or the batch fires unless a test asks
/// for one.
const UNBOUNDED: u64 = 64 * 1024 * 1024;

const NO_CONTENTION: Duration = Duration::from_secs(1);

/// The samples an opened batch carries, so a test that is about delivery does
/// not carry the schema check with it.
fn samples_of(envelope: &envelope::Envelope) -> Vec<Sample> {
    envelope.samples().expect("a sample batch carries samples")
}

fn lines_of(envelope: &envelope::Envelope) -> Vec<TraceLine> {
    envelope
        .trace_lines()
        .expect("a trace-line batch carries lines")
}

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

fn temp_delivery(dir: &tempfile::TempDir, key: &SigningKey) -> Delivery {
    delivery_with_batch_cap(dir, key, UNBOUNDED)
}

fn delivery_with_batch_cap(
    dir: &tempfile::TempDir,
    key: &SigningKey,
    batch_max_bytes: u64,
) -> Delivery {
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap();
    Delivery::new(spool, key.clone(), 0, batch_max_bytes)
}

/// The trace-line writer, which is a separate connection in the binary too.
fn temp_log_spool(dir: &tempfile::TempDir) -> LogSpool {
    LogSpool::open(&LogSpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap()
}

// The whole loop contract: what was pushed comes out as a batch the server's
// own call (`open` with the verifying key) accepts, and an ack empties the
// spool.
#[test]
fn pushed_samples_seal_verify_and_ack_drains_the_spool() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    let samples = [sample_at(1), sample_at(2)];
    for sample in &samples {
        delivery.push(sample).unwrap();
    }
    let batch = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(samples_of(&opened), samples);
    assert_eq!(
        opened.provenance.pool_id,
        PoolId::from_cold_key(&key.verifying_key())
    );
    delivery.ack(batch).unwrap();
    assert!(
        delivery
            .take_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// A failed PUT means no ack: the retry offers the same samples again but
// under a fresh counter — a counter value is never handed out twice.
#[test]
fn unacked_batch_is_retaken_with_a_fresh_counter() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    delivery.push(&sample_at(1)).unwrap();
    let open = |batch: &metsuke::delivery::SealedBatch| {
        envelope::open(
            &key.verifying_key(),
            &batch.wire_bytes,
            &batch.signature,
            TEST_LIMITS,
        )
        .unwrap()
    };
    let first = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let retry = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let (first, retry) = (open(&first), open(&retry));
    assert_eq!(samples_of(&first), samples_of(&retry));
    assert!(retry.counter > first.counter);
}

#[test]
fn empty_spool_yields_no_batch() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    assert!(
        delivery
            .take_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
    assert!(
        delivery
            .take_line_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// Trace lines take the same path as samples and come out as a schema v2
// envelope the server's own call opens, with the lines field for field.
#[test]
fn spooled_trace_lines_seal_as_their_own_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    let mut lines = temp_log_spool(&dir);
    let recorded = [
        r#"{"at":"2026-08-25T18:19:38.019429126Z"}"#,
        r#"{"ns":"Consensus.Leios","sev":"Notice"}"#,
    ]
    .map(trace_line);
    for line in &recorded {
        lines.push(line).unwrap();
    }
    let batch = delivery
        .take_line_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(opened.schema_version(), 2);
    assert_eq!(lines_of(&opened), recorded);
    delivery.ack(batch).unwrap();
    assert!(
        delivery
            .take_line_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// Acking a trace-line batch must not touch a spooled sample, and vice versa:
// the two streams share one file and one counter, nothing else.
#[test]
fn acking_one_stream_leaves_the_other_spooled() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    let mut lines = temp_log_spool(&dir);
    delivery.push(&sample_at(1)).unwrap();
    lines.push(&trace_line(r#"{"ns":"one line"}"#)).unwrap();

    let batch = delivery
        .take_line_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    delivery.ack(batch).unwrap();

    let samples = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .expect("the sample is still spooled");
    let opened = envelope::open(
        &key.verifying_key(),
        &samples.wire_bytes,
        &samples.signature,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(samples_of(&opened), [sample_at(1)]);
}

/// An envelope of `payload`'s shape with nothing in it, at the widest counter a
/// spool can hand out: what the batch budget reserves before any row is in it.
fn empty_envelope(key: &SigningKey, payload: Payload) -> envelope::Envelope {
    envelope::Envelope::new(
        Provenance {
            pool_id: PoolId::from_cold_key(&key.verifying_key()),
            ..test_provenance()
        },
        metsuke::AGENT_VERSION.to_string(),
        u64::MAX,
        OffsetDateTime::UNIX_EPOCH,
        payload,
    )
}

/// What that envelope's header frame costs. The batch budget covers it, so a
/// test that wants room for exactly two rows starts here.
fn framing_bytes(key: &SigningKey, payload: Payload) -> u64 {
    let empty = empty_envelope(key, payload);
    (envelope::HEADER_OFFSET + envelope::header_json(&empty).unwrap().len()) as u64
}

/// What one row costs a batch, measured off the line itself
/// (`envelope::PayloadLine::wire_bytes`).
fn sample_row_bytes(sample: &Sample) -> u64 {
    PayloadLine::sample(sample, &test_provenance())
        .unwrap()
        .wire_bytes()
}

fn line_row_bytes(line: &TraceLine) -> u64 {
    PayloadLine::trace_line(line, &test_provenance())
        .unwrap()
        .wire_bytes()
}

/// The payload bytes a batch of `envelope` seals into, which is what
/// `upload_batch_max_bytes` bounds.
fn body_bytes(envelope: &envelope::Envelope) -> u64 {
    envelope::payload_lines(envelope).len() as u64
}

// The batch budget is what keeps one envelope off both the agent's memory and
// the server's body limit; the remainder is taken next time.
#[test]
fn a_batch_stops_at_the_configured_budget_and_the_rest_is_retaken() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    // One row's full cost (`spool::RowBudget`); the framing is spent before any
    // row is.
    let one = sample_row_bytes(&sample_at(1));
    let cap = framing_bytes(&key, Payload::samples(vec![])) + 2 * one;
    let mut delivery = delivery_with_batch_cap(&dir, &key, cap);
    for secs in 1..=5 {
        delivery.push(&sample_at(secs)).unwrap();
    }
    let open = |batch: &metsuke::delivery::SealedBatch| {
        envelope::open(
            &key.verifying_key(),
            &batch.wire_bytes,
            &batch.signature,
            TEST_LIMITS,
        )
        .unwrap()
    };

    let first = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(samples_of(&open(&first)), [sample_at(1), sample_at(2)]);
    delivery.ack(first).unwrap();

    let second = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(samples_of(&open(&second)), [sample_at(3), sample_at(4)]);
}

// metsuke-4zo.96: the budget bounds the whole sealed body, framing and
// separators included, so a batch cannot exceed the number the operator set.
// It wedged before: the batch is deterministic, so the same rejected batch
// retries until its rows are evicted.
#[test]
fn no_batch_seals_a_body_past_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    // Enough of both streams that the budget, not the spool, is what stops the
    // batch; the trace line is a real one, escaping and all.
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","data":{"msg":"\"quoted\""},"sev":"Info"}"#,
    );
    let cap = framing_bytes(&key, Payload::trace_lines(vec![])) + 40 * line.to_line().len() as u64;
    let mut delivery = delivery_with_batch_cap(&dir, &key, cap);
    let mut lines = temp_log_spool(&dir);
    for secs in 1..=200 {
        delivery.push(&sample_at(secs)).unwrap();
        lines.push(&line).unwrap();
    }
    let open = |batch: &metsuke::delivery::SealedBatch| {
        envelope::open(
            &key.verifying_key(),
            &batch.wire_bytes,
            &batch.signature,
            TEST_LIMITS,
        )
        .unwrap()
    };

    let samples = open(&delivery.take_batch(test_now()).unwrap().unwrap());
    let traces = open(&delivery.take_line_batch(test_now()).unwrap().unwrap());

    assert!(!samples_of(&samples).is_empty() && !lines_of(&traces).is_empty());
    assert!(body_bytes(&samples) <= cap, "{}", body_bytes(&samples));
    assert!(body_bytes(&traces) <= cap, "{}", body_bytes(&traces));
}

// The same budget, narrowed to the row that is over it on its own: it cannot
// be sealed into any batch the budget allows, so it goes and the lines behind
// it ship in its place rather than waiting out the spool's own cap.
#[test]
fn a_line_larger_than_the_whole_budget_is_dropped_rather_than_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let carriable = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","sev":"Info"}"#,
    );
    let carriable_bytes = line_row_bytes(&carriable);
    let cap = framing_bytes(&key, Payload::trace_lines(vec![])) + 2 * carriable_bytes;
    let mut delivery = delivery_with_batch_cap(&dir, &key, cap);
    let mut lines = temp_log_spool(&dir);
    lines
        .push(&trace_line(&format!(
            r#"{{"ns":"{}"}}"#,
            "x".repeat(4 * carriable_bytes as usize)
        )))
        .unwrap();
    lines.push(&carriable).unwrap();

    let batch = delivery.take_line_batch(test_now()).unwrap().unwrap();

    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(lines_of(&opened), [carriable]);
    assert!(body_bytes(&opened) <= cap, "{}", body_bytes(&opened));
    assert_eq!(delivery.take_uncarriable_report().oversized, 1);
}

// A budget the framing alone exhausts leaves no room for any row, and every
// row is then over it: the same delete would take the whole stream. It fails
// the tick instead, and the rows are still there for a fixed config.
#[test]
fn a_budget_the_framing_exhausts_fails_the_tick_and_keeps_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","sev":"Info"}"#,
    );
    let mut lines = temp_log_spool(&dir);
    lines.push(&line).unwrap();
    let framing = framing_bytes(&key, Payload::trace_lines(vec![]));

    // `framing_bytes` stamps UNIX_EPOCH, so this budget is the framing exactly
    // and what it leaves for rows is zero rather than negative.
    let attempt =
        delivery_with_batch_cap(&dir, &key, framing).take_line_batch(OffsetDateTime::UNIX_EPOCH);

    match attempt {
        Err(metsuke::delivery::DeliveryError::BudgetBelowFraming { .. }) => {}
        Err(other) => panic!("a budget under the framing reports itself, got {other:?}"),
        Ok(_) => panic!("a budget under the framing seals no batch"),
    }
    let batch = delivery_with_batch_cap(&dir, &key, UNBOUNDED)
        .take_line_batch(test_now())
        .unwrap()
        .unwrap();
    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(lines_of(&opened), [line]);
}

/// What the spool charged the rows of one stream, read off the file rather
/// than recomputed: the `bytes` column `push_capped` wrote.
fn charged_bytes(dir: &tempfile::TempDir, table: &str) -> u64 {
    rusqlite::Connection::open(dir.path().join("spool.sqlite"))
        .unwrap()
        .query_row(
            &format!("SELECT COALESCE(SUM(bytes), 0) FROM {table}"),
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as u64
}

// metsuke-jfb.9: what the spool charged a row is what the server has to inflate
// for it. Measured against `max_decompressed_bytes` itself, the limit the agent's
// `upload_batch_max_bytes` has to stay under: the batch opens at exactly the
// charged total and is refused one byte under it, so a framing change that moves
// the payload's real cost without the `bytes` column fails here.
#[test]
fn the_spool_charges_a_row_what_the_server_inflates_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    let mut lines = temp_log_spool(&dir);
    for secs in 1..=5 {
        delivery.push(&sample_at(secs)).unwrap();
        lines
            .push(&trace_line(&format!(
                r#"{{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.Leios","data":{{"msg":"\"quoted\" {secs}"}}}}"#
            )))
            .unwrap();
    }
    let charged = [
        charged_bytes(&dir, "samples"),
        charged_bytes(&dir, "log_lines"),
    ];
    let batches = [
        delivery.take_batch(test_now()).unwrap().unwrap(),
        delivery.take_line_batch(test_now()).unwrap().unwrap(),
    ];

    for (charged, batch) in charged.into_iter().zip(batches) {
        let opened = |max_decompressed_bytes| {
            envelope::open(
                &key.verifying_key(),
                &batch.wire_bytes,
                &batch.signature,
                Limits {
                    max_header_bytes: TEST_LIMITS.max_header_bytes,
                    max_decompressed_bytes,
                },
            )
        };
        opened(charged).expect("the payload fits in exactly what its rows were charged");
        match opened(charged - 1) {
            Err(envelope::OpenError::TooLarge { .. }) => {}
            other => panic!("the payload is smaller than its rows were charged: {other:?}"),
        }
    }
}

/// A timestamp with the subsecond digits a real batch carries: the header line
/// is longer for them, and the framing reserve has to cover that.
fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(1_780_000_000_123_456_789).unwrap()
}
