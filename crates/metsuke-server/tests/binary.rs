//! Binary-level tests: the built `metsuke-server` spawned as an operator
//! would run it, answered over a real socket. Covers the `run()` wiring, and
//! the status each outcome earns, which is only observable on the wire.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use metsuke_wire::envelope::{Ack, Envelope, HEADER_SIGNATURE, HEADER_VKEY, PoolId, SigningKey};
use metsuke_wire::hex;
use time::OffsetDateTime;

mod support;
use metsuke_server::config::IngestConfig;
use support::{
    DEVELOPER_PASSWORD, ServerToml, developer_config, developer_toml_with_rows, envelope_at,
    example_s3_archive, filesystem_archive, ingest_toml, nonzero_u32, nonzero_u64, object_name,
    other_key, permissive_config, pool_of, seal, server_toml, stored_submission, test_key,
};

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
    fn start(allowed: &[PoolId]) -> Server {
        Server::start_with(allowed, |_, _| {})
    }

    /// Spawn against the config the suite runs on, with `adjust` given the
    /// chance to replace the one section the test is about. It also gets the
    /// directory, so a test can put a file where that section points.
    fn start_with(
        allowed: &[PoolId],
        adjust: impl FnOnce(&mut ServerToml, &std::path::Path),
    ) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let mut config = server_toml(dir.path(), allowed);
        adjust(&mut config, dir.path());
        let path = config.write(dir.path());
        let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
            .args(["--config", path.to_str().unwrap()])
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

    /// POST a sealed batch with the ADR-0001 headers. Returns the status and
    /// the body, which is the Ack on success and the rejection text
    /// otherwise.
    fn post(&self, key: &SigningKey, envelope: &Envelope) -> (u16, String) {
        let (wire_bytes, signature) = seal(key, envelope);
        self.post_raw(
            &hex::encode(key.verifying_key().as_bytes()),
            &hex::encode(&signature.to_bytes()),
            wire_bytes,
        )
    }

    fn post_raw(&self, vkey: &str, signature: &str, body: Vec<u8>) -> (u16, String) {
        let mut response = agent()
            .post(&self.url)
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

/// A developer pull: GET one of the two read routes, with whatever
/// credentials the caller wants to present.
impl Server {
    fn base(&self) -> String {
        self.url
            .strip_suffix(metsuke_server::http::SUBMIT_PATH)
            .expect("the startup line names the submit route")
            .to_string()
    }

    /// GET `path` with the configured developer credentials.
    fn pull(&self, path: &str) -> (u16, Vec<u8>, Vec<(String, String)>) {
        self.pull_as(
            path,
            Some((&developer_config(self.dir.path()).user, DEVELOPER_PASSWORD)),
        )
    }

    fn pull_as(
        &self,
        path: &str,
        credentials: Option<(&str, &str)>,
    ) -> (u16, Vec<u8>, Vec<(String, String)>) {
        let mut request = agent().get(format!("{}{path}", self.base()));
        if let Some((user, password)) = credentials {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            request = request.header("authorization", format!("Basic {encoded}"));
        }
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(field, value)| {
                (
                    field.as_str().to_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        (
            status,
            response.body_mut().with_config().read_to_vec().unwrap(),
            headers,
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
    let server = Server::start(&[pool_of(&key)]);
    let envelope = envelope_now(&key, 1);
    let (status, body) = server.post(&key, &envelope);
    assert_eq!(status, 200, "{body}");
    let ack: Ack = serde_json::from_str(&body).unwrap();
    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);

    let (wire_bytes, _) = seal(&key, &envelope);
    let object = server.archive_root().join(only_object_key(&server));
    assert_eq!(
        std::fs::read(&object).unwrap(),
        wire_bytes,
        "the archived object must be the received body verbatim"
    );
}

/// A client that resent because it never saw the ack gets both batches stored:
/// nothing here refuses a body for having been seen before, and the ids differ.
#[test]
fn a_resent_batch_is_stored_again_rather_than_refused() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let envelope = envelope_now(&key, 7);
    assert_eq!(server.post(&key, &envelope).0, 200);
    let (status, reason) = server.post(&key, &envelope);
    assert_eq!(status, 200, "{reason}");
    assert_eq!(
        listed_keys(&server, metsuke_server::http::SUBMISSIONS_PATH).len(),
        2
    );
}

#[test]
fn a_pool_off_the_allowlist_is_refused_and_stores_nothing() {
    let allowed = test_key();
    let stranger = other_key();
    let server = Server::start(&[pool_of(&allowed)]);
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
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.ingest = ingest_toml(&IngestConfig {
            max_body_bytes: nonzero_u64(16),
            ..permissive_config(&[pool_of(&key)])
        });
    });
    let (status, reason) = server.post(&key, &envelope_now(&key, 1));
    assert_eq!(status, 413, "{reason}");
    assert!(reason.contains("16 byte limit"), "got: {reason}");
}

#[test]
fn a_pool_over_its_rate_limit_gets_429() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.ingest = ingest_toml(&IngestConfig {
            rate_limit_uploads: nonzero_u32(1),
            ..permissive_config(&[pool_of(&key)])
        });
    });
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 200);
    let (status, reason) = server.post(&key, &envelope_now(&key, 2));
    assert_eq!(status, 429, "{reason}");
}

