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
use crate::sync::Insist;

/// The state file's whole content. What a run was asked for is in it because a
/// cursor only means anything against that: a run advances past every key it
/// saw, downloaded, filtered out or refused alike, so the same cursor read
/// under anything else would skip objects it never fetched.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub prefix: String,
    pub selection: Selection,
    /// What the run insisted on. Here for the same reason the filters are: a
    /// refusal advances the cursor too, so a cursor taken under one bar and
    /// read under a lower one resumes past objects that bar refused and this
    /// one wants. Defaulted, so a state file written before this field reads as
    /// `Nothing` and is refused for any run that asks for more.
    #[serde(default)]
    pub insist: Insist,
    /// The first day the run was bounded to, as `select::Days` means it. Here
    /// because it relocates where the listing starts: a cursor taken from one
    /// day onward, read with no first day, would resume past everything
    /// before it. The last day is not here, and must not be: it only stops
    /// the walk, in the same direction the cursor moves, so nothing is ever
    /// passed over by it.
    #[serde(default)]
    pub from: Option<String>,
    /// The last key seen. Empty is the archive's start, which is also what a
    /// state file that does not exist yet means.
    pub after: String,
}

/// What a state file is for, as one line. Built in one place so `held` and
/// `asked` cannot describe two different shapes.
fn describe(prefix: &str, selection: &Selection, insist: Insist, from: Option<&str>) -> String {
    let day = match from {
        Some(from) => format!(", from {from:?}"),
        None => String::new(),
    };
    format!("prefix {prefix:?}, {selection}, {insist}{day}")
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
    /// Refused rather than reset: a cursor taken under `v1/2026-08-01/`, under
    /// one pool, or under a higher bar, read under anything wider would skip
    /// everything before it and report a whole sync.
    #[error(
        "the state file {path} is for another run\n  \
         holds: {held}\n  \
         asked: {asked}\n  \
         name a state file of its own for this one"
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
    /// The cursor `path` holds for `filters` at `insist`, or a fresh one when
    /// there is no state file yet.
    pub fn read(path: &Path, filters: &Filters<'_>, insist: Insist) -> Result<Cursor, CursorError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Cursor {
                    prefix: filters.prefix.to_string(),
                    selection: filters.selection.clone(),
                    insist,
                    from: filters.days.from.clone(),
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
        // Any difference and not just a lower bar: raising it leaves objects on
        // disk the new bar would never have written, so the directory stops
        // matching what the flags say it holds either way.
        match held.prefix == filters.prefix
            && held.selection == *filters.selection
            && held.insist == insist
            && held.from == filters.days.from
        {
            true => Ok(held),
            false => Err(CursorError::OtherFilters {
                path: path.to_path_buf(),
                held: describe(
                    &held.prefix,
                    &held.selection,
                    held.insist,
                    held.from.as_deref(),
                ),
                asked: describe(
                    filters.prefix,
                    filters.selection,
                    insist,
                    filters.days.from.as_deref(),
                ),
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
