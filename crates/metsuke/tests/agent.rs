//! External-behaviour test for the agent loop body (ticket metsuke-4zo.5):
//! a recorded Leios scrape body in, a signed compressed batch out that the
//! server's own call (`open`) accepts; an ack drains the spool, any failure
//! leaves it intact.

use std::time::Duration;

use metsuke::agent::Agent;
use metsuke::delivery::Delivery;
use metsuke::scrape::ScrapeConfig;
use metsuke::scraper::ScraperConfig;
use metsuke::sntp::SntpConfig;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig};
use metsuke::uploader::{UploadConfig, UploadOutcome};
use metsuke_wire::envelope::{self, Signature, VerifyingKey};
use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::{TEST_LIMITS, block_number, test_key, test_provenance, trace_line};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

/// Wide enough that no spool or batch cap fires here.
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
        Delivery::new(spool, test_key(), 0, UNBOUNDED),
        UploadConfig {
            upload_url: format!("{}/v1/submit", uploads.uri()).try_into().unwrap(),
            timeout: Duration::from_secs(5),
        },
        test_key().verifying_key(),
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

// Acceptance: recorded scrape bodies in → signed, compressed batch with
// correct headers out, and the ack deletes the delivered rows.
#[tokio::test]
async fn scraped_metrics_upload_as_a_verified_batch_and_ack_drains_the_spool() {
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
        matches!(first, Some(UploadOutcome::Acked(_))),
        "expected ack, got {first:?}"
    );
    assert!(second.is_none(), "acked rows must leave the spool");

    let request = &uploads.received_requests().await.unwrap()[0];
    let header = |name: &str| request.headers.get(name).unwrap().to_str().unwrap();
    let vkey_bytes = hex::decode::<32>(header(HEADER_VKEY)).unwrap();
    let sig_bytes = hex::decode::<64>(header(HEADER_SIGNATURE)).unwrap();
    let opened = envelope::open(
        &VerifyingKey::from_bytes(&vkey_bytes).unwrap(),
        &request.body,
        &Signature::from_bytes(&sig_bytes),
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

    assert!(
        matches!(first, Some(UploadOutcome::Acked(_))),
        "expected ack, got {first:?}"
    );
    assert!(second.is_none(), "both streams must have been acked");

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
                &VerifyingKey::from_bytes(&vkey).unwrap(),
                &request.body,
                &Signature::from_bytes(&signature),
                TEST_LIMITS,
            )
            .unwrap()
            .schema_version()
        })
        .collect();
    assert_eq!(versions, [1, 2]);
}

// Acceptance: 5xx (and 4xx alike) leaves the spool intact — the same rows
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

    assert!(matches!(first, Some(UploadOutcome::Retryable(_))));
    assert!(
        matches!(second, Some(UploadOutcome::Retryable(_))),
        "unacked rows must be offered again, got {second:?}"
    );
}