/// An archive that cannot store must answer 5xx, or the agent acks and
/// deletes spooled samples that were never written (ADR 0004).
#[test]
fn an_unwritable_archive_answers_503() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], |config, dir| {
        // A regular file where the archive expects a directory root.
        let path = dir.join("archive-is-a-file");
        std::fs::write(&path, b"not a directory").unwrap();
        config.archive = filesystem_archive(&path);
    });
    let (status, reason) = server.post(&key, &envelope_now(&key, 1));
    assert_eq!(status, 503, "{reason}");
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
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.archive = example_s3_archive(&endpoint, 1);
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
    let mut server = Server::start(&[pool_of(&key)]);
    assert_eq!(server.post(&key, &envelope_now(&key, 1)).0, 200);
    server.stop();

    let output = server.run(metsuke_server::cli::VERIFY_ARCHIVE);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no metadata"), "{stderr}");
}

/// Two subcommand words in one invocation: one would silently win, and the
/// operator would believe the other ran.
#[test]
fn two_subcommands_are_refused_naming_both() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args([
            metsuke_server::cli::VERIFY_ARCHIVE,
            metsuke_server::cli::VERIFY_ARCHIVE,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot both run"), "{stderr}");
}

/// An empty bucket verified nothing, so it must not exit zero: that code is
/// what a monitor reads as "the corpus is intact".
#[test]
fn verify_archive_on_an_empty_bucket_exits_nonzero() {
    let key = test_key();
    let endpoint = empty_bucket_endpoint();
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.archive = example_s3_archive(&endpoint, 0);
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
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.archive = example_s3_archive(&endpoint, 0);
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
    let mut config = server_toml(dir.path(), &[pool_of(&test_key())]);
    config.archive = example_s3_archive("http://127.0.0.1:9", 0);
    let config = config.write(dir.path());
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

/// Restarting keeps nothing but the in-memory rate-limit windows, so the
/// listing a fresh process answers is the archive itself.
#[test]
fn a_restarted_server_lists_what_the_archive_still_holds() {
    let key = test_key();
    let mut server = Server::start(&[pool_of(&key)]);
    assert_eq!(server.post(&key, &envelope_now(&key, 7)).0, 200);
    let stored = only_object_key(&server);
    server.stop();

    let restarted = Server::start_with(&[pool_of(&key)], {
        let root = server.archive_root();
        move |config, _| config.archive = filesystem_archive(&root)
    });

    assert_eq!(only_object_key(&restarted), stored);
}

#[test]
fn a_request_without_the_headers_is_refused_naming_the_first_missing_one() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let mut response = agent().post(&server.url).send(&b""[..]).unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert!(
        response
            .body_mut()
            .read_to_string()
            .unwrap()
            .contains(HEADER_VKEY)
    );
}

#[test]
fn another_route_and_another_method_are_refused() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    // Not the root: that is the instructions page.
    let elsewhere = server
        .url
        .replace(metsuke_server::http::SUBMIT_PATH, "/nowhere");
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

/// Developer pull access is gated on the one credential (ticket
/// metsuke-4zo.10): both routes, no credentials, nothing read.
#[test]
fn both_developer_routes_refuse_a_request_without_credentials() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    for path in [
        metsuke_server::http::SUBMISSIONS_PATH.to_string(),
        format!("{}?key=anything", metsuke_server::http::OBJECT_PATH),
    ] {
        let (status, body, headers) = server.pull_as(&path, None);
        assert_eq!(status, 401, "{path}: {}", String::from_utf8_lossy(&body));
        assert_eq!(
            String::from_utf8_lossy(&body),
            metsuke_server::http::UNAUTHORIZED_BODY,
            "{path} told the client why it was refused"
        );
        // The challenge is what makes `curl -u` and a browser prompt work.
        let challenge = headers
            .iter()
            .find(|(field, _)| field == "www-authenticate")
            .unwrap_or_else(|| panic!("{path} answered no challenge: {headers:?}"));
        assert!(challenge.1.contains("Basic"), "got: {challenge:?}");
    }
}

