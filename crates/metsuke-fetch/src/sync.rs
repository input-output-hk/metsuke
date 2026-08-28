//! The sync: page the listing from the cursor, write each object down under
//! the key it is filed as, advance the cursor behind it. There is no manifest
//! and nothing to reconcile. The objects on disk plus the cursor are the whole
//! state of a sync.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::cursor::{Cursor, CursorError};
use crate::pull::{Archive, PullError};
use crate::select::{Filters, Selected};
use crate::staged;

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
    mut landed: impl FnMut(&str),
) -> Result<Report, SyncError> {
    let mut cursor = Cursor::read(destination.state, filters)?;
    let mut report = Report::default();
    for page in Pages::from(archive, filters.prefix, &cursor.after) {
        for key in page? {
            match filters.selection.selects(&key) {
                Selected::Yes => {
                    report.bytes += download(archive, destination.into, &key)?;
                    report.objects += 1;
                    landed(&key);
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
fn download(archive: &Archive, into: &Path, key: &str) -> Result<u64, SyncError> {
    let path = destination(into, key).ok_or_else(|| SyncError::NotAKey {
        key: key.to_string(),
    })?;
    // The pull failure travels out as the `io::Error` the staging path takes
    // and is taken back off it here, so a refused download reports the
    // server's reason rather than a write that stopped.
    staged::replacing(&path, |file| {
        archive.object(key, file).map_err(io::Error::other)
    })
    .map_err(|error| match error.downcast::<PullError>() {
        Ok(pull) => SyncError::Pull(pull),
        Err(source) => SyncError::Write { path, source },
    })
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
