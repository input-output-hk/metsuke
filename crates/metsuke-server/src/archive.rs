//! The storage seam (ADR 0005). Both implementations key objects the same
//! way, so a rebuild reads either one and `cargo test` covers the naming the
//! bucket relies on.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use metsuke::envelope::{PoolId, PoolIdError, Signature, VerifyingKey};
use time::{OffsetDateTime, UtcOffset};

/// One accepted submission: the bytes to store, plus what `S3Archive` writes
/// as object metadata. `FilesystemArchive` writes the bytes only.
pub struct StoredSubmission<'a> {
    pub pool_id: PoolId,
    pub counter: u64,
    pub timestamp: OffsetDateTime,
    pub schema_version: u32,
    pub vkey: VerifyingKey,
    pub signature: Signature,
    /// The body as received: compressed, signed, untouched.
    pub wire_bytes: &'a [u8],
}

impl StoredSubmission<'_> {
    pub fn name(&self) -> ObjectName {
        ObjectName {
            pool_id: self.pool_id,
            counter: self.counter,
            timestamp: self.timestamp,
        }
    }

    pub fn object_key(&self) -> String {
        self.name().to_key()
    }
}

/// What the ADR-0005 object key encodes. `parse` is the inverse of `to_key`,
/// and nothing else parses a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectName {
    pub pool_id: PoolId,
    pub counter: u64,
    pub timestamp: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
#[error("{key:?} is not a v1 archive object key: {reason}")]
pub struct ObjectNameError {
    key: String,
    reason: String,
}

pub const KEY_PREFIX: &str = "v1/";

impl ObjectName {
    pub fn to_key(&self) -> String {
        // The day folder is UTC, not the sender's: the agent stamps envelopes
        // with its own offset, and a key naming a local day would sort two
        // pools' uploads of the same instant into different folders.
        let date = self.timestamp.to_offset(UtcOffset::UTC).date();
        format!(
            "{KEY_PREFIX}{pool}/{year:04}-{month:02}-{day:02}/{unix}-{counter}.json.zst",
            pool = self.pool_id,
            year = date.year(),
            month = u8::from(date.month()),
            day = date.day(),
            unix = self.timestamp.unix_timestamp(),
            counter = self.counter,
        )
    }

    pub fn parse(key: &str) -> Result<ObjectName, ObjectNameError> {
        let refuse = |reason: String| ObjectNameError {
            key: key.to_string(),
            reason,
        };
        let [prefix, pool, date, file] = *key.split('/').collect::<Vec<_>>() else {
            return Err(refuse("expected v1/<pool>/<date>/<file>".to_string()));
        };
        let schema = KEY_PREFIX.trim_end_matches('/');
        if prefix != schema {
            return Err(refuse(format!(
                "schema prefix is {prefix:?}, not {schema:?}"
            )));
        }
        let pool_id =
            PoolId::from_bech32(pool).map_err(|error: PoolIdError| refuse(error.to_string()))?;
        let stem = file
            .strip_suffix(".json.zst")
            .ok_or_else(|| refuse(format!("{file:?} is not a .json.zst object")))?;
        // From the right: the counter is the last segment, and a pre-epoch
        // timestamp would carry a leading minus of its own.
        let (unix, counter) = stem
            .rsplit_once('-')
            .ok_or_else(|| refuse(format!("{stem:?} is not <timestamp>-<counter>")))?;
        let counter: u64 = counter
            .parse()
            .map_err(|_| refuse(format!("counter {counter:?} is not a number")))?;
        let unix: i64 = unix
            .parse()
            .map_err(|_| refuse(format!("timestamp {unix:?} is not a number")))?;
        let timestamp = OffsetDateTime::from_unix_timestamp(unix)
            .map_err(|error| refuse(format!("timestamp {unix}: {error}")))?;
        let name = ObjectName {
            pool_id,
            counter,
            timestamp,
        };
        // The folder repeats the timestamp's day; a key where they disagree
        // was written by something other than `to_key`.
        if name.to_key() != key {
            return Err(refuse(format!(
                "timestamp {unix} is not in the {date} folder"
            )));
        }
        Ok(name)
    }
}

