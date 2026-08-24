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
use metsuke_server::calidus::{CalidusKeys, Directory, DirectoryError, Registration};
use metsuke_server::config::{AbsolutePath, ApplicationsConfig, IngestConfig};
use metsuke_server::counters::CounterStore;
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
pub fn allowlist_toml(allowed: &[PoolId]) -> String {
    let pairs: Vec<String> = allowlist(allowed)
        .iter()
        .map(|(pool_id, code)| format!("{pool_id} = \"{code}\""))
        .collect();
    format!("{{ {} }}", pairs.join(", "))
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

/// The counter database every test opens, under a directory it owns.
pub fn counter_store(dir: &Path) -> CounterStore {
    CounterStore::open(&dir.join("counters.sqlite")).unwrap()
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

/// The example's `[archive]` body, with the endpoint and retry count a test
/// needs. Stops at the blank line, so the commented-out filesystem block below
/// it stays out.
pub fn example_s3_archive(endpoint: &str, put_retries: u32) -> String {
    let body = example_config()
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
        .collect::<Vec<_>>();
    body.join("\n")
}

/// The gate's config pointed at a `psql` a test placed, with the connection
/// values the assertions name. These tests drive the registered half, so the
/// applications path is a placeholder nothing opens.
pub fn applications_config(psql_path: &Path, socket_dir: &Path) -> ApplicationsConfig {
    ApplicationsConfig {
        applications_csv: absolute("/nonexistent/applications.csv"),
        psql_path: absolute(psql_path),
        socket_dir: absolute(socket_dir),
        dbname: "cexplorer".to_string(),
        role: "metsuke_ro".to_string(),
        query_timeout_secs: nonzero_u64(7),
    }
}

pub fn absolute(path: impl Into<PathBuf>) -> AbsolutePath {
    AbsolutePath::new(path.into()).expect("a test path is absolute")
}

/// A `psql` that records what it was run with and prints `csv` back. Shell
/// builtins alone: `Psql` clears the environment, leaving the shell on whatever
/// default `PATH` it was compiled with.
///
/// The CSV a test hands it is written by hand: no db-sync has answered this
/// query yet (ticket metsuke-4zo.52), so what it proves is the reading, not the
/// shape of a real answer.
pub fn fake_psql(dir: &Path, recording: &Path, csv: &str) -> PathBuf {
    let psql = dir.join("psql");
    std::fs::write(
        &psql,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"PGOPTIONS=$PGOPTIONS\" \"$@\" > {recording}\n\
             printf '%s' '{csv}'\n",
            recording = recording.display(),
        ),
    )
    .unwrap();
    make_executable(&psql);
    psql
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
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

/// A Calidus directory serving what a test put in it, counting what it was
/// asked. The count is the assertion behind cache-forever: a resolution that
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
    registrations: RefCell<HashMap<PoolId, Vec<Registration>>>,
    lookups: Cell<usize>,
}

impl CannedDirectory {
    pub fn holding(pool_id: PoolId, registrations: Vec<Registration>) -> Self {
        let directory = CannedDirectory {
            chain: Rc::new(Chain::default()),
        };
        directory.rotate(pool_id, registrations);
        directory
    }

    /// What the chain says from now on, as a re-registration would leave it.
    pub fn rotate(&self, pool_id: PoolId, registrations: Vec<Registration>) {
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
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Registration>, DirectoryError> {
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

/// The CIP-151 registration naming one of the tests' keys.
pub fn registration(key: &SigningKey, nonce: u64) -> Registration {
    Registration {
        nonce,
        key: key.verifying_key().to_bytes(),
    }
}

/// The Calidus-capable authority over a directory the caller still holds.
pub fn calidus_authority(directory: CannedDirectory) -> ColdKeyOrCalidus<CannedDirectory> {
    ColdKeyOrCalidus::new(CalidusKeys::new(
        directory,
        nonzero_u32(1),
        nonzero_u64(3600),
    ))
}

/// A directory that cannot answer, standing in for a db-sync that is down.
pub struct UnavailableDirectory {
    pub reason: &'static str,
}

impl Directory for UnavailableDirectory {
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Registration>, DirectoryError> {
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
