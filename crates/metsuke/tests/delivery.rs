//! Delivery seam tests (ticket metsuke-4zo.4): spool state in, sealed bytes
//! out, ack shrinks the spool. The submission is checked by `open`ing it with the
//! verifying key.

use std::time::Duration;

use metsuke::delivery::Delivery;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig};
use metsuke_wire::envelope::{self, Limits, Payload, PayloadLine, Scrape, TraceLine};
use time::OffsetDateTime;

mod support;
use support::{
    TEST_LIMITS, scrape_at, test_pool_id, test_provenance, test_submission_key, trace_line,
};

/// Wide enough that no cap in the spool or the submission fires unless a test asks
/// for one.
const UNBOUNDED: u64 = 64 * 1024 * 1024;

const NO_CONTENTION: Duration = Duration::from_secs(1);

/// The scrapes an opened submission carries, so a test that is about delivery does
/// not carry the schema check with it.
fn scrapes_of(envelope: &envelope::Envelope) -> Vec<Scrape> {
    envelope
        .scrapes()
        .expect("a scrape submission carries scrapes")
}

fn lines_of(envelope: &envelope::Envelope) -> Vec<TraceLine> {
    envelope
        .trace_lines()
        .expect("a trace-line submission carries lines")
}

fn temp_delivery(dir: &tempfile::TempDir) -> Delivery {
    delivery_with_submission_cap(dir, UNBOUNDED)
}

fn delivery_with_submission_cap(dir: &tempfile::TempDir, batch_max_bytes: u64) -> Delivery {
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap();
    Delivery::new(spool, test_submission_key(), 0, batch_max_bytes)
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

// What the upload loop drains on: whether the budget cut a batch off with
// rows still spooled, or it took the stream to the end. Asked of the rows and
// not of the bytes, so a header whose length follows the clock cannot decide
// it differently from one run to the next.
#[test]
fn a_submission_says_whether_the_budget_cut_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let now = OffsetDateTime::UNIX_EPOCH;
    let two_scrapes = spent_on_rows(&[scrape_at(1), scrape_at(2)]);
    let mut delivery = delivery_with_submission_cap(&dir, two_scrapes);
    for at in 1..=3 {
        delivery.push(&scrape_at(at)).unwrap();
    }

    let cut_off = delivery.take_submission(now).unwrap().unwrap();
    assert_eq!(cut_off.lines(), 2, "the budget holds two");
    assert!(cut_off.more_waits(), "a third scrape is still spooled");
    delivery.ack(cut_off).unwrap();

    let last = delivery.take_submission(now).unwrap().unwrap();
    assert_eq!(last.lines(), 1);
    assert!(
        !last.more_waits(),
        "the stream ran out, so nothing waits behind it"
    );
}

/// A submission cap that holds `rows` and no more: the framing, which is
/// spent before any row, plus what those rows cost the spool, plus half a row
/// of slack. The slack is what makes it a count rather than a coin flip: the
/// header carries a timestamp whose subsecond digits vary per run, so a cap
/// measured to the byte sometimes holds one row fewer.
fn spent_on_rows(rows: &[Scrape]) -> u64 {
    let empty = envelope::Envelope::new(
        test_provenance(),
        metsuke::AGENT_VERSION.to_string(),
        u64::MAX,
        OffsetDateTime::UNIX_EPOCH,
        Payload::scrapes(vec![]),
    );
    let framing = (envelope::HEADER_OFFSET + envelope::header_json(&empty).unwrap().len()) as u64;
    let row = |scrape: &Scrape| {
        PayloadLine::scrape(scrape, &test_provenance())
            .unwrap()
            .wire_bytes()
    };
    let slack = rows.first().map(row).unwrap_or_default() / 2;
    framing + rows.iter().map(row).sum::<u64>() + slack
}