/// A stored object read back: its bytes and the metadata written beside them
/// (ADR 0005). The `metadata_*` fields are the header copies, `name` the
/// key's; `verify` is what reconciles them with the payload.
#[derive(Debug)]
pub struct FetchedObject {
    pub name: ObjectName,
    pub vkey: VerifyingKey,
    pub signature: Signature,
    pub metadata_schema_version: u32,
    pub metadata_counter: u64,
    pub wire_bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("storing {key}: {source}")]
    Io {
        key: String,
        #[source]
        source: io::Error,
    },
    #[error("fetching {key}: {reason}")]
    Fetch { key: String, reason: String },
    #[error("storing {key}: {reason} (after {attempts} attempts)")]
    Upload {
        key: String,
        attempts: u32,
        reason: String,
    },
    #[error("listing the archive: {reason}")]
    List { reason: String },
}

pub trait Archive {
    /// Store the submission. Returning `Ok` is what lets the server ACK, so
    /// an implementation returns only once the bytes are durable (ADR 0004).
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError>;

    /// Every object key held, in no particular order.
    fn list_keys(&self) -> Result<Vec<String>, ArchiveError>;
}

/// The inverse of `Archive::store`, read by `verify::audit` and, once it
/// exists, the download endpoint (ticket metsuke-4zo.10). Separate from
/// `Archive` because storing and reading back are different privileges — the
/// ingest path never fetches.
pub trait Fetch {
    fn fetch(&self, key: &str) -> Result<FetchedObject, ArchiveError>;
}

/// Objects as files under a root directory, keyed exactly as S3 keys them.
///
/// Implements no `Fetch`: the metadata would need a sidecar, and a sidecar
/// under the same prefix comes back in `list_keys` as an object whose key
/// nothing can parse.
pub struct FilesystemArchive {
    root: PathBuf,
}

impl FilesystemArchive {
    pub fn new(root: &Path) -> Self {
        FilesystemArchive {
            root: root.to_path_buf(),
        }
    }

    /// Append every object under `relative`, a path already below the archive
    /// root, so what lands in `keys` is the key and not the local path.
    fn walk(&self, relative: &Path, keys: &mut Vec<String>) -> Result<(), ArchiveError> {
        let directory = self.root.join(relative);
        for entry in read_dir(&directory)? {
            let entry = entry.map_err(|error| unreadable(&directory, &error))?;
            let path = relative.join(entry.file_name());
            // `file_type` over `is_dir`: the latter answers false when it
            // cannot stat, which would push an unreadable directory as an
            // object key and report the failure as a malformed one.
            let kind = entry
                .file_type()
                .map_err(|error| unreadable(&self.root.join(&path), &error))?;
            if kind.is_dir() {
                self.walk(&path, keys)?;
            } else {
                let key = path.to_str().ok_or_else(|| ArchiveError::List {
                    reason: format!("{} is not a UTF-8 object key", path.display()),
                })?;
                keys.push(key.to_string());
            }
        }
        Ok(())
    }
}

fn read_dir(directory: &Path) -> Result<fs::ReadDir, ArchiveError> {
    fs::read_dir(directory).map_err(|error| unreadable(directory, &error))
}

fn unreadable(path: &Path, error: &io::Error) -> ArchiveError {
    ArchiveError::List {
        reason: format!("{}: {error}", path.display()),
    }
}

impl Archive for FilesystemArchive {
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError> {
        let key = submission.object_key();
        let path = self.root.join(&key);
        let write = || -> io::Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, submission.wire_bytes)
        };
        write().map_err(|source| ArchiveError::Io { key, source })
    }

    fn list_keys(&self) -> Result<Vec<String>, ArchiveError> {
        // Only a root that is not there yet is an empty archive. A root that
        // exists and cannot be read must fail: reported as empty it would
        // reset every counter the objects still hold.
        if let Err(error) = fs::read_dir(&self.root) {
            return match error.kind() {
                io::ErrorKind::NotFound => Ok(Vec::new()),
                _ => Err(unreadable(&self.root, &error)),
            };
        }
        let mut keys = Vec::new();
        self.walk(Path::new(""), &mut keys)?;
        Ok(keys)
    }
}
