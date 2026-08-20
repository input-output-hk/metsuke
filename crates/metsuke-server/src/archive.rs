//! The storage seam (ADR 0005). The filesystem implementation is what
//! `cargo test` runs against; the S3 one (ticket metsuke-4zo.7) is the same
//! trait.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use metsuke::envelope::{PoolId, Signature, VerifyingKey};
use time::OffsetDateTime;

/// One accepted submission: the bytes to store, plus the envelope metadata
/// the S3 implementation duplicates into object headers (ticket
/// metsuke-4zo.7). `FilesystemArchive` writes the bytes only.
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
    /// The ADR-0005 object key. Sorting by key groups a pool's uploads by
    /// day and orders them within it.
    pub fn object_key(&self) -> String {
        let date = self.timestamp.date();
        format!(
            "v1/{pool}/{year:04}-{month:02}-{day:02}/{unix}-{counter}.json.zst",
            pool = self.pool_id,
            year = date.year(),
            month = u8::from(date.month()),
            day = date.day(),
            unix = self.timestamp.unix_timestamp(),
            counter = self.counter,
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("storing {key}: {source}")]
    Io {
        key: String,
        #[source]
        source: io::Error,
    },
}

pub trait Archive {
    /// Store the submission. Returning `Ok` is what lets the server ACK, so
    /// an implementation returns only once the bytes are durable (ADR 0004).
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError>;
}

/// Objects as files under a root directory, keyed exactly as S3 keys them.
pub struct FilesystemArchive {
    root: PathBuf,
}

impl FilesystemArchive {
    pub fn new(root: &Path) -> Self {
        FilesystemArchive {
            root: root.to_path_buf(),
        }
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
}
