//! Binary-level tests (ticket metsuke-4zo.5): the built `metsuke` binary
//! spawned as an SPO would run it. Covers the `run()` wiring the library
//! tests can't reach, in particular that the `--signing-key` flag really
//! beats the config path, which a swapped argument pair would invert
//! without failing any type check.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use metsuke_wire::envelope::PoolId;
use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;
use metsuke_wire::hex;
use support::{TEST_LIMITS, attestation_of, sh_stand_in, test_key};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

/// A real node's trace stream, fed to the recorder on stdin as the node would.
const RECORDED_TRACES: &str = include_str!("fixtures/recordings/leios-node-traces.log");

/// The name the config gives this machine, and the id it folds down to
/// (`AgentId::slugify`): what the startup line and every header must carry.
const AGENT_NAME: &str = "Test_Relay 1";
const AGENT_ID: &str = "test-relay-1";

fn write_config(
    dir: &tempfile::TempDir,
    server_uri: &str,
    signing_key_line: &str,
) -> std::path::PathBuf {
    write_config_for(
        dir,
        server_uri,
        signing_key_line,
        PoolId::from_cold_key(&test_key().verifying_key()),
        AGENT_NAME,
    )
}

fn write_config_for(
    dir: &tempfile::TempDir,
    server_uri: &str,
    signing_key_line: &str,
    pool_id: PoolId,
    agent_name: &str,
) -> std::path::PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            r#"
            pool_id = "{}"
            agent_id = "{agent_name}"
            metrics_url = "{server_uri}/metrics"
            upload_url = "{server_uri}/v1/submit"
            scrape_interval_secs = 1
            upload_interval_secs = 1
            sntp_servers = []
            spool_path = "{}"
            {signing_key_line}
            "#,
            pool_id.to_bech32(),
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

// The `identity::check_pool_id` refusal reaches the exit code: a mismatch stops
// the process rather than surfacing as a rejected upload later.
#[test]
fn binary_refuses_a_pool_id_the_signing_key_does_not_hash_to() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = write_test_envelope(&dir);
    let other = PoolId::from_cold_key(
        &metsuke_wire::envelope::SigningKey::from_bytes(&[9u8; 32]).verifying_key(),
    );
    let config = write_config_for(&dir, "http://127.0.0.1:9", "", other, AGENT_NAME);

    let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .args(["--config", config.to_str().unwrap()])
        .args(["--signing-key", key_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mine = PoolId::from_cold_key(&test_key().verifying_key()).to_bech32();
    assert!(
        stderr.contains(&other.to_bech32()) && stderr.contains(&mine),
        "startup failure must name both pool ids, got: {stderr}"
    );
}

// The `[log]` startup path end to end: a journalctl that never follows ends the
// start (`StartupError::TraceSource`), rather than leaving the agent up.
#[test]
fn binary_refuses_a_journalctl_that_cannot_read_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = write_test_envelope(&dir);
    let config = write_config(&dir, "http://127.0.0.1:9", "");
    let journalctl = sh_stand_in(&dir, "refused-journalctl", "exit 13");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[log]\nsource = \"journald\"\njournal_unit = \"cardano-node\"\n\
         journalctl_path = \"{}\"\nstart_grace_secs = 1\n",
        journalctl.display(),
    ));
    std::fs::write(&config, text).unwrap();

    // Bounded rather than waited on: the failure this covers is an agent that
    // stays up collecting nothing, which a plain `output()` would hang on.
    let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .args(["--config", config.to_str().unwrap()])
        .args(["--signing-key", key_path.to_str().unwrap()])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let status = exited_within(&mut child, Duration::from_secs(30));

    assert!(!status.success());
    let mut said = String::new();
    std::io::Read::read_to_string(&mut stderr, &mut said).unwrap();
    assert!(
        said.contains("journal") && said.contains("13"),
        "startup failure must name the journal and journalctl's status, got: {said}"
    );
}

/// What the onboarding page's check step shows, recorded off a real run rather
/// than written by hand. `METSUKE_RERECORD=1 cargo test --test binary` rewrites
/// it, and `metsuke-server` carries it whole.
const JOURNAL_RECORDING: &str = "tests/fixtures/recordings/agent-journal.log";

/// The endpoint the shipped config names. The recorder binds an ephemeral port,
/// so the one it scraped is rewritten to this before the recording is kept: the
/// port is the only thing about that line an operator cannot reproduce, and the
/// shipped one is what their own config will make it say.
const SHIPPED_METRICS_URL: &str = "http://127.0.0.1:12798/metrics";

/// A line as journalctl shows it. systemd reads the priority prefix off the
/// front and does not pass it on, so an operator never sees one.
fn without_priority(line: &str) -> &str {
    [
        metsuke_wire::journal::INFO,
        metsuke_wire::journal::WARNING,
        metsuke_wire::journal::ERR,
    ]
    .iter()
    .find_map(|prefix| line.strip_prefix(prefix))
    .unwrap_or(line)
}

