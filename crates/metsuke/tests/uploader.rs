//! Upload seam tests (ticket metsuke-4zo.5): a sealed batch in, one POST
//! out carrying the ADR-0001 header contract, the server's answer classified
//! into ack / retryable / rejected without touching the spool.

use std::time::Duration;

use metsuke::delivery::Delivery;
use metsuke::spool::{Spool, SpoolConfig};
use metsuke::uploader::{UploadConfig, UploadOutcome, upload};
use metsuke_wire::envelope::{
    self, HEADER_SIGNATURE, HEADER_VKEY, PoolId, Sample, Signature, VerifyingKey,
};
use time::OffsetDateTime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::{TEST_LIMITS, test_key, test_provenance};

const UNBOUNDED: u64 = 64 * 1024 * 1024;

fn sealed_test_batch(dir: &tempfile::TempDir) -> metsuke::delivery::SealedBatch {
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_bytes: UNBOUNDED,
        busy_timeout: Duration::from_secs(1),
        provenance: test_provenance(),
    })
    .unwrap();
    let mut delivery = Delivery::new(spool, test_key(), 0, UNBOUNDED);
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

fn upload_config(base_url: &str) -> UploadConfig {
    UploadConfig {
        upload_url: format!("{base_url}/v1/submit").try_into().unwrap(),
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
        .respond_with(ResponseTemplate::new(503).set_body_string("no archive right now"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = upload_config(&server.uri());
    let outcome =
        tokio::task::spawn_blocking(move || upload(&config, &test_key().verifying_key(), &batch))
            .await
            .unwrap();
    // The server's own words reach the journal on this path too, not just on
    // a rejection.
    let UploadOutcome::Retryable(reason) = outcome else {
        panic!("expected retryable, got {outcome:?}");
    };
    assert_eq!(reason, "server answered 503: no archive right now");
}

// An unreachable server is the same scheduling decision as a 5xx.
#[tokio::test]
async fn unreachable_server_is_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = UploadConfig {
        timeout: Duration::from_millis(200),
        // TEST-NET-1 (RFC 5737) is unroutable: connect fails, nothing answers.
        ..upload_config("https://192.0.2.1:9")
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

// The opening two bytes of a TLS record: content type 22 (handshake), then
// the legacy record version's major byte (RFC 8446 §5.1).
const TLS_HANDSHAKE_PREFIX: [u8; 2] = [0x16, 0x03];

/// Whatever one connection to `listener` sends first. Takes more bytes than
/// the caller asserts on so a cleartext regression prints as readable text.
fn opening_bytes(listener: std::net::TcpListener, budget: Duration) -> Vec<u8> {
    use std::io::{ErrorKind, Read};

    listener.set_nonblocking(true).unwrap();
    let start = std::time::Instant::now();
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                assert!(start.elapsed() < budget, "nothing connected in {budget:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(budget)).unwrap();
    let mut buffer = [0u8; 5];
    let read = stream
        .read(&mut buffer)
        .expect("reading the peer's opening bytes");
    buffer[..read].to_vec()
}

// An https upload_url must leave the host as TLS.
#[test]
fn an_https_upload_url_speaks_tls() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || opening_bytes(listener, Duration::from_secs(5)));

    let dir = tempfile::tempdir().unwrap();
    let batch = sealed_test_batch(&dir);
    let config = upload_config(&format!("https://127.0.0.1:{port}"));
    let outcome = upload(&config, &test_key().verifying_key(), &batch);

    let bytes = peer.join().unwrap();
    assert_eq!(
        bytes.get(..2),
        Some(&TLS_HANDSHAKE_PREFIX[..]),
        "expected a TLS handshake, peer saw {:?}",
        String::from_utf8_lossy(&bytes)
    );
    // The peer read its five bytes and went away mid-handshake, so the
    // handshake fails, and a failed handshake is a transport failure.
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
    let config = upload_config(&server.uri());
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
    let config = upload_config(&server.uri());

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
    // No pool id header: the pool is the hash of the key in HEADER_VKEY, so the
    // server derives it (metsuke-jfb.4).
    assert!(request.headers.get("x-metsuke-pool-id").is_none());
    assert_eq!(header("content-encoding"), "zstd");
    let vkey_bytes = hex::decode::<32>(header(HEADER_VKEY)).unwrap();
    let vkey = VerifyingKey::from_bytes(&vkey_bytes).unwrap();
    let sig_bytes = hex::decode::<64>(header(HEADER_SIGNATURE)).unwrap();
    let signature = Signature::from_bytes(&sig_bytes);
    let opened = envelope::open(&vkey, &request.body, &signature, TEST_LIMITS).unwrap();
    let samples = opened.samples().expect("a sample batch carries samples");
    assert_eq!(samples[0].block_height, Some(5));
    assert_eq!(
        opened.pool_id,
        PoolId::from_cold_key(&test_key().verifying_key())
    );
}
