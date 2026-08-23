//! Binary-level tests: the built `metsuke-server` spawned as an operator
//! would run it, answered over a real socket. Covers the `run()` wiring, and
//! the status each outcome earns, which is only observable on the wire.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use metsuke_server::counters::CounterStore;
use metsuke_wire::envelope::{
    Ack, Envelope, HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, SigningKey,
};
use metsuke_wire::hex;
use time::OffsetDateTime;

mod support;
use support::{
    envelope_at, example_s3_archive, other_key, pool_of, seal, stored_submission, test_key,
};

/// Limits wide enough that no check fires on its own. A test exercising one
/// of them passes its own `[ingest]` body instead.
const PERMISSIVE_INGEST: &str = r#"
max_body_bytes = 1048576
max_decompressed_bytes = 4194304
rate_limit_uploads = 100
rate_limit_window_secs = 3600
max_timestamp_skew_secs = 300
"#;

/// What the S3 archive reads its credentials from. Passed to every spawn, so
/// the one test that asserts they are required can withhold them.
const TEST_CREDENTIALS: [(&str, &str); 2] = [
    ("AWS_ACCESS_KEY_ID", "test-key-id"),
    ("AWS_SECRET_ACCESS_KEY", "test-secret"),
];

/// A spawned server, killed when the test drops it.
struct Server {
    child: Child,
    url: String,
    dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Spawn on a kernel-chosen port, returning once the server has named it.
    fn start(allowed: &[PoolId], ingest: &str) -> Server {
        Server::start_with(allowed, ingest, |dir| dir.join("archive"))
    }

    fn start_with(
        allowed: &[PoolId],
        ingest: &str,
        archive_root: impl Fn(&std::path::Path) -> std::path::PathBuf,
    ) -> Server {
        Server::start_archiving(allowed, ingest, &|dir| {
            format!(
                "kind = \"filesystem\"\nroot = \"{}\"",
                archive_root(dir).display()
            )
        })
    }

    /// Spawn with a caller-written `[archive]` body, which is what lets a test
    /// point the binary at an S3 endpoint it controls.
    fn start_archiving(
        allowed: &[PoolId],
        ingest: &str,
        archive: &dyn Fn(&std::path::Path) -> String,
    ) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server.toml");
        let allowlist: Vec<String> = allowed
            .iter()
            .map(|pool| format!("\"{}\"", pool.to_bech32()))
            .collect();
        std::fs::write(
            &config,
            format!(
                r#"
                listen = "127.0.0.1:0"
                counters_path = "{counters}"

                [archive]
                {archive}

                [ingest]
                allowlist = [{allowlist}]
                {ingest}
                "#,
                counters = dir.path().join("counters.sqlite").display(),
                archive = archive(dir.path()),
                allowlist = allowlist.join(", "),
            ),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
            .args(["--config", config.to_str().unwrap()])
            .envs(TEST_CREDENTIALS)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let url = bound_url(child.stderr.take().unwrap());
        Server { child, url, dir }
    }

    fn archive_root(&self) -> std::path::PathBuf {
        self.dir.path().join("archive")
    }

    /// Kill the server, so what follows reads files nobody is writing.
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn counters_path(&self) -> std::path::PathBuf {
        self.dir.path().join("counters.sqlite")
    }

    /// Run a subcommand against this server's own config, as an operator would.
    fn run(&self, command: &str) -> std::process::Output {
        self.run_with(&[command])
    }

    fn run_with(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
            .args([
                "--config",
                self.dir.path().join("server.toml").to_str().unwrap(),
            ])
            .args(args)
            .envs(TEST_CREDENTIALS)
            .output()
            .unwrap()
    }

