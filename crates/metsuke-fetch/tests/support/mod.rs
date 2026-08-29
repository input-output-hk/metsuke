//! The real server, in this process, over a filesystem archive the test seeds:
//! the routes, the credential and the listing body are the shipped ones, so a
//! query field or a JSON key this tool misreads fails here.
//!
//! Every test target compiles the whole module, so a helper only one of them
//! needs reads as dead code in the others.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::time::Duration;

use metsuke_fetch::pull::Archive;
use metsuke_server::applications::Codes;
use metsuke_server::archive::{FilesystemArchive, Kind, ObjectName};
use metsuke_server::config::{AbsolutePath, DeveloperConfig, HttpConfig, IngestConfig};
use metsuke_server::developer::Developer;
use metsuke_server::instructions;
use metsuke_server::intake::Intake;
use metsuke_server::serve;
use metsuke_wire::envelope::{
    AgentId, Envelope, Payload, PayloadLine, PoolId, Provenance, Scrape, Signature, SigningKey,
    VerifyingKey, seal,
};
use metsuke_wire::fixtures;
use time::OffsetDateTime;

/// The one account the routes authenticate.
pub const USER: &str = "developer";
pub const PASSWORD: &str = "the-shared-account";

/// Whole-request deadline for the tool's own requests. Loopback and a
/// filesystem archive, so anything near it is a hang rather than a slow read.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The all-sevens test seed, matching the other suites.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A second pool's key, so the archive holds more than one pool's objects.
pub fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

pub fn pool_of(key: &SigningKey) -> PoolId {
    PoolId::from_cold_key(&key.verifying_key())
}

pub fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_755_000_000).expect("a fixed instant")
}

/// One object as the archive holds it: the key it is filed under and the
/// signed bytes stored there.
pub struct Object {
    pub key: String,
    pub wire_bytes: Vec<u8>,
    pub signature: Signature,
    /// The key that sealed it, so a test can answer the pair beside the bytes
    /// or tamper with either half.
    pub signer: SigningKey,
}

/// A filesystem archive that answers the metadata an S3 one holds beside an
/// object. A real filesystem archive discards the pair at ingest, so a suite
/// built on one could only ever exercise the unverifiable path; what a test
/// seeds into `attested` is what the download then carries.
/// Shared, so a test can answer a different pair after the server is up.
pub type Attested = std::sync::Arc<std::sync::Mutex<HashMap<String, (VerifyingKey, Signature)>>>;

pub struct Attesting {
    inner: FilesystemArchive,
    attested: Attested,
}

impl metsuke_server::archive::Store for Attesting {
    fn store(
        &self,
        submission: &metsuke_server::archive::StoredSubmission<'_>,
    ) -> Result<(), metsuke_server::archive::ArchiveError> {
        self.inner.store(submission)
    }
}

impl metsuke_server::archive::Bytes for Attesting {
    fn reader(
        &self,
        key: &str,
    ) -> Result<metsuke_server::archive::ObjectStream, metsuke_server::archive::ArchiveError> {
        Ok(metsuke_server::archive::ObjectStream {
            attestation: self
                .attested
                .lock()
                .expect("no panic holds this lock")
                .get(key)
                .map(|(vkey, signature)| metsuke_server::archive::Attestation {
                    vkey: *vkey,
                    signature: *signature,
                }),
            ..self.inner.reader(key)?
        })
    }
}

impl metsuke_server::archive::List for Attesting {
    fn location(&self) -> String {
        self.inner.location()
    }

    fn for_each_key<E: From<metsuke_server::archive::ArchiveError>>(
        &self,
        visit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        self.inner.for_each_key(visit)
    }

    fn page(
        &self,
        prefix: &str,
        after: &str,
        max_keys: NonZeroU32,
    ) -> Result<metsuke_server::archive::Page, metsuke_server::archive::ArchiveError> {
        self.inner.page(prefix, after, max_keys)
    }
}

/// A server serving one archive directory, with the objects the test seeded in
/// it. Serving is left running for the process's life: the thread holds the
/// listener, and a test that returns is a test that has stopped asking.
pub struct Server {
    pub url: String,
    pub objects: Vec<Object>,
    root: PathBuf,
    attested: Attested,
    /// Held so the archive and the password file outlive the server.
    _dir: tempfile::TempDir,
}

impl Server {
    /// Serve `count` objects, `list_max_rows` keys to a page, with nothing to
    /// check them by. That is what a filesystem archive answers, so it is what
    /// every test not about the checking meets.
    pub fn with_objects(count: usize, list_max_rows: u32) -> Server {
        Server::serving(count, list_max_rows, false)
    }

