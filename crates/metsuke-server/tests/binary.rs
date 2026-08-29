//! Binary-level tests: the built `metsuke-server` spawned as an operator
//! would run it, answered over a real socket. Covers the `run()` wiring, and
//! the status each outcome earns, which is only observable on the wire.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use metsuke_wire::envelope::{Ack, Envelope, HEADER_SIGNATURE, HEADER_VKEY, PoolId, SigningKey};
use metsuke_wire::hex;
use time::OffsetDateTime;

mod support;
use metsuke_server::config::{HttpConfig, IngestConfig};
use support::{
    DEVELOPER_PASSWORD, ServerToml, developer_config, developer_toml_with_rows, envelope_at,
    example_s3_archive, filesystem_archive, http_toml, ingest_toml, nonzero_u32, nonzero_u64,
    object_name, other_key, permissive_config, permissive_http, pool_of, seal, server_toml,
    stored_submission, test_key,
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
    /// Everything the server has logged since it named its address.
    logged: Arc<std::sync::Mutex<String>>,
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
        let logged = Arc::new(std::sync::Mutex::new(String::new()));
        let url = bound_url(child.stderr.take().unwrap(), Arc::clone(&logged));
        Server {
            child,
            url,
            dir,
            logged,
        }
    }

    /// What the server has logged so far, for an outcome that reaches no
    /// client: the answer's head went out before the failure was known.
    fn logged(&self) -> String {
        self.logged
            .lock()
            .expect("no panic holds this lock")
            .clone()
    }

    fn archive_root(&self) -> std::path::PathBuf {
        self.dir.path().join("archive")
    }

    /// `host:port`, for a test that opens its own socket.
    fn address(&self) -> String {
        self.url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .expect("the startup line names host:port")
            .to_string()
    }

    /// The high-water mark of the serving process's virtual size, in
    /// kilobytes. The peak and not the current size: an allocation made and
    /// freed while answering is gone from `VmSize` by the time the answer
    /// arrives, so reading that would pass on the very code the measurement in
    /// metsuke-a3a is about.
    fn peak_virtual_kb(&self) -> u64 {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap();
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmPeak:"))
            .and_then(|value| value.split_whitespace().next())
            .expect("a live process reports VmPeak")
            .parse()
            .unwrap()
    }

    /// How many sockets the serving process holds, the listener excluded: a
    /// connection still in the kernel's backlog is not one of them.
    fn connections(&self) -> usize {
        let sockets = std::fs::read_dir(format!("/proc/{}/fd", self.child.id()))
            .expect("a live process has a descriptor table")
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter(|target| target.to_string_lossy().starts_with("socket:"))
            .count();
        // Less the listener, which is open for the process's whole life.
        sockets - 1
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

    /// The same POST, put in flight and left there. Returns as soon as the
    /// request is handed to a thread, so a test can overlap it with whatever
    /// it claims does not hold it up.
    fn posting(&self, key: &SigningKey, envelope: &Envelope) -> Upload {
        let url = self.url.clone();
        let (wire_bytes, signature) = seal(key, envelope);
        let vkey = hex::encode(key.verifying_key().as_bytes());
        let signature = hex::encode(&signature.to_bytes());
        let (done, answered) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let status = agent()
                .post(&url)
                .header(HEADER_VKEY, &vkey)
                .header(HEADER_SIGNATURE, &signature)
                .content_type("application/json")
                .send(&wire_bytes[..])
                .map(|response| response.status().as_u16());
            let _ = done.send(status);
        });
        Upload(answered)
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

/// An upload in flight, whose answer is waited on with a deadline rather than
/// joined. The deadline is the assertion: a server that serves one connection
/// at a time still answers this upload, only after the connection ahead of it
/// has run out its own timeout, and a join cannot tell that from being served
/// straight away. The thread is left running where it does not arrive.
struct Upload(std::sync::mpsc::Receiver<Result<u16, ureq::Error>>);

impl Upload {
    fn within(self, budget: Duration) -> u16 {
        use std::sync::mpsc::RecvTimeoutError::{Disconnected, Timeout};
        match self.0.recv_timeout(budget) {
            Ok(answer) => answer.expect("the upload failed"),
            Err(Timeout) => panic!("the upload went unanswered for {budget:?}"),
            Err(Disconnected) => panic!("the upload thread died before answering"),
        }
    }
}