    fn rebuild_index(&self) -> String {
        let output = self.run(metsuke_server::cli::REBUILD_INDEX);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// POST a sealed batch with the ADR-0001 headers. Returns the status and
    /// the body, which is the Ack on success and the rejection text
    /// otherwise.
    fn post(&self, key: &SigningKey, envelope: &Envelope) -> (u16, String) {
        let (wire_bytes, signature) = seal(key, envelope);
        self.post_raw(
            &pool_of(key).to_bech32(),
            &hex::encode(key.verifying_key().as_bytes()),
            &hex::encode(&signature.to_bytes()),
            wire_bytes,
        )
    }

    fn post_raw(&self, pool_id: &str, vkey: &str, signature: &str, body: Vec<u8>) -> (u16, String) {
        let mut response = agent()
            .post(&self.url)
            .header(HEADER_POOL_ID, pool_id)
            .header(HEADER_VKEY, vkey)
            .header(HEADER_SIGNATURE, signature)
            .content_type("application/json")
            .send(&body[..])
            .unwrap();
        (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap(),
        )
    }
}

/// The client the server itself is reached with, generous enough that a
/// timeout here means the binary hung.
fn agent() -> ureq::Agent {
    metsuke_wire::http::agent(Duration::from_secs(30))
}

/// Reading the startup line is the readiness wait: it is printed after the
/// listener is bound.
fn bound_url(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "server exited before naming its address"
        );
        if let Some(start) = line.find("http://") {
            // Drain the rest in the background: a full stderr pipe would
            // otherwise wedge the server mid-test.
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = BufReader::new(reader.into_inner()).read_to_end(&mut sink);
            });
            return line[start..].trim().to_string();
        }
    }
}

/// The server judges skew against its own clock, so test envelopes are
/// stamped now rather than at the suite's fixed instant.
fn envelope_now(key: &SigningKey, counter: u64) -> Envelope {
    envelope_at(key, counter, OffsetDateTime::now_utc())
}

#[test]
fn a_missing_config_exits_nonzero_naming_the_path() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args(["--config", "/nonexistent/metsuke-server.toml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/nonexistent/metsuke-server.toml"),
        "{stderr}"
    );
}

#[test]
fn an_unknown_argument_exits_nonzero_showing_the_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .arg("--verbose")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--config"), "{stderr}");
}

#[test]
fn a_sealed_batch_is_acked_and_archived_byte_for_byte() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    let envelope = envelope_now(&key, 1);
    let (status, body) = server.post(&key, &envelope);
    assert_eq!(status, 200, "{body}");
    let ack: Ack = serde_json::from_str(&body).unwrap();
    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);

    let (wire_bytes, signature) = seal(&key, &envelope);
    let stored = stored_submission(
        &key,
        envelope.counter,
        envelope.timestamp,
        signature,
        &wire_bytes,
    );
    let object = server.archive_root().join(stored.object_key());
    assert_eq!(
        std::fs::read(&object).unwrap(),
        wire_bytes,
        "the archived object must be the received body verbatim"
    );
}

/// The counter is what makes a resent upload detectable, and the client's own
/// retry looks exactly like the attack.
#[test]
fn the_same_batch_twice_is_refused_the_second_time() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    let envelope = envelope_now(&key, 7);
    assert_eq!(server.post(&key, &envelope).0, 200);
    let (status, reason) = server.post(&key, &envelope);
    assert_eq!(status, 400, "{reason}");
    assert!(reason.contains("does not advance"), "got: {reason}");
}

#[test]
fn a_pool_off_the_allowlist_is_refused_and_stores_nothing() {
    let allowed = test_key();
    let stranger = other_key();
    let server = Server::start(&[pool_of(&allowed)], PERMISSIVE_INGEST);
    let (status, reason) = server.post(&stranger, &envelope_now(&stranger, 1));
    assert_eq!(status, 403, "{reason}");
    assert!(reason.contains("allowlist"), "got: {reason}");
    assert!(
        !server.archive_root().exists(),
        "a refused pool must leave no object behind"
    );
}

#[test]
fn a_body_over_the_limit_is_refused_before_verification() {
    let key = test_key();
    let server = Server::start(
        &[pool_of(&key)],
        r#"
        max_body_bytes = 16
        max_decompressed_bytes = 4194304
        rate_limit_uploads = 100
        rate_limit_window_secs = 3600
        max_timestamp_skew_secs = 300
        "#,
    );
    let (status, reason) = server.post(&key, &envelope_now(&key, 1));
    assert_eq!(status, 413, "{reason}");
    assert!(reason.contains("16 byte limit"), "got: {reason}");
}

#[test]
fn a_pool_over_its_rate_limit_gets_429() {
    let key = test_key();
    let server = Server::start(
        &[pool_of(&key)],
        r#"
        max_body_bytes = 1048576
        max_decompressed_bytes = 4194304
        rate_limit_uploads = 1
        rate_limit_window_secs = 3600
        max_timestamp_skew_secs = 300
        "#,
    );
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 200);
    let (status, reason) = server.post(&key, &envelope_now(&key, 2));
    assert_eq!(status, 429, "{reason}");
}