// The whole loop contract: what was pushed comes out as a submission the server's
// own call (`open` with the verifying key) accepts, and an ack empties the
// spool.
#[test]
fn pushed_scrapes_seal_verify_and_ack_drains_the_spool() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    let scrapes = [scrape_at(1), scrape_at(2)];
    for scrape in &scrapes {
        delivery.push(scrape).unwrap();
    }
    let submission = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let opened =
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap();
    assert_eq!(scrapes_of(&opened), scrapes);
    assert_eq!(opened.provenance.pool_id, test_pool_id());
    delivery.ack(submission).unwrap();
    assert!(
        delivery
            .take_submission(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// A failed PUT means no ack: the retry offers the same scrapes again but
// under a fresh counter. A counter value is never handed out twice.
#[test]
fn unacked_submission_is_retaken_with_a_fresh_counter() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    delivery.push(&scrape_at(1)).unwrap();
    let open = |submission: &metsuke::delivery::SealedSubmission| {
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap()
    };
    let first = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let retry = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    // The digest is what a reader has to match the two by: everything else
    // about the retry differs, including the bytes and their length, because
    // the header carries a fresh counter and a later timestamp.
    assert_eq!(first.payload_digest, retry.payload_digest);
    assert_ne!(first.wire_bytes, retry.wire_bytes);

    let (first, retry) = (open(&first), open(&retry));
    assert_eq!(scrapes_of(&first), scrapes_of(&retry));
    assert!(retry.counter > first.counter);
}

// And it has to be the payload's, not the attempt's: two submissions carrying
// different rows must not answer the same, or matching a refusal to a retry
// would confirm rows that never went.
#[test]
fn submissions_of_different_rows_carry_different_digests() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    delivery.push(&scrape_at(1)).unwrap();
    let first = delivery.take_submission(test_now()).unwrap().unwrap();
    let first_digest = first.payload_digest.clone();
    delivery.ack(first).unwrap();

    delivery.push(&scrape_at(2)).unwrap();
    let second = delivery.take_submission(test_now()).unwrap().unwrap();
    assert_ne!(first_digest, second.payload_digest);
}

// The archive is where a consumer checks the claim, so the digest has to be
// over what a stored object decompresses to and nothing else.
#[test]
fn the_digest_covers_exactly_what_zstd_hands_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    for secs in 1..=3 {
        delivery.push(&scrape_at(secs)).unwrap();
    }
    let sealed = delivery.take_submission(test_now()).unwrap().unwrap();
    let opened = envelope::open(&sealed.attestation, &sealed.wire_bytes, TEST_LIMITS).unwrap();
    assert_eq!(sealed.payload_digest, envelope::payload_digest(&opened));
}

