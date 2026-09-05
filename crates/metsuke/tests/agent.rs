//! External-behaviour test for the agent loop body (ticket metsuke-4zo.5):
//! a recorded Leios scrape body in, a signed compressed submission out that the
//! server's own call (`open`) accepts; an ack drains the spool, any failure
//! leaves it intact.

use std::num::NonZeroUsize;
use std::time::Duration;

use metsuke::agent::{Agent, Uploaded};
use metsuke::delivery::Delivery;
use metsuke::scrape::ScrapeConfig;
use metsuke::scraper::ScraperConfig;
use metsuke::sntp::SntpConfig;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig};
use metsuke::uploader::{UploadConfig, UploadOutcome};
use metsuke_wire::envelope::{self};
use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::{
    TEST_LIMITS, attestation_of, block_number, test_pool_id, test_provenance, test_submission_key,
    trace_line,
};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

/// Wide enough that no spool or submission cap fires here.
const UNBOUNDED: u64 = 64 * 1024 * 1024;

const NO_CONTENTION: Duration = Duration::from_secs(1);

fn spool_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("spool.sqlite")
}

/// The trace-line writer, as the binary spawns it: its own connection to the
/// same file the upload loop reads.
fn test_log_spool(dir: &tempfile::TempDir) -> LogSpool {
    LogSpool::open(&LogSpoolConfig {
        path: spool_path(dir),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap()
}

/// An agent scraping the given metrics server and uploading to the given
/// upload server. SNTP points at a dead loopback port so the offset is null.
fn test_agent(dir: &tempfile::TempDir, metrics: &MockServer, uploads: &MockServer) -> Agent {
    // Wide enough that a tick drains whatever a test spooled, and one submission
    // holds it, so a test that is not about either says nothing about them.
    agent_with(dir, metrics, uploads, UNBOUNDED, shipped_submissions())
}

/// The shipped `upload_max_submissions`, so the suite exercises the allowance
/// operators actually run with.
fn shipped_submissions() -> usize {
    support::shipped_config().upload_max_submissions.get()
}

fn agent_with(
    dir: &tempfile::TempDir,
    metrics: &MockServer,
    uploads: &MockServer,
    batch_max_bytes: u64,
    max_submissions: usize,
) -> Agent {
    let spool = Spool::open(&SpoolConfig {
        path: spool_path(dir),
        max_bytes: UNBOUNDED,
        busy_timeout: NO_CONTENTION,
        provenance: test_provenance(),
    })
    .unwrap();
    Agent::new(
        ScraperConfig {
            scrape: ScrapeConfig {
                metrics_url: format!("{}/metrics", metrics.uri()).try_into().unwrap(),
                timeout: Duration::from_secs(5),
                max_body_bytes: 1024 * 1024,
            },
            sntp: SntpConfig {
                servers: vec![],
                timeout: Duration::from_millis(50),
            },
        },
        Delivery::new(spool, test_submission_key(), 0, batch_max_bytes),
        UploadConfig {
            upload_url: format!("{}/v1/submit", uploads.uri()).try_into().unwrap(),
            timeout: Duration::from_secs(5),
            max_submissions: NonZeroUsize::new(max_submissions).expect("the allowance is not zero"),
        },
        test_pool_id(),
    )
}

async fn metrics_server() -> MockServer {
    let metrics = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(RECORDED_CHAIN, "text/plain;version=0.0.4;charset=utf-8"),
        )
        .mount(&metrics)
        .await;
    metrics
}

// Acceptance: recorded scrape bodies in → signed, compressed submission with
// correct headers out, and the ack deletes the delivered rows.
#[tokio::test]
async fn scraped_metrics_upload_as_a_verified_submission_and_ack_drains_the_spool() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&uploads)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut agent = test_agent(&dir, &metrics, &uploads);

    let (first, second) = tokio::task::spawn_blocking(move || {
        agent.scrape_once().unwrap();
        let first = agent.upload_once().unwrap();
        let second = agent.upload_once().unwrap();
        (first, second)
    })
    .await
    .unwrap();

    assert!(
        matches!(
            first.as_slice(),
            [Uploaded {
                outcome: UploadOutcome::Acked(_),
                carried: "scrape",
                ..
            }]
        ),
        "expected one acked scrape submission, got {first:?}"
    );
    assert!(second.is_empty(), "acked rows must leave the spool");

    let request = &uploads.received_requests().await.unwrap()[0];
    let header = |name: &str| request.headers.get(name).unwrap().to_str().unwrap();
    let vkey_bytes = hex::decode::<32>(header(HEADER_VKEY)).unwrap();
    let sig_bytes = hex::decode::<64>(header(HEADER_SIGNATURE)).unwrap();
    let opened = envelope::open(
        &attestation_of(&vkey_bytes, &sig_bytes),
        &request.body,
        TEST_LIMITS,
    )
    .unwrap();
    // What the recorded body states: tests/scrape.rs.
    let scrapes = opened.scrapes().expect("a scrape tick uploads scrapes");
    assert_eq!(scrapes.len(), 1);
    assert_eq!(block_number(&scrapes[0]), Some(5));
    assert_eq!(scrapes[0].clock_offset_ms, None);
}