/// An archive that cannot store must answer 5xx, or the agent acks and
/// deletes spooled samples that were never written (ADR 0004).
#[test]
fn an_unwritable_archive_answers_503() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], PERMISSIVE_INGEST, |dir| {
        // A regular file where the archive expects a directory root.
        let path = dir.join("archive-is-a-file");
        std::fs::write(&path, b"not a directory").unwrap();
        path
    });
    let (status, reason) = server.post(&key, &envelope_now(&key, 1));
    assert_eq!(status, 503, "{reason}");
}

/// A counter spent on a submission the archive refused would lock the pool
/// out of retrying it.
#[test]
fn a_failed_store_leaves_the_counter_unspent() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], PERMISSIVE_INGEST, |dir| {
        let path = dir.join("archive-is-a-file");
        std::fs::write(&path, b"not a directory").unwrap();
        path
    });
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 503);
    // Same counter, and the only reason it can still fail is the archive.
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 503);
}

/// An S3 endpoint that refuses every request, standing in for a bucket the
/// server cannot write to, with a count of what it was asked. Serves for the
/// rest of the test binary.
fn refusing_endpoint() -> (String, Arc<AtomicUsize>) {
    let endpoint = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", endpoint.server_addr());
    let refused = Arc::new(AtomicUsize::new(0));
    std::thread::spawn({
        let refused = Arc::clone(&refused);
        move || {
            for request in endpoint.incoming_requests() {
                refused.fetch_add(1, Ordering::SeqCst);
                let _ = request.respond(
                    tiny_http::Response::from_string("InternalError").with_status_code(500),
                );
            }
        }
    });
    (url, refused)
}

/// The S3 archive on the ingest path: a PUT that will not land, retried as
/// configured, must reach the client as 503 with the counter unspent, or the
/// agent acks samples the bucket never took (ADR 0004).
#[test]
fn an_s3_put_that_fails_after_its_retry_answers_503_and_spends_no_counter() {
    let key = test_key();
    let (endpoint, refused) = refusing_endpoint();
    let server = Server::start_archiving(&[pool_of(&key)], PERMISSIVE_INGEST, &|_| {
        example_s3_archive(&endpoint, 1)
    });
    let (status, reason) = server.post(&key, &envelope_now(&key, 1));
    assert_eq!(status, 503, "{reason}");
    assert_eq!(
        refused.load(Ordering::SeqCst),
        2,
        "the PUT and its one configured retry must both have been attempted"
    );
    // The same counter again: the only reason it can still fail is the bucket.
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 503);
}

#[test]
fn verify_archive_on_a_filesystem_archive_exits_nonzero() {
    let key = test_key();
    let mut server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 200);
    server.stop();

    let output = server.run(metsuke_server::cli::VERIFY_ARCHIVE);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no metadata"), "{stderr}");
}

/// Two subcommands in one invocation: one would silently win, and the operator
/// would believe the other ran.
#[test]
fn two_subcommands_are_refused_naming_both() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args([
            metsuke_server::cli::REBUILD_INDEX,
            metsuke_server::cli::VERIFY_ARCHIVE,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(metsuke_server::cli::REBUILD_INDEX)
            && stderr.contains(metsuke_server::cli::VERIFY_ARCHIVE),
        "{stderr}"
    );
}

/// An empty bucket verified nothing, so it must not exit zero: that code is
/// what a monitor reads as "the corpus is intact".
#[test]
fn verify_archive_on_an_empty_bucket_exits_nonzero() {
    let key = test_key();
    let endpoint = empty_bucket_endpoint();
    let server = Server::start_archiving(&[pool_of(&key)], PERMISSIVE_INGEST, &|_| {
        example_s3_archive(&endpoint, 0)
    });
    let output = server.run(metsuke_server::cli::VERIFY_ARCHIVE);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no objects"), "{stderr}");
}

/// An endpoint answering a valid listing of an empty bucket.
fn empty_bucket_endpoint() -> String {
    let endpoint = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", endpoint.server_addr());
    std::thread::spawn(move || {
        for request in endpoint.incoming_requests() {
            let _ = request.respond(tiny_http::Response::from_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
    <Name>metsuke-test</Name>
    <IsTruncated>false</IsTruncated>
    <EncodingType>url</EncodingType>
</ListBucketResult>"#
                    .to_string(),
            ));
        }
    });
    url
}