#[test]
fn empty_spool_yields_no_submission() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    assert!(
        delivery
            .take_submission(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
    assert!(
        delivery
            .take_line_submission(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// Trace lines take the same path as scrapes and come out as a schema v2
// envelope the server's own call opens, with the lines field for field.
#[test]
fn spooled_trace_lines_seal_as_their_own_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    let mut lines = temp_log_spool(&dir);
    let recorded = [
        r#"{"at":"2026-08-25T18:19:38.019429126Z"}"#,
        r#"{"ns":"Consensus.LeiosKernel","sev":"Notice"}"#,
    ]
    .map(trace_line);
    for line in &recorded {
        lines.push(line).unwrap();
    }
    let submission = delivery
        .take_line_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let opened =
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap();
    assert_eq!(opened.schema_version(), 2);
    assert_eq!(lines_of(&opened), recorded);
    delivery.ack(submission).unwrap();
    assert!(
        delivery
            .take_line_submission(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// What the journal says a submission was. The header's counter, so a log line
// and an archived object name the same one; the row count and what to call
// those rows, so an agent collecting traces does not report both of a tick's
// submissions as the same thing.
#[test]
fn a_sealed_submission_states_what_the_journal_names_it_by() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    let mut lines = temp_log_spool(&dir);
    for scrape in [scrape_at(1), scrape_at(2)] {
        delivery.push(&scrape).unwrap();
    }
    lines.push(&trace_line(r#"{"ns":"one line"}"#)).unwrap();

    let scrapes = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(scrapes.lines(), 2);
    assert_eq!(scrapes.carried(), "scrapes");

    let traces = delivery
        .take_line_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(traces.lines(), 1);
    assert_eq!(
        traces.carried(),
        "trace line",
        "the word agrees with the count"
    );

    // One counter across both streams, and the field carries what the header
    // was stamped with rather than a second count of its own.
    assert_eq!(traces.counter, scrapes.counter + 1);
    let opened = envelope::open(&traces.attestation, &traces.wire_bytes, TEST_LIMITS).unwrap();
    assert_eq!(opened.counter, traces.counter);
}

// Acking a trace-line submission must not touch a spooled scrape, and vice versa:
// the two streams share one file and one counter, nothing else.
#[test]
fn acking_one_stream_leaves_the_other_spooled() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    let mut lines = temp_log_spool(&dir);
    delivery.push(&scrape_at(1)).unwrap();
    lines.push(&trace_line(r#"{"ns":"one line"}"#)).unwrap();

    let submission = delivery
        .take_line_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    delivery.ack(submission).unwrap();

    let scrapes = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .expect("the scrape is still spooled");
    let opened = envelope::open(&scrapes.attestation, &scrapes.wire_bytes, TEST_LIMITS).unwrap();
    assert_eq!(scrapes_of(&opened), [scrape_at(1)]);
}

/// An envelope of `payload`'s shape with nothing in it, at the widest counter a
/// spool can hand out: what the submission budget reserves before any row is in it.
fn empty_envelope(payload: Payload) -> envelope::Envelope {
    envelope::Envelope::new(
        test_provenance(),
        metsuke::AGENT_VERSION.to_string(),
        u64::MAX,
        OffsetDateTime::UNIX_EPOCH,
        payload,
    )
}

/// What that envelope's header frame costs. The submission budget covers it, so a
/// test that wants room for exactly two rows starts here.
fn framing_bytes(payload: Payload) -> u64 {
    let empty = empty_envelope(payload);
    (envelope::HEADER_OFFSET + envelope::header_json(&empty).unwrap().len()) as u64
}

/// What one row costs a submission, measured off the line itself
/// (`envelope::PayloadLine::wire_bytes`).
fn scrape_row_bytes(scrape: &Scrape) -> u64 {
    PayloadLine::scrape(scrape, &test_provenance())
        .unwrap()
        .wire_bytes()
}

fn line_row_bytes(line: &TraceLine) -> u64 {
    PayloadLine::trace_line(line, &test_provenance())
        .unwrap()
        .wire_bytes()
}

/// The payload bytes a submission of `envelope` seals into, which is what
/// `upload_batch_max_bytes` bounds.
fn body_bytes(envelope: &envelope::Envelope) -> u64 {
    envelope::payload_lines(envelope).len() as u64
}

// The submission budget is what keeps one envelope off both the agent's memory and
// the server's body limit; the remainder is taken next time.
#[test]
fn a_submission_stops_at_the_configured_budget_and_the_rest_is_retaken() {
    let dir = tempfile::tempdir().unwrap();
    // One row's full cost (`spool::RowBudget`); the framing is spent before any
    // row is.
    let one = scrape_row_bytes(&scrape_at(1));
    let cap = framing_bytes(Payload::scrapes(vec![])) + 2 * one;
    let mut delivery = delivery_with_submission_cap(&dir, cap);
    for secs in 1..=5 {
        delivery.push(&scrape_at(secs)).unwrap();
    }
    let open = |submission: &metsuke::delivery::SealedSubmission| {
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap()
    };

    let first = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(scrapes_of(&open(&first)), [scrape_at(1), scrape_at(2)]);
    delivery.ack(first).unwrap();

    let second = delivery
        .take_submission(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    assert_eq!(scrapes_of(&open(&second)), [scrape_at(3), scrape_at(4)]);
}

// metsuke-4zo.96: the budget bounds the whole sealed body, framing and
// separators included, so a submission cannot exceed the number the operator set.
// It wedged before: the submission is deterministic, so the same rejected submission
// retries until its rows are evicted.
#[test]
fn no_submission_seals_a_body_past_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    // Enough of both streams that the budget, not the spool, is what stops the
    // submission; the trace line is a real one, escaping and all.
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","data":{"msg":"\"quoted\""},"sev":"Info"}"#,
    );
    let cap = framing_bytes(Payload::trace_lines(vec![])) + 40 * line.to_line().len() as u64;
    let mut delivery = delivery_with_submission_cap(&dir, cap);
    let mut lines = temp_log_spool(&dir);
    for secs in 1..=200 {
        delivery.push(&scrape_at(secs)).unwrap();
        lines.push(&line).unwrap();
    }
    let open = |submission: &metsuke::delivery::SealedSubmission| {
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap()
    };

    let scrapes = open(&delivery.take_submission(test_now()).unwrap().unwrap());
    let traces = open(&delivery.take_line_submission(test_now()).unwrap().unwrap());

    assert!(!scrapes_of(&scrapes).is_empty() && !lines_of(&traces).is_empty());
    assert!(body_bytes(&scrapes) <= cap, "{}", body_bytes(&scrapes));
    assert!(body_bytes(&traces) <= cap, "{}", body_bytes(&traces));
}

// The same budget, narrowed to the row that is over it on its own: it cannot
// be sealed into any submission the budget allows, so it goes and the lines behind
// it ship in its place rather than waiting out the spool's own cap.
#[test]
fn a_line_larger_than_the_whole_budget_is_dropped_rather_than_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let carriable = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","sev":"Info"}"#,
    );
    let carriable_bytes = line_row_bytes(&carriable);
    let cap = framing_bytes(Payload::trace_lines(vec![])) + 2 * carriable_bytes;
    let mut delivery = delivery_with_submission_cap(&dir, cap);
    let mut lines = temp_log_spool(&dir);
    lines
        .push(&trace_line(&format!(
            r#"{{"ns":"{}"}}"#,
            "x".repeat(4 * carriable_bytes as usize)
        )))
        .unwrap();
    lines.push(&carriable).unwrap();

    let submission = delivery.take_line_submission(test_now()).unwrap().unwrap();

    let opened =
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap();
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
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosPeer.Announcement","sev":"Info"}"#,
    );
    let mut lines = temp_log_spool(&dir);
    lines.push(&line).unwrap();
    let framing = framing_bytes(Payload::trace_lines(vec![]));

    // `framing_bytes` stamps UNIX_EPOCH, so this budget is the framing exactly
    // and what it leaves for rows is zero rather than negative.
    let attempt = delivery_with_submission_cap(&dir, framing)
        .take_line_submission(OffsetDateTime::UNIX_EPOCH);

    match attempt {
        Err(metsuke::delivery::DeliveryError::BudgetBelowFraming { .. }) => {}
        Err(other) => panic!("a budget under the framing reports itself, got {other:?}"),
        Ok(_) => panic!("a budget under the framing seals no submission"),
    }
    let submission = delivery_with_submission_cap(&dir, UNBOUNDED)
        .take_line_submission(test_now())
        .unwrap()
        .unwrap();
    let opened =
        envelope::open(&submission.attestation, &submission.wire_bytes, TEST_LIMITS).unwrap();
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
// `upload_batch_max_bytes` has to stay under: the submission opens at exactly the
// charged total and is refused one byte under it, so a framing change that moves
// the payload's real cost without the `bytes` column fails here.
#[test]
fn the_spool_charges_a_row_what_the_server_inflates_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut delivery = temp_delivery(&dir);
    let mut lines = temp_log_spool(&dir);
    for secs in 1..=5 {
        delivery.push(&scrape_at(secs)).unwrap();
        lines
            .push(&trace_line(&format!(
                r#"{{"at":"2026-08-25T18:19:38.018453907Z","ns":"Consensus.LeiosKernel","data":{{"msg":"\"quoted\" {secs}"}}}}"#
            )))
            .unwrap();
    }
    let charged = [
        charged_bytes(&dir, "scrapes"),
        charged_bytes(&dir, "log_lines"),
    ];
    let submissions = [
        delivery.take_submission(test_now()).unwrap().unwrap(),
        delivery.take_line_submission(test_now()).unwrap().unwrap(),
    ];

    for (charged, submission) in charged.into_iter().zip(submissions) {
        let opened = |max_decompressed_bytes| {
            envelope::open(
                &submission.attestation,
                &submission.wire_bytes,
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

/// A timestamp with the subsecond digits a real submission carries: the header line
/// is longer for them, and the framing reserve has to cover that.
fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(1_780_000_000_123_456_789).unwrap()
}
