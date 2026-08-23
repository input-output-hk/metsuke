//! Upload seam tests (ticket metsuke-4zo.5): a sealed batch in, one POST
//! out carrying the ADR-0001 header contract, the server's answer classified
//! into ack / retryable / rejected without touching the spool.

use std::time::Duration;

use metsuke::delivery::Delivery;
use metsuke::spool::{Spool, SpoolConfig};
use metsuke::uploader::{UploadConfig, UploadOutcome, upload};
use metsuke_wire::envelope::{
    self, HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, Sample, Signature, VerifyingKey,
};
use time::OffsetDateTime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::test_key;

// Large enough for any test batch; the real limit is server config.
const TEST_DECOMPRESS_LIMIT: u64 = 64 * 1024 * 1024;

fn sealed_test_batch(dir: &tempfile::TempDir) -> metsuke::delivery::SealedBatch {
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_samples: 100,
    })
    .unwrap();
    let pool_id = PoolId::from_cold_key(&test_key().verifying_key());
    let mut delivery = Delivery::new(spool, test_key(), pool_id, 0);
    delivery
        .push(&Sample {
            sampled_at: OffsetDateTime::UNIX_EPOCH,
            block_height: Some(5),
            slot: None,
            slot_in_epoch: None,
            epoch: None,
            sync_progress: None,
            node_version: None,
            node_revision: None,
            clock_offset_ms: None,
        })
        .unwrap();
    delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap()
}

fn upload_config(server: &MockServer) -> UploadConfig {
    UploadConfig {
        upload_url: format!("{}/v1/submit", server.uri()),
        pool_id: PoolId::from_cold_key(&test_key().verifying_key()),
        timeout: Duration::from_secs(5),
    }
}

// Acceptance: a newer latest_version in the ACK must be detected, an equal
// or older one must not (ADR 0006).
#[test]
fn nudge_fires_only_when_the_ack_version_is_newer() {
    use metsuke::uploader::newer_version_available;
    assert!(newer_version_available("0.1.0", "0.2.0"));
    assert!(newer_version_available("0.1.0", "0.1.1"));
    assert!(newer_version_available("0.9.0", "0.10.0"));
    assert!(!newer_version_available("0.1.0", "0.1.0"));
    assert!(!newer_version_available("0.2.0", "0.1.0"));
    // A malformed ack version cannot claim to be newer.
    assert!(!newer_version_available("0.1.0", "not-a-version"));
}

// 5xx means the server may recover on its own: retry, no operator action.
#[tokio::test]
async fn server_error_is_retryable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = upload_config(&server);
    let outcome =
        tokio::task::spawn_blocking(move || upload(&config, &test_key().verifying_key(), &batch))
            .await
            .unwrap();
    assert!(
        matches!(outcome, UploadOutcome::Retryable(_)),
        "expected retryable, got {outcome:?}"
    );
}

// An unreachable server is the same scheduling decision as a 5xx.
#[tokio::test]
async fn unreachable_server_is_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = UploadConfig {
        // TEST-NET-1 (RFC 5737) is unroutable: connect fails, nothing answers.
        upload_url: "http://192.0.2.1:9/v1/submit".into(),
        pool_id: PoolId::from_cold_key(&test_key().verifying_key()),
        timeout: Duration::from_millis(200),
    };
    let outcome =
        tokio::task::spawn_blocking(move || upload(&config, &test_key().verifying_key(), &batch))
            .await
            .unwrap();
    assert!(
        matches!(outcome, UploadOutcome::Retryable(_)),
        "expected retryable, got {outcome:?}"
    );
}

// 4xx carries a reason an operator must act on (user story 12): the outcome
// keeps the server's own words for the journal.
#[tokio::test]
async fn client_error_is_rejected_with_the_server_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string("pool not on the allowlist"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = upload_config(&server);
    let outcome =
        tokio::task::spawn_blocking(move || upload(&config, &test_key().verifying_key(), &batch))
            .await
            .unwrap();
    let UploadOutcome::Rejected { status, reason } = outcome else {
        panic!("expected rejected, got {outcome:?}");
    };
    assert_eq!(status, 403);
    assert_eq!(reason, "pool not on the allowlist");
}

// The full header contract: the request the server sees must verify with
// nothing but its own headers and body (ADR 0001).
#[tokio::test]
async fn acked_upload_carries_verifiable_headers_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = upload_config(&server);

    let outcome =
        tokio::task::spawn_blocking(move || upload(&config, &test_key().verifying_key(), &batch))
            .await
            .unwrap();

    let UploadOutcome::Acked(ack) = outcome else {
        panic!("expected ack, got {outcome:?}");
    };
    assert_eq!(ack.latest_version, "0.1.0");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let header = |name: &str| request.headers.get(name).unwrap().to_str().unwrap();
    assert_eq!(
        header(HEADER_POOL_ID),
        PoolId::from_cold_key(&test_key().verifying_key()).to_bech32()
    );
    assert_eq!(header("content-encoding"), "zstd");
    let vkey_bytes = hex::decode::<32>(header(HEADER_VKEY)).unwrap();
    let vkey = VerifyingKey::from_bytes(&vkey_bytes).unwrap();
    let sig_bytes = hex::decode::<64>(header(HEADER_SIGNATURE)).unwrap();
    let signature = Signature::from_bytes(&sig_bytes);
    let opened = envelope::open(&vkey, &request.body, &signature, TEST_DECOMPRESS_LIMIT).unwrap();
    assert_eq!(opened.samples[0].block_height, Some(5));
    assert_eq!(
        opened.pool_id,
        PoolId::from_cold_key(&test_key().verifying_key())
    );
}
