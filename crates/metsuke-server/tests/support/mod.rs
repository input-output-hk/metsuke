//! Helpers for the server tests: keys, an envelope, the sealed form of one,
//! and the stores a test runs against.
//!
//! Every test target compiles the whole module, so a helper only one of them
//! needs reads as dead code in the others.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use metsuke_server::applications::ApplicationCode;
use metsuke_server::archive::{
    ArchiveError, Bytes, Kind, List, ObjectName, ObjectStream, Page, Store, StoredSubmission,
};
use metsuke_server::authority::Signed;
use metsuke_server::config::{AbsolutePath, DeveloperConfig, HttpConfig, IngestConfig};
use metsuke_wire::envelope::{
    self, AgentId, Envelope, Payload, PayloadLine, PoolId, Provenance, Sample, Signature,
    SigningKey, TraceLine, VerifyingKey,
};
use time::OffsetDateTime;

/// The all-sevens test seed, matching the agent suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// The machine every test submission says it came from.
pub fn test_agent_id() -> AgentId {
    AgentId::parse("test-relay").expect("a fixed name is a slug")
}

/// A trace line as the wire holds one: the node's object, parsed.
pub fn trace_line(line: &str) -> TraceLine {
    TraceLine::parse(line).unwrap_or_else(|error| panic!("{line:.60}: {error}"))
}

/// A second key, for the pool that did not sign.
pub fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

pub fn pool_of(key: &SigningKey) -> PoolId {
    PoolId::from_cold_key(&key.verifying_key())
}

/// A ceiling wide enough that no test submission's header reaches it. A plain
/// number: `verify` and `audit` take the bound, not the config field.
pub const MAX_HEADER_BYTES: u64 = 4096;

/// The clock every test judges against; envelopes are stamped with it.
pub fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap()
}

pub fn envelope_for(key: &SigningKey, counter: u64) -> Envelope {
    envelope_at(key, counter, test_now())
}

/// An envelope stamped with a caller-chosen clock.
pub fn envelope_at(key: &SigningKey, counter: u64, now: OffsetDateTime) -> Envelope {
    envelope_carrying(
        key,
        counter,
        now,
        Payload::samples(vec![
            PayloadLine::sample(&test_sample(now), &provenance_of(key))
                .expect("a test sample stamps"),
        ]),
    )
}

/// What every line of a submission from `key` is stamped with.
pub fn provenance_of(key: &SigningKey) -> Provenance {
    Provenance {
        pool_id: pool_of(key),
        agent_id: test_agent_id(),
    }
}

pub fn test_sample(now: OffsetDateTime) -> Sample {
    Sample {
        sampled_at: now,
        block_height: Some(12_345),
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}

/// The schema v2 envelope an agent collecting trace lines sends.
pub fn lines_envelope_at(
    key: &SigningKey,
    counter: u64,
    now: OffsetDateTime,
    lines: Vec<TraceLine>,
) -> Envelope {
    let stamp = provenance_of(key);
    envelope_carrying(
        key,
        counter,
        now,
        Payload::trace_lines(
            lines
                .iter()
                .map(|line| PayloadLine::trace_line(line, &stamp).expect("a parsed line stamps"))
                .collect(),
        ),
    )
}

pub fn envelope_carrying(
    key: &SigningKey,
    counter: u64,
    now: OffsetDateTime,
    payload: Payload,
) -> Envelope {
    Envelope::new(
        provenance_of(key),
        metsuke_server::CLIENT_VERSION.to_string(),
        counter,
        now,
        payload,
    )
}

/// The wire bytes and signature a client would send for this envelope.
pub fn seal(key: &SigningKey, envelope: &Envelope) -> (Vec<u8>, Signature) {
    envelope::seal(key, envelope, 0).unwrap()
}

/// Assemble the headers-and-body pair the intake takes.
pub fn submission(vkey: VerifyingKey, signature: Signature, wire_bytes: &[u8]) -> Signed<'_> {
    Signed {
        vkey,
        signature,
        wire_bytes,
    }
}

/// The application code every test pool is allowlisted against.
pub fn test_code() -> ApplicationCode {
    ApplicationCode::parse("MUSA-0000").expect("the test code is well formed")
}

/// The allowlist as the config holds it: pool against the code it came in on.
pub fn allowlist(allowed: &[PoolId]) -> BTreeMap<PoolId, ApplicationCode> {
    allowed
        .iter()
        .map(|pool_id| (*pool_id, test_code()))
        .collect()
}

/// The same allowlist as the inline TOML table a config file holds.
pub fn allowlist_toml(allowed: &BTreeMap<PoolId, ApplicationCode>) -> String {
    let pairs: Vec<String> = allowed
        .iter()
        .map(|(pool_id, code)| format!("{pool_id} = \"{code}\""))
        .collect();
    format!("{{ {} }}", pairs.join(", "))
}

/// The config file a spawned server reads, section by section.
/// `server_toml` is what the suite runs on, and a test exercising one section
/// replaces that field alone. Sections are rendered from the structs the
/// server loads them back into, and no config field has a default, so a field
/// added to `config.rs` and not to a renderer here stops every binary test at
/// load rather than passing on a value nobody set.
pub struct ServerToml {
    pub listen: String,
    pub http: String,
    pub archive: String,
    pub ingest: String,
    pub developer: String,
}