/// Long enough for an answer that is not being held up, short enough to be
/// under the timeout a held-up one would be answered by, which is what makes
/// it tell "served straight away" from "served once something else timed out".
/// Only sound where the call site leaves `permissive_http`'s 10 s alone.
const PROMPTLY: Duration = Duration::from_secs(2);

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
/// listener is bound. The rest of the log is drained into `logged`, which a
/// test asserting on a line the server writes while serving reads back.
fn bound_url(stderr: ChildStderr, logged: Arc<std::sync::Mutex<String>>) -> String {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "server exited before naming its address"
        );
        // Kept as it is read, not only after the address line: what a server
        // says while starting is said before it names its address, and a test
        // about a startup line would otherwise be reading an empty string.
        logged
            .lock()
            .expect("no panic holds this lock")
            .push_str(&line);
        if let Some(start) = line.find("http://") {
            // Drained in the background rather than at the end: a full stderr
            // pipe would otherwise wedge the server mid-test.
            std::thread::spawn(move || {
                let mut rest = String::new();
                while reader
                    .read_line(&mut rest)
                    .expect("the server's stderr reads")
                    > 0
                {
                    logged
                        .lock()
                        .expect("no panic holds this lock")
                        .push_str(&rest);
                    rest.clear();
                }
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

/// Named and pointed at `--help`, rather than the whole usage on top of the
/// error: the operator can ask for that, and now has to read one line.
#[test]
fn an_unknown_argument_exits_nonzero_pointing_at_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .arg("--verbose")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--verbose") && stderr.contains(metsuke_server::cli::HELP_HINT),
        "{stderr}"
    );
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
/// deletes spooled scrapes that were never written (ADR 0004).
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
/// agent acks scrapes the bucket never took (ADR 0004).
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

/// An endpoint answering the listing a bucket holding nothing really sent
/// (`list-empty.http`).
fn empty_bucket_endpoint() -> String {
    let endpoint = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", endpoint.server_addr());
    std::thread::spawn(move || {
        let empty = support::Reply::recorded("list-empty");
        for request in endpoint.incoming_requests() {
            let _ = request.respond(empty.as_response());
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

/// The one backend whose objects nobody can ever check says so at startup.
/// The pair is dropped at the moment of storing, so this line is the only
/// place an operator can learn it: no route, no audit and no consumer can
/// recover what the archive did not keep.
#[test]
fn a_filesystem_archive_says_what_it_gives_up() {
    let server = Server::start(&[pool_of(&test_key())]);

    let logged = server.logged();

    assert!(
        logged.contains("drops the key and signature"),
        "the startup log must say what this backend costs, got: {logged}"
    );
    assert!(
        logged.contains(&server.archive_root().display().to_string()),
        "and name the archive it is warning about, got: {logged}"
    );
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
/// error names the store, which is the operator's to see, not the client's.
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

    let (status, body, headers) = server.pull(&format!(
        "{}?key={}",
        metsuke_server::http::OBJECT_PATH,
        urlencoded(&object_key)
    ));

    assert_eq!(status, 200);
    assert_eq!(body, wire_bytes, "the download must be what was archived");
    // Length-delimited, never chunked: this is what `ObjectStream`'s
    // non-optional length and `ArchiveError::EndpointUnusable` are both for,
    // and it is only observable on the wire.
    assert_eq!(
        headers
            .iter()
            .find(|(field, _)| field == "content-length")
            .map(|(_, value)| value.as_str()),
        Some(wire_bytes.len().to_string().as_str()),
        "got: {headers:?}"
    );
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

/// How long a test waits on an answer before calling the server hung. Long
/// enough that a server which answers is never mistaken for one that does not,
/// short enough that a regression fails rather than wedges the suite.
const PATIENCE: Duration = Duration::from_secs(30);

/// A connection the test drives itself, for the request shapes no HTTP client
/// will produce.
struct Raw {
    stream: std::net::TcpStream,
    /// Everything read so far. A read carries whatever the server had sent,
    /// which is not bounded by the framing the caller asked for and cannot be
    /// put back, so it is kept rather than discarded. The write-timeout test
    /// counts these bytes.
    read: Vec<u8>,
}

impl Raw {
    fn connect(server: &Server) -> Raw {
        let stream = std::net::TcpStream::connect(server.address()).unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        Raw {
            stream,
            read: Vec::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.try_send(bytes).expect("the server took the request");
    }

    /// Send, handing back what went wrong rather than asserting. For a test
    /// that keeps writing to a connection the server is entitled to close
    /// under it, and which reads the close as its outcome.
    fn try_send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream
            .write_all(bytes)
            .and_then(|()| self.stream.flush())
    }

    /// Everything the server writes back, up to the close.
    fn answer(mut self) -> String {
        assert!(
            self.read_to_close(PATIENCE),
            "the server did not close: {}",
            String::from_utf8_lossy(&self.read)
        );
        String::from_utf8_lossy(&self.read).to_string()
    }

    /// Read until the answer's head is complete, and hand back its length.
    /// `None` only where `budget` passed with no head arriving, which is how a
    /// connection that was never served is told from one that was; a close or
    /// a fault mid-head is neither, and panics.
    fn until_blank_line(&mut self, budget: Duration) -> Option<usize> {
        self.stream.set_read_timeout(Some(budget)).unwrap();
        loop {
            if let Some(blank) = self.read.windows(4).position(|four| four == b"\r\n\r\n") {
                return Some(blank + 4);
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => panic!(
                    "the server closed mid-head: {}",
                    String::from_utf8_lossy(&self.read)
                ),
                Ok(read) => self.read.extend_from_slice(&chunk[..read]),
                Err(error) => match error.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => return None,
                    std::io::ErrorKind::Interrupted => continue,
                    _ => panic!("reading the answer's head: {error}"),
                },
            }
        }
    }

    /// The answer's head. For a connection the server keeps alive, where
    /// reading to the close would wait out `idle_timeout` instead.
    fn answer_head(&mut self) -> String {
        self.head_within(PATIENCE).expect("the server answered")
    }

    /// The same, giving up after `budget`.
    fn head_within(&mut self, budget: Duration) -> Option<String> {
        let head = self.until_blank_line(budget)?;
        Some(String::from_utf8_lossy(&self.read[..head]).to_string())
    }

    /// One whole answer, head and `Content-Length` body. Framed off the head
    /// rather than taken from one `read`, so nothing here turns on hyper
    /// writing both in one segment.
    fn one_answer(&mut self) -> String {
        let head = self
            .until_blank_line(PATIENCE)
            .expect("the server answered");
        let declared: usize = String::from_utf8_lossy(&self.read[..head])
            .lines()
            .find_map(|line| {
                line.to_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse()
                    .ok()
            })
            .expect("the answer states its length");
        while self.read.len() < head + declared {
            self.read_more();
        }
        String::from_utf8_lossy(&self.read[..head + declared]).to_string()
    }

    fn read_more(&mut self) {
        let mut chunk = [0u8; 8192];
        match self.stream.read(&mut chunk).expect("the server answered") {
            0 => panic!(
                "the server closed mid-answer: {}",
                String::from_utf8_lossy(&self.read)
            ),
            read => self.read.extend_from_slice(&chunk[..read]),
        }
    }

    /// Read until the server closes, or until `budget` passes with nothing
    /// arriving. Says which of the two happened; what arrived is in `read`.
    fn read_to_close(&mut self, budget: Duration) -> bool {
        self.stream.set_read_timeout(Some(budget)).unwrap();
        let mut chunk = [0u8; 8192];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return true,
                Ok(read) => self.read.extend_from_slice(&chunk[..read]),
                Err(error) => match error.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => return false,
                    std::io::ErrorKind::Interrupted => continue,
                    _ => panic!("reading the answer: {error}"),
                },
            }
        }
    }

    /// Read until this connection has carried `bytes` in total, or until the
    /// server closes first. Says how many it carried. For an answer whose
    /// framing says it is complete, where reading on would only wait out the
    /// idle timeout.
    fn up_to(&mut self, total: usize) -> usize {
        self.stream.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut chunk = [0u8; 8192];
        while self.read.len() < total {
            match self.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.read.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("reading the answer: {error}"),
            }
        }
        self.read.len()
    }

    /// Everything this connection ever carried, counting what earlier reads
    /// already took, and whether the server closed rather than running into
    /// `budget` still holding it.
    fn delivered(&mut self, budget: Duration) -> (bool, usize) {
        let closed = self.read_to_close(budget);
        (closed, self.read.len())
    }
}

/// The head of a submission, with the two ADR-0001 headers well formed and
/// `framing` saying how the body that follows is delimited. What follows it is
/// the test's to send, or not to.
fn submission_head(key: &SigningKey, framing: &str) -> Vec<u8> {
    let (_, signature) = seal(key, &envelope_now(key, 1));
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         {HEADER_VKEY}: {vkey}\r\n\
         {HEADER_SIGNATURE}: {signature}\r\n\
         {framing}\r\n\r\n",
        path = metsuke_server::http::SUBMIT_PATH,
        vkey = hex::encode(key.verifying_key().as_bytes()),
        signature = hex::encode(&signature.to_bytes()),
    )
    .into_bytes()
}

/// metsuke-a3a's acceptance criterion, as a test: a request declaring four
/// gigabytes and sending none of them costs the server neither the memory it
/// named nor the ingest path.
#[test]
fn a_declared_body_far_over_the_cap_neither_allocates_nor_stalls() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let before = server.peak_virtual_kb();

    let mut lying = Raw::connect(&server);
    lying.send(&submission_head(&key, "Content-Length: 4000000000"));
    let answer = lying.answer();

    assert!(answer.starts_with("HTTP/1.1 413"), "got: {answer:.80}");
    let grew = server.peak_virtual_kb().saturating_sub(before);
    assert!(
        grew < 64 * 1024,
        "the declared 4 GB must not be allocated, VmPeak grew {grew} kB"
    );
    // And the pool behind it is served, because nothing was waiting on that
    // body.
    assert_eq!(server.post(&key, &envelope_now(&key, 2)).0, 200);
}

