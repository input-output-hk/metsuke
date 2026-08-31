//! The sync: page the listing from the cursor, write each object down under
//! the key it is filed as, advance the cursor behind it. There is no manifest
//! and nothing to reconcile. The objects on disk plus the cursor are the whole
//! state of a sync.

use std::io;
use std::io::Write as _;
use std::num::NonZeroU64;
use std::path::{Component, Path, PathBuf};

use crate::cursor::{Cursor, CursorError};
use crate::pull::{Archive, Object, PullError};
use crate::select::{Filters, Selected};
use crate::staged;
use metsuke_wire::key::ObjectName;

/// What one run moved, and what it did not. `passed` and `unnameable` are
/// counted rather than dropped quietly: a filter that selected nothing and a
/// bucket holding objects this build cannot name look the same otherwise.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub objects: u64,
    pub bytes: u64,
    /// Listed, and outside the selection.
    pub passed: u64,
    /// Listed, and not a key `ObjectName::parse` reads, so no selection could
    /// answer for it.
    pub unnameable: u64,
    /// Landed and checked against the pair the download carried.
    pub verified: u64,
    /// Landed without both halves of the check: no pair to check it against,
    /// which is every object from an archive that stores none (`pull::Object`),
    /// or a signature that stands under a key naming no pool (`checked`).
    /// Counted rather than refused, or a run against one would download
    /// nothing; `--require-verified` is what turns it into a refusal.
    pub unverifiable: u64,
    /// Named rather than counted: an object nobody may trust is not a number,
    /// it is a key somebody has to look at. Each of these was not written.
    pub rejected: Vec<Rejected>,
}

/// An object the run refused to write down, and why.
#[derive(Debug, PartialEq)]
pub struct Rejected {
    pub key: String,
    pub reason: String,
}

/// What this run will hold and what it insists on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verification {
    /// Ceiling on one object, because checking it means holding it
    /// (`pull::Archive::object`).
    pub max_object_bytes: NonZeroU64,
    /// Whether an object that carries no pair is a refusal rather than a
    /// count. Off by default: the archive a developer reaches for is usually
    /// S3, which carries one, but a filesystem archive never does and this
    /// tool is how its objects are read.
    pub require_verified: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Pull(#[from] PullError),
    #[error(transparent)]
    Cursor(#[from] CursorError),
    /// The keys are the server's, and this is what keeps one of them from
    /// naming a path outside the directory the operator pointed at.
    #[error(
        "{key:?} is not a relative object key, so it names no file under the download directory"
    )]
    NotAKey { key: String },
    #[error("the listing did not advance past {after:?}, so the server is not reading the cursor")]
    Stuck { after: String },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Everything one sync needs beside the archive.
pub struct Destination<'a> {
    /// Where objects land, one file per key, directories as the key spells
    /// them.
    pub into: &'a Path,
    pub state: &'a Path,
}

/// Download every object under `prefix` that the cursor has not seen, reporting
/// each key as it lands. Resuming is the same call: the cursor says where the
/// last run stopped, and a run that stops again leaves it naming the last
/// object written whole.
pub fn run(
    archive: &Archive,
    filters: &Filters<'_>,
    destination: &Destination<'_>,
    verification: Verification,
    mut landed: impl FnMut(&str),
) -> Result<Report, SyncError> {
    let mut cursor = Cursor::read(destination.state, filters)?;
    let mut report = Report::default();
    for page in Pages::from(archive, filters.prefix, &cursor.after) {
        for key in page? {
            match filters.selection.selects(&key) {
                Selected::Yes => {
                    match download(archive, destination.into, &key, verification)? {
                        Landed::Verified(bytes) => {
                            report.bytes += bytes;
                            report.objects += 1;
                            report.verified += 1;
                            landed(&key);
                        }
                        Landed::Unverifiable(bytes) => {
                            report.bytes += bytes;
                            report.objects += 1;
                            report.unverifiable += 1;
                            landed(&key);
                        }
                        // Not handed to `landed`: that is the run's list of
                        // what a reader will find on disk, and this is not on
                        // it.
                        Landed::Rejected(reason) => report.rejected.push(Rejected {
                            key: key.clone(),
                            reason,
                        }),
                    }
                }
                Selected::No => report.passed += 1,
                Selected::Unnameable => report.unnameable += 1,
            }
            // After the object is on disk: a cursor ahead of its objects skips
            // them for good. A key the selection passed over is behind the
            // cursor too. It was listed, and re-listing it would download
            // nothing.
            cursor.advance(destination.state, &key)?;
        }
    }
    Ok(report)
}

/// The keys the filters select, reported as the listing produces them and
/// nothing downloaded.
pub fn list(
    archive: &Archive,
    filters: &Filters<'_>,
    mut found: impl FnMut(&str),
) -> Result<Report, SyncError> {
    let mut report = Report::default();
    for page in Pages::from(archive, filters.prefix, "") {
        for key in page? {
            match filters.selection.selects(&key) {
                Selected::Yes => {
                    report.objects += 1;
                    found(&key);
                }
                Selected::No => report.passed += 1,
                Selected::Unnameable => report.unnameable += 1,
            }
        }
    }
    Ok(report)
}

