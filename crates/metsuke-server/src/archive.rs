//! The storage seam (ADR 0005). Both implementations key objects the same
//! way, so a rebuild reads either one and `cargo test` covers the naming the
//! bucket relies on.

use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use metsuke_wire::envelope::{
    AgentId, AgentIdError, PoolId, SCHEMA_VERSION_LINES, SCHEMA_VERSION_SAMPLES, Signature,
    VerifyingKey,
};
use time::{Date, OffsetDateTime};
use uuid::{NoContext, Timestamp, Uuid, Version};

/// One accepted submission: the bytes to store under `name`, plus what
/// `S3Archive` writes as object metadata. `FilesystemArchive` writes the bytes
/// only: the metadata is `vkey` and `signature`, and why only those two is
/// ADR 0005's.
pub struct StoredSubmission<'a> {
    pub name: ObjectName,
    pub vkey: VerifyingKey,
    pub signature: Signature,
    /// The body as received: compressed, signed, untouched.
    pub wire_bytes: &'a [u8],
}

impl StoredSubmission<'_> {
    pub fn object_key(&self) -> String {
        self.name.to_key()
    }
}

/// Which payload a batch carries, as the object key spells it. Named from the
/// schema version rather than read out of the payload: the server never
/// decompresses one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Metrics,
    Logs,
}

impl Kind {
    /// `None` for a version this build has no name for. A batch it cannot name
    /// is a batch it cannot file, which is the only reason ingest still reads
    /// the version at all.
    pub fn of(schema_version: u32) -> Option<Kind> {
        match schema_version {
            SCHEMA_VERSION_SAMPLES => Some(Kind::Metrics),
            SCHEMA_VERSION_LINES => Some(Kind::Logs),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Metrics => "metrics",
            Kind::Logs => "logs",
        }
    }

    fn parse(segment: &str) -> Option<Kind> {
        [Kind::Metrics, Kind::Logs]
            .into_iter()
            .find(|kind| kind.as_str() == segment)
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an object key encodes. `parse` is the inverse of `to_key`, and nothing
/// else parses a key.
///
/// Time-major, so one start-after cursor is the whole delta-sync protocol: the
/// day folder orders the corpus and the UUIDv7 orders within it. Uniqueness is
/// the UUIDv7's alone — never the clock, the sequence number or the agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectName {
    /// Stamped when the submission was received, which is what makes a late
    /// spool flush sort after everything already synced.
    pub id: Uuid,
    pub pool_id: PoolId,
    pub agent_id: AgentId,
    pub kind: Kind,
}

#[derive(Debug, thiserror::Error)]
#[error("{key:?} is not a v1 archive object key: {reason}")]
pub struct ObjectNameError {
    key: String,
    reason: String,
}

pub const KEY_PREFIX: &str = "v1/";

/// The suffix every object carries: JSON Lines, zstd-compressed, as the wire
/// container holds them.
const KEY_SUFFIX: &str = ".jsonl.zst";

impl ObjectName {
    /// The name a submission received at `now` is filed under.
    pub fn stamped(
        now: OffsetDateTime,
        pool_id: PoolId,
        agent_id: AgentId,
        kind: Kind,
    ) -> ObjectName {
        // Seconds and nanos of the receipt instant: the UUIDv7 carries the
        // millisecond, which is what the day folder is then read back from.
        let stamp = Timestamp::from_unix(NoContext, now.unix_timestamp() as u64, now.nanosecond());
        ObjectName {
            id: Uuid::new_v7(stamp),
            pool_id,
            agent_id,
            kind,
        }
    }

    /// The day the id was stamped on, UTC. Read back out of the id rather than
    /// held beside it, so the folder and the object cannot name two days.
    pub fn date(&self) -> Date {
        let (seconds, _) = self
            .id
            .get_timestamp()
            .expect("a v7 id carries its timestamp")
            .to_unix();
        // UTC, not the sender's offset: a key naming a local day would sort two
        // pools' uploads of the same instant into different folders. A v7
        // timestamp is a millisecond count, so every id this stamps is inside
        // `OffsetDateTime`'s range.
        OffsetDateTime::from_unix_timestamp(seconds as i64)
            .expect("a v7 millisecond is a representable instant")
            .date()
    }

