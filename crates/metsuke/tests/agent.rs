//! External-behaviour test for the agent loop body (ticket metsuke-4zo.5):
//! a recorded Leios scrape body in, a signed compressed batch out that the
//! server's own call (`open`) accepts; an ack drains the spool, any failure
//! leaves it intact.

use std::time::Duration;

use metsuke::agent::Agent;
use metsuke::delivery::Delivery;
use metsuke::sampler::SamplerConfig;
use metsuke::scrape::ScrapeConfig;
use metsuke::sntp::SntpConfig;
use metsuke::spool::{Spool, SpoolConfig};
use metsuke::uploader::{UploadConfig, UploadOutcome};
use metsuke_wire::envelope::{self, PoolId, Signature, VerifyingKey};
use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::test_key;

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

// Large enough for any test batch; the real limit is server config.
const TEST_DECOMPRESS_LIMIT: u64 = 64 * 1024 * 1024;

/// An agent sampling the given metrics server and uploading to the given
/// upload server. SNTP points at a dead loopback port so the offset is null.
fn test_agent(dir: &tempfile::TempDir, metrics: &MockServer, uploads: &MockServer) -> Agent {
    let pool_id = PoolId::from_cold_key(&test_key().verifying_key());
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_samples: 100,
    })
    .unwrap();
    Agent::new(
        SamplerConfig {
            scrape: ScrapeConfig {
                metrics_url: format!("{}/metrics", metrics.uri()),
                timeout: Duration::from_secs(5),
                max_body_bytes: 1024 * 1024,
            },
            sntp: SntpConfig {
                servers: vec![],
                timeout: Duration::from_millis(50),
            },
        },
        Delivery::new(spool, test_key(), pool_id, 0),
        UploadConfig {
            upload_url: format!("{}/v1/submit", uploads.uri()),
            pool_id,
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
async fn sampled_metrics_upload_as_a_verified_batch_and_ack_drains_the_spool() {
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
        agent.sample_once().unwrap();
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
        TEST_DECOMPRESS_LIMIT,
    )
    .unwrap();
    // Recorded-body field values: tests/scrape.rs.
    assert_eq!(opened.samples.len(), 1);
    assert_eq!(opened.samples[0].block_height, Some(5));
    assert_eq!(opened.samples[0].clock_offset_ms, None);
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
        agent.sample_once().unwrap();
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