/// The listing walked page by page, which is the one thing `run` and `list`
/// share. Each page resumes from the last key of the one before it, so what
/// bounds a page is the server's and never this side's.
struct Pages<'a> {
    archive: &'a Archive,
    prefix: &'a str,
    after: String,
    /// The last page has been handed out. Set by `truncated`, so the walk
    /// stops on the server's answer rather than on a short page, and by a
    /// failure, so a caller that keeps iterating past one gets nothing.
    done: bool,
}

impl<'a> Pages<'a> {
    fn from(archive: &'a Archive, prefix: &'a str, after: &str) -> Pages<'a> {
        Pages {
            archive,
            prefix,
            after: after.to_string(),
            done: false,
        }
    }
}

impl Iterator for Pages<'_> {
    type Item = Result<Vec<String>, SyncError>;

    fn next(&mut self) -> Option<Result<Vec<String>, SyncError>> {
        if self.done {
            return None;
        }
        let page = match self.archive.page(self.prefix, &self.after) {
            Ok(page) => page,
            Err(error) => {
                self.done = true;
                return Some(Err(error.into()));
            }
        };
        self.done = true;
        // The route reads `after` exclusively, so a page ending at or before
        // the cursor is not the next page, and asking again would hand back
        // the same one for as long as the walk ran. An empty page the server
        // calls truncated is the same shape: it says there is more and hands
        // none of it over, so taking it as the end would report a whole sync.
        match page.keys.last().cloned() {
            Some(last) if last > self.after => {
                self.after = last;
                self.done = !page.truncated;
                Some(Ok(page.keys))
            }
            None if !page.truncated => None,
            _ => Some(Err(SyncError::Stuck {
                after: std::mem::take(&mut self.after),
            })),
        }
    }
}

/// One object written under `into`, verbatim (`staged::replacing`).
/// What one object's download came to: the bytes it added, or the reason it
/// was not written. Not a `SyncError`: an object nobody may trust is this
/// object's news and not the run's, so the sync goes on and the caller counts
/// it (`Report::rejected`).
enum Landed {
    Verified(u64),
    Unverifiable(u64),
    Rejected(String),
}

fn download(
    archive: &Archive,
    into: &Path,
    key: &str,
    verification: Verification,
) -> Result<Landed, SyncError> {
    let path = destination(into, key).ok_or_else(|| SyncError::NotAKey {
        key: key.to_string(),
    })?;
    let object = match archive.object(key, verification.max_object_bytes) {
        Ok(object) => object,
        // An object over the bound is the run's to report and not to stop for,
        // the same as one that does not verify: the operator raises the flag
        // or leaves it, and the rest of the archive still syncs.
        Err(oversized @ PullError::Oversized { .. }) => {
            return Ok(Landed::Rejected(oversized.to_string()));
        }
        Err(error) => return Err(SyncError::Pull(error)),
    };
    let landed = match checked(key, &object, verification.require_verified) {
        Err(reason) => return Ok(Landed::Rejected(reason)),
        Ok(landed) => landed,
    };
    // Written only once it is decided: an object nobody may trust never
    // reaches the download directory, so a reader globbing the tree cannot
    // pick one up without having been told.
    staged::replacing(&path, |file| file.write_all(&object.bytes))
        .map_err(|source| SyncError::Write { path, source })?;
    Ok(landed(object.bytes.len() as u64))
}

/// Whether these bytes may be written down, and as what. Two halves, and both
/// are needed: the signature says a holder of that key sealed exactly these
/// bytes, and the hash says which pool that key speaks for. Checking one alone
/// takes an object a stranger signed, or one filed under a pool that never
/// sent it.
///
/// Only a cold key has the second half. A Leios key names no pool, and what
/// tied it to one was the server's roster at the moment the object was
/// accepted, which this tool does not keep and cannot reconstruct (ADR 0011).
/// Such an object lands with its signature checked and its filing taken on the
/// server's word, which is what `Unverifiable` says.
fn checked(
    key: &str,
    object: &Object,
    require_verified: bool,
) -> Result<fn(u64) -> Landed, String> {
    let Some(attestation) = &object.attestation else {
        return match require_verified {
            true => Err("carries no key and signature to check it with".to_string()),
            false => Ok(Landed::Unverifiable),
        };
    };
    if !attestation.verifies(&object.bytes) {
        return Err("the signature does not stand over the bytes as downloaded".to_string());
    }
    let name = ObjectName::parse(key).map_err(|error| error.to_string())?;
    let Some(signer) = attestation.attributes() else {
        return match require_verified {
            true => Err("signed by a Leios key, which names no pool to check \
                         the filing against"
                .to_string()),
            false => Ok(Landed::Unverifiable),
        };
    };
    if signer != name.pool_id {
        return Err(format!(
            "signed by {signer}, and filed under {}",
            name.pool_id
        ));
    }
    Ok(Landed::Verified)
}

/// The file under `into` an object key names, or `None` for a key that names
/// anything else: an absolute path, a parent directory, or nothing at all.
pub fn destination(into: &Path, key: &str) -> Option<PathBuf> {
    let key = Path::new(key);
    let named = key
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    match named && key.components().next().is_some() {
        true => Some(into.join(key)),
        false => None,
    }
}