/// The line with every number and digest blanked. What a second run repeats is
/// the words and their order; the build, the counter, the digest, how much each
/// submission carried and its size all follow the run. Word by word rather than
/// field by field, because the two accepted lines carry different fields.
///
/// A word is blanked only if it is nothing but hex digits and dots, so a pool
/// id, a URL and every ordinary word survive.
fn shape(line: &str) -> String {
    line.split_whitespace()
        .map(|word| {
            match word
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'.')
            {
                true => "#",
                false => word,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The check step's promise: an agent that is working says so, in lines an
/// operator can match against their own journal. Recorded from the built
/// binary, so the page cannot show a line the agent does not print.
#[tokio::test]
async fn the_journal_lines_the_page_shows_are_the_ones_the_agent_prints() {
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
            "latest_version": env!("CARGO_PKG_VERSION")
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let key_path = write_test_envelope(&dir);
    let config = write_config_for(
        &dir,
        &server.uri(),
        // The source the page's own try-it uses, so what is recorded is what an
        // operator following it sees. It also gives the second accepted line:
        // scrapes and trace lines are separate streams and a tick sends one
        // submission for each.
        "[log]\nsource = \"pipe\"",
        PoolId::from_cold_key(&test_key().verifying_key()),
        // Not the slugification fixture the other tests use: this name reaches
        // an operator's screen, and `relay-1` is what one of theirs looks like.
        "relay-1",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .args(["--config", config.to_str().unwrap()])
        .args(["--signing-key", key_path.to_str().unwrap()])
        .stdin(Stdio::piped())
        // Written through unchanged, which is the node's output and not
        // something this test reads.
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().expect("stderr was piped");

    // The node's share of the pipeline: the recorded stream, then a handle held
    // open. Closing it is EOF, which the agent stops on, and it has to still be
    // running when the upload tick fires.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    std::io::Write::write_all(&mut stdin, RECORDED_TRACES.as_bytes()).unwrap();
    std::io::Write::flush(&mut stdin).unwrap();

    // Up to the accepted trace-line submission, which is the last line the step
    // promises. Reading is what bounds this: the agent uploads on its first
    // tick, and a run that never got there blocks and fails the suite's clock
    // rather than passing on the lines it had.
    let mut reader = std::io::BufReader::new(&mut stderr);
    let mut recorded = String::new();
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "the agent stopped before it accepted both submissions, said: {recorded}"
        );
        let line = without_priority(&line);
        recorded.push_str(line);
        if line.contains("trace lines,") {
            break;
        }
    }
    drop(stdin);
    child.kill().unwrap();
    child.wait().unwrap();
    let recorded = recorded.replace(&format!("{}/metrics", server.uri()), SHIPPED_METRICS_URL);

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNAL_RECORDING);
    if std::env::var_os("METSUKE_RERECORD").is_some() {
        std::fs::write(&path, &recorded).unwrap();
        return;
    }
    let shipped = std::fs::read_to_string(&path).expect("the recording is committed");
    let blank = |text: &str| text.lines().map(shape).collect::<Vec<_>>().join("\n");
    assert_eq!(
        blank(&recorded),
        blank(&shipped),
        "the agent no longer prints what the page shows; re-record with METSUKE_RERECORD=1"
    );
}

/// The status of a process that had to end on its own. One still running at
/// the deadline is killed, and the test fails.
fn exited_within(child: &mut std::process::Child, within: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    panic!("the binary was still running after {within:?}");
}

// The whole wiring: config + flag in, a verifiable upload out. The config's
// signing_key points at a path that does not exist, so this passes only
// when the flag wins the precedence. A swapped `resolve_signing_key`
// argument pair fails at startup instead.
#[tokio::test]
async fn binary_uploads_a_submission_signed_by_the_flag_key() {
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
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().expect("stderr was piped");

    // The first thing it says is its build, before anything that can fail;
    // the second is who it is, with the configured name folded into an id.
    let mut reader = std::io::BufReader::new(&mut stderr);
    let mut build = String::new();
    reader.read_line(&mut build).unwrap();
    assert!(
        build.contains(env!("CARGO_PKG_VERSION")),
        "the first line must name the build, got: {build}"
    );
    let mut startup = String::new();
    reader.read_line(&mut startup).unwrap();
    assert!(
        startup.contains(AGENT_ID),
        "the startup line must name the resolved agent id, got: {startup}"
    );

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
    let vkey_bytes = hex::decode::<32>(header(HEADER_VKEY)).unwrap();
    let sig_bytes = hex::decode::<64>(header(HEADER_SIGNATURE)).unwrap();
    let opened = metsuke_wire::envelope::open(
        &attestation_of(&vkey_bytes, &sig_bytes),
        &post.body,
        TEST_LIMITS,
    )
    .unwrap();
    assert_eq!(
        opened.provenance.pool_id,
        PoolId::from_cold_key(&test_key().verifying_key())
    );
    assert_eq!(opened.provenance.agent_id.as_str(), AGENT_ID);
    // The flag key signed it: `open` verified against the header vkey,
    // which must be the flag key's.
    assert_eq!(vkey_bytes, test_key().verifying_key().to_bytes());
}

/// The version an operator reads off a build is the one the crate shipped, so
/// this compares the binary's answer with the manifest's value rather than a
/// string written here.
#[test]
fn version_is_printed_on_its_own_and_names_the_crates_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
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

/// Asked for, so it is the answer: stdout, exit zero, and the usage the
/// parser refuses with. Neither form may need a readable config, which is
/// why this passes no `--config` at all.
#[test]
fn help_is_printed_on_stdout_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
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
            metsuke::cli::USAGE
        );
    }
}

/// The run that most needs a build named is the one that failed, so the line
/// comes before anything that can stop the start.
#[test]
fn a_start_that_stops_on_its_config_still_names_the_build() {
    let dir = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
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

/// A mistyped flag points at --help rather than restating the usage, so the
/// error stays one line an operator reads.
#[test]
fn an_unknown_flag_points_at_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke"))
        .arg("--sining-key")
        .output()
        .expect("the binary runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--sining-key") && stderr.contains(metsuke::cli::HELP_HINT),
        "got: {stderr}"
    );
}