// One upload tick clears both streams: an agent that shipped scrapes and left
// the trace lines for the next tick would deliver them an upload interval late
// for as long as any scrape is ever spooled.
#[tokio::test]
async fn one_tick_uploads_both_the_scrapes_and_the_trace_lines() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&uploads)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut agent = test_agent(&dir, &metrics, &uploads);
    test_log_spool(&dir)
        .push(&trace_line(r#"{"ns":"one trace line"}"#))
        .unwrap();

    let (first, second) = tokio::task::spawn_blocking(move || {
        agent.scrape_once().unwrap();
        let first = agent.upload_once().unwrap();
        let second = agent.upload_once().unwrap();
        (first, second)
    })
    .await
    .unwrap();

    // Both come back, not just the last. Reporting only the trace lines would
    // leave the operator of an agent collecting them with no sign that their
    // scrapes were ever taken.
    assert!(
        matches!(
            first.as_slice(),
            [
                Uploaded {
                    outcome: UploadOutcome::Acked(_),
                    carried: "scrape",
                    ..
                },
                Uploaded {
                    outcome: UploadOutcome::Acked(_),
                    carried: "trace line",
                    ..
                }
            ]
        ),
        "expected an acked submission per stream, got {first:?}"
    );
    assert!(second.is_empty(), "both streams must have been acked");

    let versions: Vec<u32> = uploads
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            let vkey =
                hex::decode::<32>(request.headers.get(HEADER_VKEY).unwrap().to_str().unwrap())
                    .unwrap();
            let signature = hex::decode::<64>(
                request
                    .headers
                    .get(HEADER_SIGNATURE)
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            envelope::open(
                &attestation_of(&vkey, &signature),
                &request.body,
                TEST_LIMITS,
            )
            .unwrap()
            .schema_version()
        })
        .collect();
    assert_eq!(versions, [1, 2]);
}

// Acceptance: 5xx (and 4xx alike) leaves the spool intact. The same rows
// are offered again on the next attempt.
#[tokio::test]
async fn failed_upload_keeps_the_rows_for_the_next_attempt() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&uploads)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut agent = test_agent(&dir, &metrics, &uploads);

    let (first, second) = tokio::task::spawn_blocking(move || {
        agent.scrape_once().unwrap();
        let first = agent.upload_once().unwrap();
        let second = agent.upload_once().unwrap();
        (first, second)
    })
    .await
    .unwrap();

    // One attempt, not two: a submission the server did not take ends the
    // tick rather than the trace lines being offered on top of it.
    assert!(matches!(
        first.as_slice(),
        [Uploaded {
            outcome: UploadOutcome::Retryable(_),
            ..
        }]
    ));
    assert!(
        matches!(
            second.as_slice(),
            [Uploaded {
                outcome: UploadOutcome::Retryable(_),
                ..
            }]
        ),
        "unacked rows must be offered again, got {second:?}"
    );
}

/// A submission cap that holds one trace line and no more, so a tick has to send
/// one submission per line to clear the spool.
fn one_line_per_submission(line: &str) -> u64 {
    lines_per_submission(line, 1)
}