#[test]
fn verify_archive_fails_when_the_bucket_cannot_be_listed() {
    let key = test_key();
    let (endpoint, _) = refusing_endpoint();
    let server = Server::start_archiving(&[pool_of(&key)], PERMISSIVE_INGEST, &|_| {
        example_s3_archive(&endpoint, 0)
    });
    let output = server.run(metsuke_server::cli::VERIFY_ARCHIVE);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("500"), "{stderr}");
}

/// Credentials come from the environment, so a server configured for S3
/// without them must refuse to start rather than serve 503s.
#[test]
fn an_s3_archive_without_credentials_exits_nonzero_naming_them() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            r#"
            listen = "127.0.0.1:0"
            counters_path = "{counters}"

            [archive]
            {archive}

            [ingest]
            allowlist = ["{pool}"]
            {PERMISSIVE_INGEST}
            "#,
            counters = dir.path().join("counters.sqlite").display(),
            archive = example_s3_archive("http://127.0.0.1:9", 0),
            pool = pool_of(&test_key()).to_bech32(),
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args(["--config", config.to_str().unwrap()])
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("AWS_ACCESS_KEY_ID"), "{stderr}");
}

#[test]
fn rebuild_index_restores_the_counter_state_from_the_archive() {
    let key = test_key();
    let mut server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    assert_eq!(server.post(&key, &envelope_now(&key, 7)).0, 200);
    server.stop();
    std::fs::remove_file(server.counters_path()).unwrap();

    let summary = server.rebuild_index();
    assert!(
        summary.contains("1 objects") && summary.contains(&pool_of(&key).to_bech32()),
        "got: {summary}"
    );
    let counters = CounterStore::open(&server.counters_path()).unwrap();
    assert_eq!(counters.last_counter(pool_of(&key)).unwrap(), Some(7));
}

/// Refused as misplaced, not as unknown: the usage text carries the flag name
/// too, so only `ArgsError::AllowEmptyWithoutRebuild`'s own words tell the two
/// refusals apart.
#[test]
fn allow_empty_without_rebuild_index_is_refused() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    for args in [
        vec![metsuke_server::cli::ALLOW_EMPTY],
        vec![
            metsuke_server::cli::VERIFY_ARCHIVE,
            metsuke_server::cli::ALLOW_EMPTY,
        ],
    ] {
        let output = server.run_with(&args);
        assert!(!output.status.success(), "{args:?} must be refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("only means anything to"),
            "{args:?}: {stderr}"
        );
    }
}

/// `rebuild::EmptyArchive` reached from the command line, both ways.
#[test]
fn rebuild_index_on_an_archive_with_nothing_in_it_exits_nonzero() {
    let key = test_key();
    let mut server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    server.stop();

    let output = server.run(metsuke_server::cli::REBUILD_INDEX);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no objects") && stderr.contains(metsuke_server::cli::ALLOW_EMPTY),
        "{stderr}"
    );
    // Which archive listed nothing is the whole question the operator is being
    // asked to answer.
    assert!(
        stderr.contains(server.archive_root().to_str().unwrap()),
        "{stderr}"
    );

    // Either order, as `Args::parse` claims.
    for args in [
        [
            metsuke_server::cli::REBUILD_INDEX,
            metsuke_server::cli::ALLOW_EMPTY,
        ],
        [
            metsuke_server::cli::ALLOW_EMPTY,
            metsuke_server::cli::REBUILD_INDEX,
        ],
    ] {
        let output = server.run_with(&args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("0 objects"));
    }
}

#[test]
fn a_request_without_the_headers_is_refused_naming_the_first_missing_one() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    let mut response = agent().post(&server.url).send(&b""[..]).unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert!(
        response
            .body_mut()
            .read_to_string()
            .unwrap()
            .contains(HEADER_POOL_ID)
    );
}

#[test]
fn another_route_and_another_method_are_refused() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)], PERMISSIVE_INGEST);
    let elsewhere = server.url.replace(metsuke_server::http::SUBMIT_PATH, "/");
    let mut response = agent().post(&elsewhere).send(&b""[..]).unwrap();
    assert_eq!(response.status().as_u16(), 404);
    assert!(
        response
            .body_mut()
            .read_to_string()
            .unwrap()
            .contains(metsuke_server::http::SUBMIT_PATH)
    );

    let response = agent().get(&server.url).call().unwrap();
    assert_eq!(response.status().as_u16(), 405);
}