/// A whole config over an archive under `dir`, on a kernel-chosen port, with
/// every limit wide enough that only the check under test can fire.
pub fn server_toml(dir: &Path, allowed: &[PoolId]) -> ServerToml {
    ServerToml {
        listen: "127.0.0.1:0".to_string(),
        http: http_toml(&permissive_http()),
        archive: filesystem_archive(&dir.join("archive")),
        ingest: ingest_toml(&permissive_config(allowed)),
        developer: developer_toml(dir),
    }
}

/// Transport limits wide enough that only the check under test can fire. The
/// timeouts are bounded rather than unlimited, so a regression fails a test
/// rather than hanging it.
pub fn permissive_http() -> HttpConfig {
    HttpConfig {
        idle_timeout_secs: nonzero_u64(10),
        read_timeout_secs: nonzero_u64(10),
        write_timeout_secs: nonzero_u64(10),
        max_concurrent_requests: nonzero_u32(64),
    }
}

/// `[http]` as the file holds it. Destructured for the same reason
/// `ingest_toml` is: a field added to `HttpConfig` and not here does not
/// compile.
pub fn http_toml(config: &HttpConfig) -> String {
    let HttpConfig {
        idle_timeout_secs,
        read_timeout_secs,
        write_timeout_secs,
        max_concurrent_requests,
    } = config;
    format!(
        "[http]
idle_timeout_secs = {idle_timeout_secs}
read_timeout_secs = {read_timeout_secs}
write_timeout_secs = {write_timeout_secs}
max_concurrent_requests = {max_concurrent_requests}
"
    )
}

impl ServerToml {
    pub fn render(&self) -> String {
        [
            format!("listen = \"{}\"", self.listen),
            self.http.clone(),
            self.archive.clone(),
            self.ingest.clone(),
            self.developer.clone(),
        ]
        .join("\n")
    }

    /// Write the file and hand back its path, which is what `--config` takes.
    pub fn write(&self, dir: &Path) -> PathBuf {
        let path = dir.join("server.toml");
        std::fs::write(&path, self.render()).unwrap();
        path
    }
}

pub fn filesystem_archive(root: &Path) -> String {
    format!(
        "[archive]\nkind = \"filesystem\"\nroot = \"{}\"\n",
        root.display()
    )
}

/// `[ingest]` as the file holds it, from the struct it loads back into. The
/// destructure is the drift check: a field added to `IngestConfig` and not to
/// this renderer does not compile.
pub fn ingest_toml(config: &IngestConfig) -> String {
    let IngestConfig {
        allowlist,
        max_body_bytes,
        max_header_bytes,
        rate_limit_uploads,
        rate_limit_uploads_total,
        rate_limit_window_secs,
    } = config;
    format!(
        "[ingest]
allowlist = {allowlist}
max_body_bytes = {max_body_bytes}
max_header_bytes = {max_header_bytes}
rate_limit_uploads = {rate_limit_uploads}
rate_limit_uploads_total = {rate_limit_uploads_total}
rate_limit_window_secs = {rate_limit_window_secs}
",
        allowlist = allowlist_toml(allowlist),
    )
}

/// Limits wide enough that only the check under test can fire.
pub fn permissive_config(allowed: &[PoolId]) -> IngestConfig {
    IngestConfig {
        allowlist: allowlist(allowed),
        max_body_bytes: nonzero_u64(1024 * 1024),
        max_header_bytes: nonzero_u64(MAX_HEADER_BYTES),
        rate_limit_uploads: nonzero_u32(100),
        rate_limit_uploads_total: nonzero_u32(1000),
        rate_limit_window_secs: nonzero_u64(3600),
    }
}

/// Config limits are `NonZero`, and a test naming one wants the literal, not
/// the ceremony.
pub fn nonzero_u64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a test limit is never zero")
}

pub fn nonzero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("a test limit is never zero")
}

/// The submission `seal` produced, as the archive is asked to store it. The
/// name is stamped here, so a test that has to know the key holds it.
pub fn stored_submission<'a>(
    key: &SigningKey,
    name: ObjectName,
    signature: Signature,
    wire_bytes: &'a [u8],
) -> StoredSubmission<'a> {
    StoredSubmission {
        name,
        vkey: key.verifying_key(),
        signature,
        wire_bytes,
    }
}

/// One object read whole off an archive. The download streams
/// (`archive::Bytes`), and a test comparing it to the bytes that were stored
/// wants both in hand.
pub fn read_object(archive: &impl Bytes, key: &str) -> Result<Vec<u8>, ArchiveError> {
    use std::io::Read as _;

    let mut stream = archive.reader(key)?;
    let mut bytes = Vec::new();
    stream
        .reader
        .read_to_end(&mut bytes)
        .expect("a test archive's object reads");
    Ok(bytes)
}