/// The other half of metsuke-a3a, against `read_timeout_ms`. The body has to
/// keep arriving and the clock is what says it did: a body that merely stops
/// would be refused by a narrower bound too.
#[test]
fn a_body_that_keeps_trickling_is_cut_off_at_the_read_timeout() {
    let key = test_key();
    // What the bound has to be wide enough for is `every` landing inside it
    // repeatedly: a trickle whose interval overshoots the bound is a stalled
    // body, which is the case this one exists to be distinguished from. Ten
    // intervals to the bound leaves that margin to the scheduler.
    let bound = Duration::from_millis(1000);
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.http = http_toml(&HttpConfig {
            read_timeout_ms: nonzero_u64(bound.as_millis() as u64),
            ..permissive_http()
        });
    });
    let every = Duration::from_millis(100);
    // Past the bound even if every sleep overshoots, so the trickle is always
    // cut rather than running out of attempts.
    let attempts = 30;

    let mut trickling = Raw::connect(&server);
    let started = std::time::Instant::now();
    trickling.send(&submission_head(&key, "Content-Length: 1000"));
    // `try_send`, because the server closing mid-trickle is the outcome under
    // test.
    let mut landed = 0;
    let mut cut_after = None;
    for _ in 0..attempts {
        if trickling.try_send(b"x").is_err() {
            cut_after = Some(started.elapsed());
            break;
        }
        landed += 1;
        std::thread::sleep(every);
    }

    // Elapsed time rather than a count of bytes: `sleep` overshoots, so a
    // loaded run eats a count's margin and not a clock's
    // (`config::HttpConfig::read_timeout_ms`).
    let cut_after =
        cut_after.unwrap_or_else(|| panic!("the trickle was never cut, {landed} bytes landed"));
    assert!(
        cut_after >= bound,
        "the trickle was cut after {cut_after:?}, inside the {bound:?} bound"
    );
    let answer = trickling.answer();
    assert!(answer.starts_with("HTTP/1.1 408"), "got: {answer:.80}");
}