    pub fn to_key(&self) -> String {
        let date = self.date();
        format!(
            "{KEY_PREFIX}{year:04}-{month:02}-{day:02}/{id}-{pool}-{agent}-{kind}{KEY_SUFFIX}",
            year = date.year(),
            month = u8::from(date.month()),
            day = date.day(),
            id = self.id,
            pool = self.pool_id,
            agent = self.agent_id,
            kind = self.kind,
        )
    }

    pub fn parse(key: &str) -> Result<ObjectName, ObjectNameError> {
        let refuse = |reason: String| ObjectNameError {
            key: key.to_string(),
            reason,
        };
        let [prefix, date, file] = *key.split('/').collect::<Vec<_>>() else {
            return Err(refuse("expected v1/<date>/<file>".to_string()));
        };
        let schema = KEY_PREFIX.trim_end_matches('/');
        if prefix != schema {
            return Err(refuse(format!(
                "schema prefix is {prefix:?}, not {schema:?}"
            )));
        }
        let stem = file
            .strip_suffix(KEY_SUFFIX)
            .ok_or_else(|| refuse(format!("{file:?} is not a {KEY_SUFFIX} object")))?;
        // Split from the left past the id, whose own dashes are at fixed
        // positions, then from the right for the kind: an agent id holds dashes
        // too, and a pool id holds none.
        let (id, rest) = stem
            .split_at_checked(uuid::fmt::Hyphenated::LENGTH)
            .ok_or_else(|| refuse(format!("{stem:?} is shorter than a uuid")))?;
        let id = Uuid::try_parse(id).map_err(|error| refuse(format!("id {id:?}: {error}")))?;
        // `date()` reads the day out of the id, which only a v7 carries; any
        // other version has to be refused here or it panics there.
        if id.get_version() != Some(Version::SortRand) {
            return Err(refuse(format!("id {id} is not a uuid v7")));
        }
        let rest = rest
            .strip_prefix('-')
            .ok_or_else(|| refuse(format!("{stem:?} is not <id>-<pool>-<agent>-<kind>")))?;
        let (pool, rest) = rest
            .split_once('-')
            .ok_or_else(|| refuse(format!("{rest:?} is not <pool>-<agent>-<kind>")))?;
        let (agent, kind) = rest
            .rsplit_once('-')
            .ok_or_else(|| refuse(format!("{rest:?} is not <agent>-<kind>")))?;
        let name = ObjectName {
            id,
            pool_id: PoolId::from_bech32(pool).map_err(|error| refuse(error.to_string()))?,
            agent_id: AgentId::parse(agent)
                .map_err(|error: AgentIdError| refuse(error.to_string()))?,
            kind: Kind::parse(kind)
                .ok_or_else(|| refuse(format!("{kind:?} is not a payload kind")))?,
        };
        // The folder repeats the id's day, and the id is the only version this
        // reads a day out of; a key where the two disagree was written by
        // something other than `to_key`.
        if name.to_key() != key {
            return Err(refuse(format!(
                "the {date} folder is not where {id} was stamped"
            )));
        }
        Ok(name)
    }
}

/// A stored object read back: its bytes, the two metadata headers written
/// beside them, and the name it was filed under. `verify` is what reconciles
/// the three.
#[derive(Debug)]
pub struct FetchedObject {
    pub name: ObjectName,
    pub vkey: VerifyingKey,
    pub signature: Signature,
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
    /// The archive answered that it holds no such key. Its own variant because
    /// it is the one fetch failure that is the client's mistake rather than
    /// the archive's, and the download route turns it into a 404.
    #[error("no object {key}")]
    NoSuchObject { key: String },
    #[error("fetching {key}: {reason}")]
    Fetch { key: String, reason: String },
    /// Not `Fetch`: no key can be read off this endpoint and no retry changes
    /// that. Both answer 503, so this exists for the operator's log line.
    #[error("{endpoint} answered a GET with no Content-Length")]
    EndpointUnusable { endpoint: String },
    #[error("storing {key}: {reason} (after {attempts} attempts)")]
    Upload {
        key: String,
        attempts: u32,
        reason: String,
    },
    #[error("listing the archive: {reason}")]
    List { reason: String },
}

/// The ingest half. Separate from `List` because the two are used by different
/// commands and neither needs the other: `Intake` only ever stores.
pub trait Store {
    /// Store the submission. Returning `Ok` is what lets the server ACK, so
    /// an implementation returns only once the bytes are durable (ADR 0004).
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError>;
}

/// One bounded page of an archive's keys, in key order.
#[derive(Debug)]
pub struct Page {
    pub keys: Vec<String>,
    /// There is more after the last key. Reported rather than implied: a
    /// caller reading a short page as the whole archive would silently miss
    /// everything past it.
    pub truncated: bool,
}

/// The read-back half, listed by `verify::audit` and paged by the developer
/// route.
pub trait List {
    /// Where this archive is, for a message about its listing. An operator
    /// told a listing came back empty has to tell a mistyped location from a
    /// genuinely empty one, and this is what they tell it by.
    fn location(&self) -> String;

