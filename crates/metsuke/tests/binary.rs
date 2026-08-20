//! Binary-level tests (ticket metsuke-4zo.5): the built `metsuke` binary
//! spawned as an SPO would run it. Covers the `run()` wiring the library
//! tests can't reach — in particular that the `--signing-key` flag really
//! beats the config path, which a swapped argument pair would invert
//! without failing any type check.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use metsuke::envelope::{HEADER_SIGNATURE, HEADER_VKEY};
use metsuke::envelope::{PoolId, Signature, VerifyingKey};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use support::{decode_hex, test_key};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

// Large enough for any test batch; the real limit is server config.
const TEST_DECOMPRESS_LIMIT: u64 = 64 * 1024 * 1024;

fn write_config(
    dir: &tempfile::TempDir,
    server_uri: &str,
    signing_key_line: &str,
) -> std::path::PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            r#"
            pool_id = "{}"
            metrics_url = "{server_uri}/metrics"
            upload_url = "{server_uri}/v1/submit"
            sample_interval_secs = 1
            upload_interval_secs = 1
            sntp_servers = []
            spool_path = "{}"
            {signing_key_line}
            "#,
            PoolId::from_cold_key(&test_key().verifying_key()).to_bech32(),
            dir.path().join("spool.sqlite").display(),
        ),
    )
    .unwrap();
    path
}

fn write_test_envelope(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("pool.skey");
    std::fs::write(
        &path,
        format!(
            r#"{{"type": "StakePoolSigningKey_ed25519", "description": "", "cborHex": "5820{}"}}"#,
            "07".repeat(32)
        ),
    )
    .unwrap();
    path
}

// Acceptance: missing key at both flag and config → startup fails loudly.
#[test]
fn binary_without_a_key_anywhere_exits_nonzero_naming_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(&dir, "http://127.0.0.1:9", "");
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .args(["--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--signing-key"),
        "startup failure must name the flag, got: {stderr}"
    );
}

// The whole wiring: config + flag in, a verifiable upload out. The config's
// signing_key points at a path that does not exist, so this passes only
// when the flag wins the precedence — a swapped `resolve_signing_key`
// argument pair fails at startup instead.
#[tokio::test]
async fn binary_uploads_a_batch_signed_by_the_flag_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(RECORDED_CHAIN, "text/plain;version=0.0.4;charset=utf-8"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest_version": "0.1.0"
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let key_path = write_test_envelope(&dir);
    let missing = dir.path().join("does-not-exist.skey");
    let config = write_config(
        &dir,
        &server.uri(),
        &format!("signing_key = \"{}\"", missing.display()),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .args(["--config", config.to_str().unwrap()])
        .args(["--signing-key", key_path.to_str().unwrap()])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let post = loop {
        let post = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.method == wiremock::http::Method::POST);
        if let Some(post) = post {
            break post;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "binary exited before uploading"
        );
        assert!(Instant::now() < deadline, "no upload within the deadline");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    child.kill().unwrap();
    child.wait().unwrap();

    let header = |name: &str| post.headers.get(name).unwrap().to_str().unwrap();
    let vkey_bytes: [u8; 32] = decode_hex(header(HEADER_VKEY)).try_into().unwrap();
    let sig_bytes: [u8; 64] = decode_hex(header(HEADER_SIGNATURE)).try_into().unwrap();
    let opened = metsuke::envelope::open(
        &VerifyingKey::from_bytes(&vkey_bytes).unwrap(),
        &post.body,
        &Signature::from_bytes(&sig_bytes),
        TEST_DECOMPRESS_LIMIT,
    )
    .unwrap();
    assert_eq!(
        opened.pool_id,
        PoolId::from_cold_key(&test_key().verifying_key())
    );
    // The flag key signed it: `open` verified against the header vkey,
    // which must be the flag key's.
    assert_eq!(vkey_bytes, test_key().verifying_key().to_bytes());
}