/// The half of the cap no declared length can catch (`serve::bounded`).
#[test]
fn a_chunked_body_past_the_cap_is_refused_as_it_arrives() {
    let key = test_key();
    let max = 64;
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.ingest = ingest_toml(&IngestConfig {
            max_body_bytes: nonzero_u64(max),
            ..permissive_config(&[pool_of(&key)])
        });
    });

    let mut chunked = Raw::connect(&server);
    chunked.send(&submission_head(&key, "Transfer-Encoding: chunked"));
    let chunk = vec![b'x'; (max * 2) as usize];
    chunked.send(format!("{:x}\r\n", chunk.len()).as_bytes());
    chunked.send(&chunk);
    // No terminating chunk: the body never ends, so only a cap that fires on
    // the bytes as they arrive can answer at all. A terminated body would be
    // refused identically by the intake's own cap, and the 413 would not say
    // which layer produced it.
    chunked.send(b"\r\n");

    let answer = chunked.one_answer();
    assert!(answer.starts_with("HTTP/1.1 413"), "got: {answer:.80}");
    assert!(
        answer.contains(&format!("{max} byte limit")),
        "the refusal must name the cap, got: {answer:.200}"
    );
}

/// An object bigger than any socket buffer, so a download of it genuinely
/// blocks on a client that has stopped reading. Written straight into the
/// archive: what these tests are about is the download, not what an upload may
/// weigh. Returns its key and its size.
fn big_object(server: &Server, key: &SigningKey) -> (String, usize) {
    let stored = object_name(
        key,
        OffsetDateTime::now_utc(),
        metsuke_server::archive::Kind::Logs,
    )
    .to_key();
    let bytes = 64 * 1024 * 1024;
    let path = server.archive_root().join(&stored);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, vec![0u8; bytes]).unwrap();
    (stored, bytes)
}