#[test]
fn the_instructions_page_is_served_without_credentials() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let (status, body, headers) = server.pull_as(metsuke_server::instructions::PATH, None);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        String::from_utf8_lossy(&body),
        metsuke_server::instructions::page()
    );
    let content_type = headers
        .iter()
        .find(|(field, _)| field == "content-type")
        .unwrap_or_else(|| panic!("no content type: {headers:?}"));
    assert!(content_type.1.starts_with("text/html"), "{content_type:?}");
}

#[test]
fn the_instructions_page_takes_only_get() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let posted = agent()
        .post(format!(
            "{}{}",
            server.base(),
            metsuke_server::instructions::PATH
        ))
        .send(&b""[..])
        .unwrap();
    assert_eq!(posted.status().as_u16(), 405);
}

#[test]
fn a_wrong_developer_password_is_refused_on_both_routes() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let user = developer_config(server.dir.path()).user;
    for path in [
        metsuke_server::http::SUBMISSIONS_PATH.to_string(),
        format!("{}?key=anything", metsuke_server::http::OBJECT_PATH),
    ] {
        let (status, body, _) = server.pull_as(&path, Some((&user, "not the password")));
        assert_eq!(status, 401, "{path}");
        assert_eq!(
            String::from_utf8_lossy(&body),
            metsuke_server::http::UNAUTHORIZED_BODY,
            "{path} told the client the password was the wrong half"
        );
    }
}

/// The listing answers off the archive, filtered as the request asked. The key
/// is time-major, so a prefix names a day and `after` is the sync cursor: two
/// pools upload, and one page carries all of them in receipt order.
#[test]
fn the_listing_answers_the_archive_filtered_by_prefix_and_after() {
    let one = test_key();
    let two = other_key();
    let server = Server::start(&[pool_of(&one), pool_of(&two)]);
    for (key, counter) in [(&one, 1), (&one, 2), (&two, 1)] {
        assert_eq!(server.post(key, &envelope_now(key, counter)).0, 200);
    }

    let whole = listing(&server, metsuke_server::http::SUBMISSIONS_PATH);
    let keys: Vec<String> = whole["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys.len(), 3, "got: {keys:?}");
    assert_eq!(whole["truncated"], false);

    // The day the server received them, which every one of the three shares.
    let prefix = keys[0]
        .rsplit_once('/')
        .map(|(folder, _)| format!("{folder}/"))
        .expect("an object key names a day folder");
    let today = format!(
        "{}?prefix={}",
        metsuke_server::http::SUBMISSIONS_PATH,
        urlencoded(&prefix)
    );
    assert_eq!(listed_keys(&server, &today).len(), 3);

    let after = format!("{today}&after={}", urlencoded(&keys[1]));
    assert_eq!(
        listed_keys(&server, &after),
        vec![keys[2].clone()],
        "the cursor key itself is behind the page it names"
    );
}

/// A listing at the configured bound says so, or a developer reads a page as
/// the whole archive.
#[test]
fn a_listing_at_the_row_bound_is_reported_as_truncated() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], |config, dir| {
        config.developer = developer_toml_with_rows(dir, 1);
    });
    for counter in [1, 2] {
        assert_eq!(server.post(&key, &envelope_now(&key, counter)).0, 200);
    }

    let page = listing(&server, metsuke_server::http::SUBMISSIONS_PATH);
    assert_eq!(page["keys"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], true);
}

/// An archive that cannot be listed is a 503 the client may retry, and its own
/// error names the store — which is the operator's to see, not the client's.
#[test]
fn a_listing_over_an_archive_that_will_not_answer_is_unavailable() {
    let key = test_key();
    let server = Server::start_with(&[pool_of(&key)], |config, dir| {
        // A regular file where the archive expects a directory root: listing
        // it fails, where a root that is merely absent is an empty archive.
        let path = dir.join("archive-is-a-file");
        std::fs::write(&path, b"not a directory").unwrap();
        config.archive = filesystem_archive(&path);
    });

    let (status, body, _) = server.pull(metsuke_server::http::SUBMISSIONS_PATH);

    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    assert!(
        !String::from_utf8_lossy(&body).contains("archive-is-a-file"),
        "the store's own error is the operator's to see, not the client's"
    );
}

/// The download hands back the archived object unchanged: a developer verifies
/// the pool's signature over exactly these bytes (ADR 0005).
#[test]
fn an_object_downloads_byte_for_byte() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let envelope = envelope_now(&key, 5);
    assert_eq!(server.post(&key, &envelope).0, 200);
    let (wire_bytes, _) = seal(&key, &envelope);
    let object_key = only_object_key(&server);

    let (status, body, _) = server.pull(&format!(
        "{}?key={}",
        metsuke_server::http::OBJECT_PATH,
        urlencoded(&object_key)
    ));

    assert_eq!(status, 200);
    assert_eq!(body, wire_bytes, "the download must be what was archived");
}