/// The name a submission from `key` received at `now` is filed under.
pub fn object_name(key: &SigningKey, now: OffsetDateTime, kind: Kind) -> ObjectName {
    ObjectName::stamped(now, pool_of(key), test_agent_id(), kind)
}

/// The shipped server config, with its placeholder pool id replaced. Loading
/// it is what keeps the file an operator copies from parsing, and reading the
/// tests' own values out of it is what keeps a field the server grows from
/// reaching the tests and the operator on different days.
pub fn example_config() -> String {
    include_str!("../../../../contrib/server.example.toml")
        .replace("pool1CHANGEME", &pool_of(&test_key()).to_bech32())
}

/// The example's `[archive]` section, with the endpoint and retry count a test
/// needs. Stops at the blank line, so the commented-out filesystem block below
/// it stays out.
pub fn example_s3_archive(endpoint: &str, put_retries: u32) -> String {
    let body: Vec<String> = example_config()
        .split_once("\n[archive]\n")
        .expect("the example names an [archive] section")
        .1
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .map(|line| match line.split_once(" = ") {
            Some(("endpoint", _)) => format!("endpoint = \"{endpoint}\""),
            Some(("put_retries", _)) => format!("put_retries = {put_retries}"),
            // A test waits on these, so the operator-facing values would stall
            // the suite.
            Some(("request_timeout_secs", _)) => "request_timeout_secs = 5".to_string(),
            Some(("put_retry_backoff_ms", _)) => "put_retry_backoff_ms = 10".to_string(),
            _ => line.to_string(),
        })
        .collect();
    format!("[archive]\n{}\n", body.join("\n"))
}

/// What every test's credential file holds. One place, so the unit tests and
/// the spawned binary authenticate as the same account.
pub const DEVELOPER_PASSWORD: &str = "hunter2";

/// The developer half over a credential file under `dir`. Writing that file is
/// `developer_toml`'s job, because only a spawned server reads it.
pub fn developer_config(dir: &Path) -> DeveloperConfig {
    DeveloperConfig {
        user: "metsuke-dev".to_string(),
        password_file: absolute(dir.join("developer-password")),
        list_max_rows: nonzero_u32(100),
    }
}

/// `[developer]` as the file holds it, with the credential file written beside
/// it: a serving process reads it at startup, so a config naming one that is
/// not there never binds.
pub fn developer_toml(dir: &Path) -> String {
    developer_toml_with_rows(dir, developer_config(dir).list_max_rows.get())
}

/// The same section at a listing bound the caller chose, for a test about what
/// a page at the bound says.
pub fn developer_toml_with_rows(dir: &Path, list_max_rows: u32) -> String {
    let DeveloperConfig {
        user,
        password_file,
        list_max_rows,
    } = DeveloperConfig {
        list_max_rows: nonzero_u32(list_max_rows),
        ..developer_config(dir)
    };
    std::fs::write(password_file.as_path(), DEVELOPER_PASSWORD).unwrap();
    format!(
        r#"
[developer]
user = "{user}"
password_file = "{password_file}"
list_max_rows = {list_max_rows}
"#,
        password_file = password_file.as_path().display(),
    )
}

pub fn absolute(path: impl Into<PathBuf>) -> AbsolutePath {
    AbsolutePath::new(path.into()).expect("a test path is absolute")
}

/// An archive that fails whichever half the caller under test uses, standing
/// in for a bucket that is unreachable.
pub struct FailingArchive {
    pub reason: &'static str,
}

impl Store for FailingArchive {
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError> {
        Err(ArchiveError::Io {
            key: submission.object_key(),
            source: std::io::Error::other(self.reason),
        })
    }
}

impl Bytes for FailingArchive {
    fn reader(&self, key: &str) -> Result<ObjectStream, ArchiveError> {
        Err(ArchiveError::Fetch {
            key: key.to_string(),
            reason: self.reason.to_string(),
        })
    }
}

impl List for FailingArchive {
    fn location(&self) -> String {
        "the test archive".to_string()
    }

    fn for_each_key<E: From<ArchiveError>>(
        &self,
        _: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        Err(ArchiveError::List {
            reason: self.reason.to_string(),
        }
        .into())
    }

    fn page(&self, _: &str, _: &str, _: NonZeroU32) -> Result<Page, ArchiveError> {
        Err(ArchiveError::List {
            reason: self.reason.to_string(),
        })
    }
}

/// The opening two bytes of a TLS record: content type 22 (handshake), then
/// the legacy record version's major byte (RFC 8446 §5.1).
pub const TLS_HANDSHAKE_PREFIX: [u8; 2] = [0x16, 0x03];

/// Whatever one connection to `listener` sends first. Takes more bytes than
/// the caller asserts on so a cleartext regression prints as readable text.
pub fn opening_bytes(listener: std::net::TcpListener, budget: std::time::Duration) -> Vec<u8> {
    use std::io::{ErrorKind, Read};

    listener.set_nonblocking(true).unwrap();
    let start = std::time::Instant::now();
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                assert!(start.elapsed() < budget, "nothing connected in {budget:?}");
                std::thread::sleep(std::time::Duration::from_millis(10));
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