/// A download of `stored` whose answer has started and which nothing is
/// reading past the head.
fn stalled_download(server: &Server, stored: &str) -> (Raw, usize) {
    let mut download = Raw::connect(server);
    let credentials = base64::engine::general_purpose::STANDARD.encode(format!(
        "{}:{DEVELOPER_PASSWORD}",
        developer_config(server.dir.path()).user
    ));
    download.send(
        &format!(
            "GET {path}?{field}={key} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Authorization: Basic {credentials}\r\n\r\n",
            path = metsuke_server::http::OBJECT_PATH,
            field = metsuke_server::http::KEY_FIELD,
            key = urlencoded(stored),
        )
        .into_bytes(),
    );
    let started = download.answer_head();
    assert!(started.starts_with("HTTP/1.1 200"), "got: {started:.80}");
    (download, started.len())
}

/// An object that shrinks after its length has been read: the head is out
/// under the old length, so the client is handed a body that does not fill it
/// and the log line is the only account there is.
///
/// Filesystem archive only. An S3 archive's reader fails on a short
/// length-delimited body before it can end, so it takes the `stopped` line
/// instead.
#[test]
fn a_download_of_an_object_that_shrank_is_logged_as_short() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let (stored, size) = big_object(&server, &key);
    // The answer's head is out and the stream is blocked on a client that is
    // not reading, so the server is at most a socket buffer and one chunk into
    // an 8 MiB object, nowhere near the 6 MiB mark this truncates to.
    let (mut download, _) = stalled_download(&server, &stored);
    let shrunk = size / 4;
    std::fs::OpenOptions::new()
        .write(true)
        .open(server.archive_root().join(&stored))
        .unwrap()
        .set_len(shrunk as u64)
        .unwrap();

    let (_, delivered) = download.delivered(PATIENCE);

    assert!(
        delivered < size,
        "the body must be short of the declared {size}, got {delivered}"
    );
    let logged = server.logged();
    assert!(
        logged.contains(&stored) && logged.contains("short of"),
        "the short read must be logged against its key, got: {logged}"
    );
}

/// The same window in the other direction: an object that grew after its
/// length was read is cut, and the cut is logged against its key.
#[test]
fn a_download_of_an_object_that_grew_is_logged_as_over_length() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let (stored, size) = big_object(&server, &key);
    let (mut download, head) = stalled_download(&server, &stored);
    std::fs::OpenOptions::new()
        .append(true)
        .open(server.archive_root().join(&stored))
        .unwrap()
        .write_all(&vec![b'x'; size / 4])
        .unwrap();

    let body = download.up_to(head + size) - head;

    // The client cannot tell (`serve::streamed`), which is why the log line is
    // the only account of it.
    assert_eq!(body, size, "the answer fills its declared length");
    let logged = server.logged();
    assert!(
        logged.contains(&stored) && logged.contains("grew past"),
        "the over-length read must be logged against its key, got: {logged}"
    );
}

/// A developer that stops reading mid-download holds its own connection and
/// nothing else: every connection is its own task, so a stalled answer is not
/// the ingest path's problem (metsuke-4zo.73).
#[test]
fn a_download_left_unread_stalls_no_upload() {
    let key = test_key();
    let server = Server::start(&[pool_of(&key)]);
    let (stored, _) = big_object(&server, &key);

    let _download = stalled_download(&server, &stored);

    let ingest = server.posting(&key, &envelope_now(&key, 1));
    assert_eq!(ingest.within(PROMPTLY), 200);
}

