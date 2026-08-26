//! Reconstruct the index from the archive listing (ADR 0005). The bucket is
//! the source of truth, so this is what a server whose disk was lost runs
//! before it serves a listing again.

use crate::archive::{ArchiveError, List, ObjectName, ObjectNameError};
use crate::cli::ALLOW_EMPTY;
use crate::index::{Index, IndexError};

/// What a listing with no objects in it means. A mistyped or unmounted
/// `archive.root` and a bucket that has never been written to list the same
/// way, and the operator running this has already lost their index: the
/// ambiguity is refused unless they say which one it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyArchive {
    Refuse,
    Accept,
}

/// What the rebuild found, for the operator to compare against what they
/// expected to be in the bucket.
#[derive(Debug)]
pub struct RebuiltIndex {
    pub objects: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    ObjectName(#[from] ObjectNameError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(
        "{archive} listed no objects, so the index would stay empty \
         (pass {ALLOW_EMPTY} if that archive really is empty)"
    )]
    Empty { archive: String },
}

/// Write one row per stored object, as the listing produces them.
pub fn rebuild(
    archive: &impl List,
    index: &mut Index,
    empty: EmptyArchive,
) -> Result<RebuiltIndex, RebuildError> {
    let mut objects = 0;
    archive.for_each_key(|key| -> Result<(), RebuildError> {
        objects += 1;
        index.record(&ObjectName::parse(key)?)?;
        Ok(())
    })?;
    if objects == 0 && empty == EmptyArchive::Refuse {
        return Err(RebuildError::Empty {
            archive: archive.location(),
        });
    }
    Ok(RebuiltIndex { objects })
}