/// A key nothing stored is the archive's own 404, not an empty body a
/// developer would take for an empty submission.
#[test]
fn an_object_the_archive_does_not_hold_is_not_found() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let never_stored = stored_submission(
        &key,
        object_name(
            &key,
            OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap(),
            metsuke_server::archive::Kind::Metrics,
        ),
        seal(&key, &envelope_now(&key, 99)).1,
        b"",
    )
    .object_key();

    let (status, _, _) = server.pull(&format!(
        "{}?key={}",
        metsuke_server::http::OBJECT_PATH,
        urlencoded(&never_stored)
    ));

    assert_eq!(status, 404);
}

/// A download naming no object at all: the mistake worth its own message,
/// because an empty `key` would otherwise read as "no such object".
#[test]
fn a_download_without_a_key_names_the_field() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let (status, body, _) = server.pull(metsuke_server::http::OBJECT_PATH);
    assert_eq!(status, 400);
    assert!(
        String::from_utf8_lossy(&body).contains(metsuke_server::http::KEY_FIELD),
        "got: {}",
        String::from_utf8_lossy(&body)
    );
}

/// The key of the one object the server has stored. Read off the listing
/// because the id in it is the server's, stamped at receipt.
fn only_object_key(server: &Server) -> String {
    match listed_keys(server, metsuke_server::http::SUBMISSIONS_PATH).as_slice() {
        [key] => key.clone(),
        other => panic!("expected one stored object, got {other:?}"),
    }
}

/// The keys one page of the listing carries.
fn listed_keys(server: &Server, path: &str) -> Vec<String> {
    listing(server, path)["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_string())
        .collect()
}

/// One page of the listing, parsed. Authenticated as the configured account,
/// so a 401 here is a failure of the test's own credentials.
fn listing(server: &Server, path: &str) -> serde_json::Value {
    let (status, body, _) = server.pull(path);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

/// Percent-encode what a key or prefix holds that a query field must not:
/// `/` alone, since an object key is otherwise unreserved characters
/// (`ObjectName::to_key`).
fn urlencoded(value: &str) -> String {
    value.replace('/', "%2F")
}

/// The method check must not answer before the credential one: a 405 to an
/// unauthenticated client confirms the route exists.
#[test]
fn a_wrong_method_on_a_developer_route_is_refused_as_unauthenticated() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let mut response = agent()
        .post(format!(
            "{}{}",
            server.base(),
            metsuke_server::http::SUBMISSIONS_PATH
        ))
        .send(&b""[..])
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
    assert!(
        !response
            .body_mut()
            .read_to_string()
            .unwrap()
            .contains("GET"),
        "a 401 must not say which method the route takes"
    );
}

/// Every credential file that yields no password stops startup, because
/// `user:` — half of it already in the public config — would otherwise
/// authorize every pull. `None` is the file never written at all.
#[test]
fn a_developer_password_file_with_no_password_in_it_stops_startup() {
    for (written, reason) in [
        (Some(""), "empty"),
        (Some("\n"), "empty"),
        (None, "No such file"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = server_toml(dir.path(), &[pool_of(&test_key())]);
        let path = config.write(dir.path());
        let password_file = developer_config(dir.path()).password_file;
        match written {
            Some(contents) => std::fs::write(password_file.as_path(), contents).unwrap(),
            None => std::fs::remove_file(password_file.as_path()).unwrap(),
        }

        let (exit, stderr) = refuses_to_start(&path);

        assert!(!exit.success(), "{written:?} must not start");
        assert!(
            stderr.contains(&password_file.as_path().display().to_string()),
            "{written:?}: the refusal must name the file, got {stderr}"
        );
        assert!(stderr.contains(reason), "{written:?}: {stderr}");
    }
}

/// Spawn the server expecting it not to reach its listener, and hand back how
/// it exited. Waiting on a deadline rather than on `output()`: a server that
/// *does* start serves forever, and a regression has to fail this test rather
/// than hang the suite.
fn refuses_to_start(config: &std::path::Path) -> (std::process::ExitStatus, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args(["--config", config.to_str().unwrap()])
        .envs(TEST_CREDENTIALS)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        if std::time::Instant::now() >= deadline {
            // Killed before the panic, or a serving process outlives the suite.
            let _ = child.kill();
            let _ = child.wait();
            panic!("the server started instead of refusing");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (exit, stderr)
}