/// And it does not hold it indefinitely: `write_timeout_ms` is what takes the
/// slot back from a client that stopped reading.
#[test]
fn a_download_nobody_reads_is_cut_off_at_the_write_timeout() {
    let key = test_key();
    let bound = Duration::from_millis(300);
    let server = Server::start_with(&[pool_of(&key)], |config, _| {
        config.http = http_toml(&HttpConfig {
            write_timeout_ms: nonzero_u64(bound.as_millis() as u64),
            ..permissive_http()
        });
    });
    let (stored, size) = big_object(&server, &key);
    let (mut download, _) = stalled_download(&server, &stored);

    // The bound is a clock with nothing to wait on but itself. Reading before
    // it expires would unblock the write and let the download finish, which is
    // exactly the outcome this has to tell apart. Twice the bound, so the
    // timeout has fired even where the sleep returns early.
    std::thread::sleep(bound * 2);
    let (closed, delivered) = download.delivered(PATIENCE);

    assert!(closed, "the server kept the connection open");

    // Half, not "less than all": what the head's read carried counts too, so a
    // download that was never cut off can still be a few bytes short of the
    // object.
    assert!(
        delivered < size / 2,
        "the download must have been cut off, got {delivered} of {size} bytes"
    );
}

/// Past `max_concurrent_requests` a client waits in the backlog rather than
/// costing the server a task. At a cap of one, a connection part-way through
/// its head holds the only slot, and the next connection is answered exactly
/// when that one lets go.
#[test]
fn a_connection_past_the_concurrency_cap_waits_for_a_slot() {
    let server = Server::start_with(&[pool_of(&test_key())], |config, _| {
        config.http = http_toml(&HttpConfig {
            max_concurrent_requests: nonzero_u32(1),
            ..permissive_http()
        });
    });
    let page = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n",
        path = metsuke_server::instructions::PATH,
    );

    // Head started and not finished, so the slot is taken and stays taken.
    let mut holding = Raw::connect(&server);
    holding.send(b"GET / HTTP/1.1\r\nHost: localhost\r\n");
    let mut waiting = Raw::connect(&server);
    waiting.send(page.as_bytes());

    // Proving an absence, so this is a wait with nothing to shorten it. It only
    // has to outlast the microseconds a broken semaphore would serve in, and it
    // has to stay well under `permissive_http`'s idle bound. Past that the
    // holding connection lets the slot go on its own and the absence means
    // nothing.
    assert!(
        waiting.head_within(Duration::from_millis(500)).is_none(),
        "the second connection was served while the cap was full"
    );
    // And it is waiting in the kernel's backlog, not in the process
    // (`serve::accept`): taking the permit after accept(2) would leave the
    // server holding both sockets.
    assert_eq!(
        server.connections(),
        1,
        "the waiting connection was accepted rather than left in the backlog"
    );
    drop(holding);
    assert!(
        waiting.answer_head().starts_with("HTTP/1.1 200"),
        "the slot was not handed on when the first connection closed"
    );
}

/// A connection that asks for nothing is closed rather than kept: without
/// `idle_timeout_ms` it would hold its slot until the client felt like
/// giving it back.
#[test]
fn a_connection_that_sends_no_request_is_closed_at_the_idle_timeout() {
    let server = Server::start_with(&[pool_of(&test_key())], |config, _| {
        config.http = http_toml(&HttpConfig {
            idle_timeout_ms: nonzero_u64(200),
            ..permissive_http()
        });
    });

    let mut silent = Raw::connect(&server);

    assert!(
        silent.delivered(Duration::from_secs(10)).0,
        "the server kept a connection that never sent a request"
    );
}

/// Every credential file that yields no password stops startup, because
/// `user:`, half of it already in the public config, would otherwise
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

/// The version a deploy is named by is the one the crate shipped, so this
/// compares the binary's answer with the manifest's value rather than a
/// string written here.
#[test]
fn version_is_printed_on_its_own_and_names_the_crates_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .arg("--version")
        .output()
        .expect("the binary runs");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

/// Asked for, so it is the answer: stdout, exit zero, and no config read,
/// which is why this passes none.
#[test]
fn help_is_printed_on_stdout_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
            .arg(flag)
            .output()
            .expect("the binary runs");

        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            metsuke_server::cli::USAGE
        );
    }
}

/// The run that most needs a build named is the one that failed, so the line
/// comes before anything that can stop the start.
#[test]
fn a_start_that_stops_on_its_config_still_names_the_build() {
    let dir = tempfile::tempdir().expect("a temp dir");

    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-server"))
        .args([
            "--config",
            &dir.path().join("absent.toml").display().to_string(),
        ])
        .output()
        .expect("the binary runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(env!("CARGO_PKG_VERSION")), "got: {stderr}");
}
