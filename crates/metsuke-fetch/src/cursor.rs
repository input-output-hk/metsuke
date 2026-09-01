//! Where a sync got to: the key of the last object it wrote whole. Held in a
//! file of its own so an interrupted run resumes from it rather than listing
//! the archive from the start.
//!
//! Replaced whole rather than edited, so a run killed mid-write leaves either
//! the old cursor or the new one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::select::{Filters, Selection};
use crate::staged;

/// The state file's whole content. The filters are in it because a cursor only
/// means anything against the listing it was taken from: a run advances past
/// every key it saw, downloaded or filtered out, so the same cursor read under
/// other filters would skip objects it never fetched.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub prefix: String,
    pub selection: Selection,
    /// The last key seen. Empty is the archive's start, which is also what a
    /// state file that does not exist yet means.
    pub after: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("cannot read the state file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the state file {path} does not parse: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    /// Refused rather than reset: a cursor taken under `v1/2026-08-01/` or
    /// under one pool, read under wider filters, would skip everything before
    /// it and report a whole sync.
    #[error(
        "the state file {path} is for other filters\n  \
         holds: {held}\n  \
         asked: {asked}\n  \
         name a state file of its own for these filters"
    )]
    OtherFilters {
        path: PathBuf,
        held: String,
        asked: String,
    },
    #[error("cannot write the state file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Cursor {
    /// The cursor `path` holds for `filters`, or a fresh one when there is no
    /// state file yet.
    pub fn read(path: &Path, filters: &Filters<'_>) -> Result<Cursor, CursorError> {
        let asked = || format!("prefix {:?} and {}", filters.prefix, filters.selection);
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Cursor {
                    prefix: filters.prefix.to_string(),
                    selection: filters.selection.clone(),
                    after: String::new(),
                });
            }
            Err(source) => {
                return Err(CursorError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let held: Cursor =
            serde_json::from_str(&text).map_err(|error| CursorError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        match held.prefix == filters.prefix && held.selection == *filters.selection {
            true => Ok(held),
            false => Err(CursorError::OtherFilters {
                path: path.to_path_buf(),
                held: format!("prefix {:?} and {}", held.prefix, held.selection),
                asked: asked(),
            }),
        }
    }

    /// Move the cursor to `key` and write it down.
    pub fn advance(&mut self, path: &Path, key: &str) -> Result<(), CursorError> {
        self.after = key.to_string();
        self.write(path)
    }

    fn write(&self, path: &Path) -> Result<(), CursorError> {
        let json = serde_json::to_vec(self).expect("a prefix, three filters and a key serialize");
        staged::replacing(path, |file| io::Write::write_all(file, &json)).map_err(|source| {
            CursorError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }
}