    /// The same, with each object's key and signature answered beside it, as
    /// an S3 archive holds them.
    pub fn attesting(count: usize, list_max_rows: u32) -> Server {
        Server::serving(count, list_max_rows, true)
    }

    fn serving(count: usize, list_max_rows: u32, attest: bool) -> Server {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path().join("archive");
        let objects = (0..count)
            .map(|index| seeded(&root, index))
            .collect::<Vec<Object>>();
        let password_file = dir.path().join("password");
        std::fs::write(&password_file, format!("{PASSWORD}\n")).expect("the password file writes");
        let developer = Developer::new(
            &DeveloperConfig {
                user: USER.to_string(),
                password_file: AbsolutePath::new(password_file).expect("a temp dir is absolute"),
                list_max_rows: nonzero_u32(list_max_rows),
            },
            PASSWORD,
        );
        let listener = serve::bind("127.0.0.1:0").expect("a kernel-chosen port binds");
        let url = format!("http://{}", listener.address());
        let attested: Attested = std::sync::Arc::new(std::sync::Mutex::new(match attest {
            false => HashMap::new(),
            true => objects
                .iter()
                .map(|object| {
                    (
                        object.key.clone(),
                        (object.signer.verifying_key(), object.signature),
                    )
                })
                .collect(),
        }));
        let intake = Intake::new(
            ingest_config(),
            Attesting {
                inner: FilesystemArchive::new(&root),
                attested: std::sync::Arc::clone(&attested),
            },
        );
        std::thread::spawn(move || {
            match listener.serve(http_config(), intake, developer, instructions::page()) {
                Ok(never) => match never {},
                Err(error) => panic!("the test server stopped accepting: {error}"),
            }
        });
        Server {
            url,
            objects,
            root,
            attested,
            _dir: dir,
        }
    }

    /// The tool's client for this server, as an operator's flags would build
    /// it.
    pub fn pulling(&self) -> Archive {
        Archive::new(&self.url, USER, PASSWORD, TIMEOUT)
    }

    /// The same with a credential this account does not hold.
    pub fn pulling_with(&self, user: &str, password: &str) -> Archive {
        Archive::new(&self.url, user, password, TIMEOUT)
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects
            .iter()
            .map(|object| object.key.clone())
            .collect()
    }

    /// One object the archive lists and cannot read.
    pub fn unreadable(&self, key: &str) {
        let path = self.root.join(key);
        let mut permissions = std::fs::metadata(&path)
            .expect("the object is there")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o000);
        std::fs::set_permissions(&path, permissions).expect("permissions change");
    }

    /// The pair this archive answers for `key`, replacing whatever it held.
    fn answer_with(&self, key: &str, vkey: VerifyingKey, signature: Signature) {
        self.attested
            .lock()
            .expect("no panic holds this lock")
            .insert(key.to_string(), (vkey, signature));
    }

    /// One byte of a stored object flipped, leaving the metadata beside it
    /// alone: bytes that no longer stand under the signature the archive
    /// answers with.
    pub fn tamper(&self, key: &str) {
        let path = self.root.join(key);
        let mut bytes = std::fs::read(&path).expect("the object is there");
        *bytes.last_mut().expect("a sealed object is not empty") ^= 0xff;
        std::fs::write(&path, &bytes).expect("the object rewrites");
    }

    /// A stored object replaced by one another key sealed, answered with that
    /// key's own pair: bytes that verify, under a pool that is not the one the
    /// object is filed under.
    pub fn reseal_as(&self, key: &str, signer: &SigningKey) {
        let name = ObjectName::parse(key).expect("a seeded key parses");
        let envelope = Envelope::new(
            Provenance {
                pool_id: pool_of(signer),
                agent_id: name.agent_id.clone(),
            },
            env!("CARGO_PKG_VERSION").to_string(),
            1,
            test_now(),
            Payload::scrapes(vec![
                PayloadLine::scrape(
                    &scrape(test_now()),
                    &Provenance {
                        pool_id: pool_of(signer),
                        agent_id: name.agent_id,
                    },
                )
                .expect("a scrape stamps"),
            ]),
        );
        let (wire_bytes, signature) = seal(signer, &envelope, 0).expect("a test envelope seals");
        std::fs::write(self.root.join(key), &wire_bytes).expect("the object rewrites");
        self.answer_with(key, signer.verifying_key(), signature);
    }

    /// An object under a key no `ObjectName::parse` reads, which is what
    /// something other than this server would leave in the bucket.
    pub fn seed_foreign(&self, key: &str, bytes: &[u8]) {
        let path = self.root.join(key);
        std::fs::create_dir_all(path.parent().expect("a key has a folder"))
            .expect("the folder writes");
        std::fs::write(&path, bytes).expect("the object writes");
    }
}