    /// Hand every object key held to `visit`, in no particular order, as the
    /// listing produces them. A visitor rather than a `Vec` so that a caller
    /// folding the listing down — `rebuild` keeps one name per pool — never
    /// holds a whole bucket's keys at once.
    ///
    /// `E` is the caller's own error, so a visitor that fails for its own
    /// reasons stops the listing without a detour through `ArchiveError`.
    fn for_each_key<E: From<ArchiveError>>(
        &self,
        visit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E>;

    /// The keys starting with `prefix` that sort after `after`, in key order,
    /// at most `max_keys` of them. Both filters read the key, which is what
    /// makes them a day-and-pool filter and a page cursor at once: an empty
    /// `prefix` is the whole archive and an empty `after` its start.
    ///
    /// One page, never more: the bound is the client's, and a caller that
    /// wants the next page passes the last key back as `after`.
    fn page(&self, prefix: &str, after: &str, max_keys: NonZeroU32) -> Result<Page, ArchiveError>;

    /// The whole listing at once, for a caller that needs the keys after the
    /// listing is closed.
    fn keys(&self) -> Result<Vec<String>, ArchiveError> {
        let mut keys = Vec::new();
        self.for_each_key(|key| -> Result<(), ArchiveError> {
            keys.push(key.to_string());
            Ok(())
        })?;
        Ok(keys)
    }
}

/// The inverse of `Store::store`, read by `verify::audit`. Separate from
/// `Store` because storing and reading back are different privileges — the
/// ingest path never fetches.
pub trait Fetch {
    fn fetch(&self, key: &str) -> Result<FetchedObject, ArchiveError>;
}

/// An object's bytes alone, which is all the download endpoint hands back: a
/// developer verifies the signature over these, so anything but the stored
/// bytes verbatim is unverifiable. Separate from `Fetch` because the metadata
/// `Fetch` reconciles is what a filesystem archive cannot answer, and
/// downloading does not need it.
pub trait Bytes {
    /// The object open for reading. A reader rather than a `Vec` because
    /// nothing bounds an object already in the archive — one written under an
    /// older, wider limit is read whole otherwise (metsuke-4zo.72).
    fn reader(&self, key: &str) -> Result<ObjectStream, ArchiveError>;
}

/// An object being read out of the archive. The length is not optional: a
/// download that could not state one would have to answer chunked, and then
/// the copy granularity would be a size the client sees. An archive that will
/// not say how much it holds fails here instead.
pub struct ObjectStream {
    /// What it is being read from. A read that fails once the answer has
    /// started can only be reported to the log, and this is what names it
    /// there — the download knows no client beyond the one credential.
    pub key: String,
    pub length: u64,
    pub reader: Box<dyn io::Read + Send>,
}

/// Objects as files under a root directory, keyed exactly as S3 keys them.
///
/// Implements `Bytes` but not `Fetch`: the metadata `Fetch` answers would need
/// a sidecar, and a sidecar under the same prefix comes back from
/// `for_each_key` as an object whose key nothing can parse. The bytes need no
/// sidecar, so a download serves off either archive kind.
pub struct FilesystemArchive {
    root: PathBuf,
}

impl FilesystemArchive {
    pub fn new(root: &Path) -> Self {
        FilesystemArchive {
            root: root.to_path_buf(),
        }
    }