/// A submission cap that holds `lines` of them and no more. Half a row of
/// slack, because the header carries a timestamp whose subsecond digits vary
/// per run: a cap measured to the byte sometimes holds one line fewer, which
/// makes what a tick does a coin flip rather than a count.
fn lines_per_submission(line: &str, lines: u64) -> u64 {
    // The framing is spent before any row is, as tests/delivery.rs measures it.
    // The timestamp carries subsecond digits because `upload_once` stamps with
    // `now_utc`, whose header line is longer than the epoch's by them.
    let empty = envelope::Envelope::new(
        test_provenance(),
        metsuke::AGENT_VERSION.to_string(),
        u64::MAX,
        time::OffsetDateTime::from_unix_timestamp_nanos(1_780_000_000_123_456_789).unwrap(),
        envelope::Payload::trace_lines(vec![]),
    );
    let framing = (envelope::HEADER_OFFSET + envelope::header_json(&empty).unwrap().len()) as u64;
    let row = envelope::PayloadLine::trace_line(&trace_line(line), &test_provenance())
        .unwrap()
        .wire_bytes();
    framing + lines * row + row / 2
}

// The wedge this fixes: a node emits more between ticks than one submission
// carries, so a tick that sent one left the difference spooled every hour
// until the cap discarded it. A tick drains the stream instead.
#[tokio::test]
async fn one_tick_drains_a_stream_that_outgrew_a_single_submission() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&uploads)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"ns":"Consensus.LeiosPeer.Msg"}"#;
    let mut agent = agent_with(
        &dir,
        &metrics,
        &uploads,
        one_line_per_submission(line),
        shipped_submissions(),
    );
    let mut spool = test_log_spool(&dir);
    for _ in 0..5 {
        spool.push(&trace_line(line)).unwrap();
    }

    let (first, second) = tokio::task::spawn_blocking(move || {
        let first = agent.upload_once().unwrap();
        let second = agent.upload_once().unwrap();
        (first, second)
    })
    .await
    .unwrap();

    assert_eq!(
        first.len(),
        5,
        "every spooled line has to leave in the one tick, got {first:?}"
    );
    assert!(
        first.iter().all(|one| one.carried == "trace line"),
        "{first:?}"
    );
    assert!(
        second.is_empty(),
        "the stream must be drained, got {second:?}"
    );
}

// And what a tick drains is the backlog it found, not the stream: a node
// emitting faster than a submission's round trip was chased to the allowance,
// a request, a counter and an object per handful of lines that arrived while
// the last one was in flight. The server here appends one as it answers each,
// which is that node in miniature.
#[tokio::test]
async fn a_tick_does_not_chase_lines_that_arrive_while_it_runs() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"ns":"Consensus.LeiosPeer.Msg"}"#;
    let writing = std::sync::Arc::new(std::sync::Mutex::new(test_log_spool(&dir)));
    let arriving = std::sync::Arc::clone(&writing);
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(move |_: &wiremock::Request| {
            arriving
                .lock()
                .expect("no panic holds this lock")
                .push(&trace_line(line))
                .expect("the line spools");
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "latest_version": "0.1.0"
            }))
        })
        .mount(&uploads)
        .await;
    let mut agent = agent_with(
        &dir,
        &metrics,
        &uploads,
        lines_per_submission(line, 3),
        shipped_submissions(),
    );
    for _ in 0..5 {
        writing
            .lock()
            .expect("no panic holds this lock")
            .push(&trace_line(line))
            .unwrap();
    }

    let sent = tokio::task::spawn_blocking(move || agent.upload_once().unwrap())
        .await
        .unwrap();

    assert_eq!(
        sent.iter().map(|one| one.lines).collect::<Vec<_>>(),
        [3, 3],
        "the five spooled leave in two, and what arrived meanwhile waits for \
         the next tick, got {sent:?}"
    );
}

// And the allowance is what bounds it, so a spool far behind does not upload
// without end on one tick.
#[tokio::test]
async fn a_tick_sends_no_more_than_its_allowance() {
    let metrics = metrics_server().await;
    let uploads = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&uploads)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"ns":"Consensus.LeiosPeer.Msg"}"#;
    let mut agent = agent_with(&dir, &metrics, &uploads, one_line_per_submission(line), 3);
    let mut spool = test_log_spool(&dir);
    for _ in 0..10 {
        spool.push(&trace_line(line)).unwrap();
    }

    let sent = tokio::task::spawn_blocking(move || agent.upload_once().unwrap())
        .await
        .unwrap();

    assert_eq!(sent.len(), 3, "the allowance bounds the tick, got {sent:?}");
}
