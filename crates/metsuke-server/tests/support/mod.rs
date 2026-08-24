//! Helpers for the server tests: keys, an envelope, the sealed form of one,
//! and the stores a test runs against.
//!
//! Every test target compiles the whole module, so a helper only one of them
//! needs reads as dead code in the others.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use metsuke_server::applications::ApplicationCode;
use metsuke_server::archive::{ArchiveError, List, Store, StoredSubmission};
use metsuke_server::authority::{ColdKeyOrCalidus, Signed};
use metsuke_server::calidus::{CalidusKeys, Directory, DirectoryError};
use metsuke_server::config::{
    AbsolutePath, ApplicationsConfig, CalidusConfig, DeveloperConfig, IngestConfig,
};
use metsuke_server::index::Index;
use metsuke_wire::envelope::{
    self, Envelope, PoolId, SCHEMA_VERSION, Sample, Signature, SigningKey, VerifyingKey,
};
use time::OffsetDateTime;

/// The all-sevens test seed, matching the agent suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A second key, for the pool that did not sign.
pub fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

/// A hot key an SPO would register as their pool's Calidus key: it hashes to
/// no pool, so only the directory can make it speak for one.
pub fn calidus_key() -> SigningKey {
    SigningKey::from_bytes(&[3u8; 32])
}

/// What the pool rotates to, for a test that must tell a fetched answer from a
/// cached one.
pub fn rotated_calidus_key() -> SigningKey {
    SigningKey::from_bytes(&[5u8; 32])
}

pub fn pool_of(key: &SigningKey) -> PoolId {
    PoolId::from_cold_key(&key.verifying_key())
}

/// A decompression ceiling wide enough that no test payload reaches it. Plain
/// `u64`: `verify` and `audit` take the limit, not the config field.
pub const MAX_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024;

/// The clock every test judges against; envelopes are stamped with it.
pub fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap()
}

pub fn envelope_for(key: &SigningKey, counter: u64) -> Envelope {
    envelope_at(key, counter, test_now())
}

/// An envelope stamped with a caller-chosen clock.
pub fn envelope_at(key: &SigningKey, counter: u64, now: OffsetDateTime) -> Envelope {
    Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: pool_of(key),
        agent_version: metsuke_server::CLIENT_VERSION.to_string(),
        counter,
        timestamp: now,
        samples: vec![Sample {
            sampled_at: now,
            block_height: Some(12_345),
            slot: None,
            slot_in_epoch: None,
            epoch: None,
            sync_progress: None,
            node_version: None,
            node_revision: None,
            clock_offset_ms: None,
        }],
    }
}

/// The wire bytes and signature a client would send for this envelope.
pub fn seal(key: &SigningKey, envelope: &Envelope) -> (Vec<u8>, Signature) {
    envelope::seal(key, envelope, 0).unwrap()
}

/// Assemble the headers-and-body triple the intake takes.
pub fn submission<'a>(
    vkey: VerifyingKey,
    pool_id: PoolId,
    signature: Signature,
    wire_bytes: &'a [u8],
) -> Signed<'a> {
    Signed {
        pool_id,
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
    pub index_path: PathBuf,
    pub archive: String,
    pub ingest: String,
    pub calidus: String,
    pub developer: String,
    /// The one optional section (`ServerConfig::applications`).
    pub applications: Option<String>,
}

/// A whole config over stores under `dir`, on a kernel-chosen port, with every
/// limit wide enough that only the check under test can fire.
pub fn server_toml(dir: &Path, allowed: &[PoolId]) -> ServerToml {
    ServerToml {
        listen: "127.0.0.1:0".to_string(),
        index_path: dir.join("index.sqlite"),
        archive: filesystem_archive(&dir.join("archive")),
        ingest: ingest_toml(&permissive_config(allowed)),
        calidus: calidus_toml(dir),
        developer: developer_toml(dir),
        applications: None,
    }
}