    /// Visit every object under `relative`, a path already below the archive
    /// root, so what `visit` is handed is the key and not the local path.
    fn walk<E: From<ArchiveError>>(
        &self,
        relative: &Path,
        visit: &mut impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
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
                self.walk(&path, visit)?;
            } else {
                let key = path.to_str().ok_or_else(|| ArchiveError::List {
                    reason: format!("{} is not a UTF-8 object key", path.display()),
                })?;
                visit(key)?;
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

impl Store for FilesystemArchive {
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
}

impl Bytes for FilesystemArchive {
    fn reader(&self, key: &str) -> Result<ObjectStream, ArchiveError> {
        let refuse = |reason: String| ArchiveError::Fetch {
            key: key.to_string(),
            reason,
        };
        // Parsed before it is joined: a key that is not an object name is the
        // only way a path outside the root could be reached, and `parse`
        // admits nothing but `v1/<pool>/<date>/<file>`.
        let name = ObjectName::parse(key).map_err(|error| refuse(error.to_string()))?;
        let file =
            fs::File::open(self.root.join(name.to_key())).map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => ArchiveError::NoSuchObject {
                    key: key.to_string(),
                },
                _ => refuse(error.to_string()),
            })?;
        // The length off the open handle, not off a separate stat: a file
        // replaced between the two would be answered with the other one's.
        let length = file
            .metadata()
            .map_err(|error| refuse(error.to_string()))?
            .len();
        Ok(ObjectStream {
            key: key.to_string(),
            length,
            reader: Box::new(file),
        })
    }
}

impl List for FilesystemArchive {
    fn location(&self) -> String {
        self.root.display().to_string()
    }

    fn for_each_key<E: From<ArchiveError>>(
        &self,
        mut visit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        // Only a root that is not there yet is an empty archive. A root that
        // exists and cannot be read must fail: reported as empty it would
        // rebuild an index short of every object the archive still holds. What
        // a *mistyped* root means is the caller's to judge —
        // `rebuild::EmptyArchive`.
        if let Err(error) = fs::read_dir(&self.root) {
            return match error.kind() {
                io::ErrorKind::NotFound => Ok(()),
                _ => Err(unreadable(&self.root, &error).into()),
            };
        }
        self.walk(Path::new(""), &mut visit)
    }

    /// The whole tree walked and sorted per request. A filesystem archive is
    /// the single-host deployment, where the corpus is small enough that the
    /// sort costs less than a second index to keep true.
    fn page(&self, prefix: &str, after: &str, max_keys: NonZeroU32) -> Result<Page, ArchiveError> {
        let mut keys = Vec::new();
        self.for_each_key(|key| -> Result<(), ArchiveError> {
            if key.starts_with(prefix) && key > after {
                keys.push(key.to_string());
            }
            Ok(())
        })?;
        keys.sort();
        Ok(bounded(keys, max_keys))
    }
}

/// Cut a page down to the bound, saying whether the bound cut it.
fn bounded(mut keys: Vec<String>, max_keys: NonZeroU32) -> Page {
    let truncated = keys.len() > max_keys.get() as usize;
    keys.truncate(max_keys.get() as usize);
    Page { keys, truncated }
}