/// A listing route answering `listing` to every request, whatever it asked
/// for. Hand-written rather than the shipped server, because what
/// `SyncError::Stuck` guards is a page the server does not produce. The body is
/// serialized from the shipped `Listing`, so the field names are still the
/// server's.
pub fn fixed_listing(listing: &metsuke_wire::http::Listing) -> Archive {
    let body = serde_json::to_string(listing).expect("a listing serializes");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a kernel-chosen port binds");
    let url = format!("http://{}", listener.local_addr().expect("a bound address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("an accepted connection");
            let mut head = std::io::BufReader::new(stream.try_clone().expect("the stream clones"));
            let mut line = String::new();
            // The head is read to its blank line and dropped: what a request
            // asked for is what this server ignores.
            while head.read_line(&mut line).expect("the request head reads") > 2 {
                line.clear();
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the answer writes");
        }
    });
    Archive::new(&url, USER, PASSWORD, TIMEOUT)
}

/// One signed object written into the archive, distinct in pool, agent and kind
/// so a prefix filter has something to select. Sealed rather than invented: the
/// bytes on disk are a real submission, so a download of them is verifiable
/// against the key that signed it.
fn seeded(root: &Path, index: usize) -> Object {
    // Two pools, so a `--pool` filter has something to leave out; the agent and
    // the kind vary with the index for the same reason.
    let key = match index % 2 {
        0 => test_key(),
        _ => other_key(),
    };
    let pool_id = PoolId::from_cold_key(&key.verifying_key());
    let agent_id = AgentId::parse(&format!("relay-{index}")).expect("a fixed name is a slug");
    let kind = match index % 2 {
        0 => Kind::Metrics,
        _ => Kind::Logs,
    };
    // A day apart each, so the date folder is a prefix a test can filter on.
    let stamped = test_now() + time::Duration::days(index as i64);
    let provenance = Provenance {
        pool_id,
        agent_id: agent_id.clone(),
    };
    let payload = match kind {
        Kind::Metrics => Payload::scrapes(vec![
            PayloadLine::scrape(&scrape(stamped), &provenance).expect("a scrape stamps"),
        ]),
        Kind::Logs => Payload::trace_lines(vec![PayloadLine::spooled(
            serde_json::json!({"ns": "Test", "metsuke": {"pool_id": pool_id, "agent_id": agent_id}})
                .to_string(),
        )]),
    };
    let envelope = Envelope::new(
        provenance,
        env!("CARGO_PKG_VERSION").to_string(),
        index as u64,
        stamped,
        payload,
    );
    let (wire_bytes, signature) = seal(&key, &envelope, 0).expect("a test envelope seals");
    let name = ObjectName::stamped(stamped, pool_id, agent_id, kind);
    let path = root.join(name.to_key());
    std::fs::create_dir_all(path.parent().expect("a key has a folder")).expect("the folder writes");
    std::fs::write(&path, &wire_bytes).expect("the object writes");
    Object {
        key: name.to_key(),
        signer: key,
        wire_bytes,
        signature,
    }
}

fn scrape(now: OffsetDateTime) -> Scrape {
    fixtures::block_number_scrape(now, 1)
}

/// Ingest limits wide enough that no test reaches one: nothing here submits,
/// and the developer routes read none of them.
fn ingest_config() -> IngestConfig {
    IngestConfig {
        allowlist: Codes::new(),
        max_body_bytes: nonzero_u64(1 << 20),
        max_header_bytes: nonzero_u64(4096),
        max_timestamp_skew_secs: nonzero_u32(u32::MAX),
        rate_limit_uploads: nonzero_u32(1000),
        rate_limit_uploads_total: nonzero_u32(1000),
        rate_limit_window_secs: nonzero_u32(3600),
    }
}

fn http_config() -> HttpConfig {
    HttpConfig {
        idle_timeout_ms: nonzero_u64(30_000),
        read_timeout_ms: nonzero_u64(30_000),
        write_timeout_ms: nonzero_u64(30_000),
        max_concurrent_requests: nonzero_u32(8),
    }
}

pub fn nonzero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("a test constant above zero")
}

pub fn nonzero_u64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a test constant above zero")
}