impl ServerToml {
    pub fn render(&self) -> String {
        let sections = [
            Some(format!(
                "listen = \"{}\"\nindex_path = \"{}\"",
                self.listen,
                self.index_path.display()
            )),
            Some(self.archive.clone()),
            Some(self.ingest.clone()),
            Some(self.calidus.clone()),
            Some(self.developer.clone()),
            self.applications.clone(),
        ];
        sections
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
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
        max_decompressed_bytes,
        rate_limit_uploads,
        rate_limit_window_secs,
        max_timestamp_skew_secs,
    } = config;
    format!(
        "[ingest]
allowlist = {allowlist}
max_body_bytes = {max_body_bytes}
max_decompressed_bytes = {max_decompressed_bytes}
rate_limit_uploads = {rate_limit_uploads}
rate_limit_window_secs = {rate_limit_window_secs}
max_timestamp_skew_secs = {max_timestamp_skew_secs}
",
        allowlist = allowlist_toml(allowlist),
    )
}

/// Limits wide enough that only the check under test can fire.
pub fn permissive_config(allowed: &[PoolId]) -> IngestConfig {
    IngestConfig {
        allowlist: allowlist(allowed),
        max_body_bytes: nonzero_u64(1024 * 1024),
        max_decompressed_bytes: nonzero_u64(MAX_DECOMPRESSED_BYTES),
        rate_limit_uploads: nonzero_u32(100),
        rate_limit_window_secs: nonzero_u64(3600),
        max_timestamp_skew_secs: nonzero_u64(300),
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

/// The index every test opens, under a directory it owns.
pub fn index_store(dir: &Path) -> Index {
    Index::open(&dir.join("index.sqlite")).unwrap()
}

/// The submission `seal` produced, as the archive is asked to store it.
pub fn stored_submission<'a>(
    key: &SigningKey,
    counter: u64,
    timestamp: OffsetDateTime,
    signature: Signature,
    wire_bytes: &'a [u8],
) -> StoredSubmission<'a> {
    StoredSubmission {
        pool_id: pool_of(key),
        counter,
        timestamp,
        schema_version: SCHEMA_VERSION,
        vkey: key.verifying_key(),
        signature,
        wire_bytes,
    }
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

/// The Calidus half's config pointed at a socket directory a test owns, with
/// the connection values the assertions name.
pub fn calidus_config(socket_dir: &Path, genesis: &Path) -> CalidusConfig {
    CalidusConfig {
        socket_dir: absolute(socket_dir),
        dbname: "cexplorer".to_string(),
        role: "metsuke_ro".to_string(),
        password_file: absolute(socket_dir.join("pgpass")),
        query_timeout_secs: nonzero_u64(11),
        shelley_genesis_path: absolute(genesis),
        resolution_ttl_secs: nonzero_u32(TEST_TTL_SECS),
    }
}

/// The security parameter every test genesis carries. Not any network's k:
/// where the real one comes from is `dbsync::security_parameter`'s to prove.
pub const TEST_SECURITY_PARAMETER: u32 = 6;

/// A Shelley genesis holding the one field the server reads, and the
/// `[calidus]` section naming it. The file is written because `serve` reads it
/// before it binds; the db-sync it names is never reached by a cold-key upload.
pub fn calidus_toml(dir: &Path) -> String {
    let genesis = dir.join("shelley-genesis.json");
    std::fs::write(
        &genesis,
        format!("{{\"securityParam\": {TEST_SECURITY_PARAMETER}}}"),
    )
    .unwrap();
    let CalidusConfig {
        socket_dir,
        dbname,
        role,
        password_file,
        query_timeout_secs,
        shelley_genesis_path,
        resolution_ttl_secs,
    } = calidus_config(dir, &genesis);
    format!(
        r#"
[calidus]
socket_dir = "{socket_dir}"
dbname = "{dbname}"
role = "{role}"
password_file = "{password_file}"
query_timeout_secs = {query_timeout_secs}
shelley_genesis_path = "{shelley_genesis_path}"
resolution_ttl_secs = {resolution_ttl_secs}
"#,
        socket_dir = socket_dir.as_path().display(),
        password_file = password_file.as_path().display(),
        shelley_genesis_path = shelley_genesis_path.as_path().display(),
    )
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

/// The applications half over a CSV the caller wrote. `socket_dir` is a
/// directory holding no socket, so the chain read fails to connect.
pub fn applications_config(applications_csv: &Path, socket_dir: &Path) -> ApplicationsConfig {
    ApplicationsConfig {
        applications_csv: absolute(applications_csv),
        socket_dir: absolute(socket_dir),
        dbname: "cexplorer".to_string(),
        role: "metsuke_ro".to_string(),
        query_timeout_secs: nonzero_u64(7),
    }
}

pub fn applications_toml(config: &ApplicationsConfig) -> String {
    let ApplicationsConfig {
        applications_csv,
        socket_dir,
        dbname,
        role,
        query_timeout_secs,
    } = config;
    format!(
        r#"
[applications]
applications_csv = "{applications_csv}"
socket_dir = "{socket_dir}"
dbname = "{dbname}"
role = "{role}"
query_timeout_secs = {query_timeout_secs}
"#,
        applications_csv = applications_csv.as_path().display(),
        socket_dir = socket_dir.as_path().display(),
    )
}

pub fn absolute(path: impl Into<PathBuf>) -> AbsolutePath {
    AbsolutePath::new(path.into()).expect("a test path is absolute")
}

/// The password file `calidus_config` names, so a test reaches the connection
/// attempt rather than stopping at the unreadable file before it.
pub fn write_password(socket_dir: &Path) {
    std::fs::write(socket_dir.join("pgpass"), "hunter2\n").unwrap();
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
}

/// One recorded label-867 blob, as db-sync hands it back. Which registration
/// each name is: tests/fixtures/calidus/README.md.
pub fn registration(name: &str) -> Vec<u8> {
    fixture("recordings", name)
}

/// A blob assembled out of real signatures to be one no tool produces.
pub fn crafted(name: &str) -> Vec<u8> {
    fixture("crafted", name)
}

fn fixture(kind: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/calidus")
        .join(kind)
        .join(format!("{name}.hex"));
    let hex = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    decode_hex(hex.trim())
}

fn decode_hex(text: &str) -> Vec<u8> {
    metsuke_wire::hex::decode_bytes(text).expect("a fixture is pairs of hex digits")
}

/// The pool every recorded registration scopes: the recorder signs their
/// witnesses with the suite's own `test_key`.
pub fn registered_pool() -> PoolId {
    pool_of(&test_key())
}

/// A Calidus directory serving the blobs a test put in it, counting what it was
/// asked. The count is the assertion behind the resolution TTL: an answer that
/// did not increment it never reached db-sync.
///
/// Shared rather than owned, so a test still holds it after the authority
/// under test took it.
#[derive(Clone)]
pub struct CannedDirectory {
    chain: Rc<Chain>,
}

#[derive(Default)]
struct Chain {
    registrations: RefCell<HashMap<PoolId, Vec<Vec<u8>>>>,
    lookups: Cell<usize>,
}

impl CannedDirectory {
    pub fn holding(pool_id: PoolId, registrations: Vec<Vec<u8>>) -> Self {
        let directory = CannedDirectory {
            chain: Rc::new(Chain::default()),
        };
        directory.rotate(pool_id, registrations);
        directory
    }

    /// What the chain says from now on, as a re-registration would leave it.
    pub fn rotate(&self, pool_id: PoolId, registrations: Vec<Vec<u8>>) {
        self.chain
            .registrations
            .borrow_mut()
            .insert(pool_id, registrations);
    }

    pub fn lookups(&self) -> usize {
        self.chain.lookups.get()
    }
}

impl Directory for CannedDirectory {
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Vec<u8>>, DirectoryError> {
        self.chain.lookups.set(self.lookups() + 1);
        Ok(self
            .chain
            .registrations
            .borrow()
            .get(&pool_id)
            .cloned()
            .unwrap_or_default())
    }
}

/// How long a test's resolutions stand. Inside `permissive_config`'s timestamp
/// skew, so a test can both age a resolution out and still submit an envelope.
pub const TEST_TTL_SECS: u32 = 60;

/// The Calidus-capable authority over a directory the caller still holds.
pub fn calidus_authority(directory: CannedDirectory) -> ColdKeyOrCalidus<CannedDirectory> {
    ColdKeyOrCalidus::new(CalidusKeys::new(directory, nonzero_u32(TEST_TTL_SECS)))
}

/// A directory that cannot answer, standing in for a db-sync that is down.
pub struct UnavailableDirectory {
    pub reason: &'static str,
}

impl Directory for UnavailableDirectory {
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Vec<u8>>, DirectoryError> {
        Err(DirectoryError::Unavailable {
            pool_id,
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
