//! Where a sync got to: the key of the last object it wrote whole. Held in a
//! file of its own so an interrupted run resumes from it rather than listing
//! the archive from the start.
//!
//! Replaced whole rather than edited, so a run killed mid-write leaves either
//! the old cursor or the new one.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::select::{Filters, Selection};
use crate::staged;
use crate::sync::{Insist, Verification};

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
    /// The size bound the run held objects to. Here for the same reason
    /// `insist` is, and it was missed when that one was fixed: an object
    /// refused for exceeding this advances the cursor like any other refusal,
    /// so a cursor taken under one bound and read under a higher one resumes
    /// past objects the first refused and the second would have taken. The
    /// refusal's own message tells an operator to raise the flag and re-run,
    /// which is exactly the sequence that lost them.
    ///
    /// Defaulted to the shipped bound, so a state file written before this
    /// field reads as a run that never set the flag, and is refused for one
    /// that did.
    #[serde(default = "default_max_object_bytes")]
    pub max_object_bytes: NonZeroU64,
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
    /// Keys whose bytes did not verify, kept across runs.
    ///
    /// The cursor advances past a refusal like any other key, so without this
    /// the only record of one is the stderr of the run that found it: a second
    /// run over the same state file reaches none of them, finds nothing wrong
    /// and exits zero, which is what makes `sync || sync` or a timer that runs
    /// twice report a clean archive over objects nobody may trust.
    ///
    /// Only `sync::Fault::Verdict` keys. A key that is gone, one over the size
    /// bound, and one below the bar a `--require` flag set are all the run
    /// working as asked, and a list that filled up with those is one an
    /// operator learns to ignore.
    ///
    /// Cleared only when asked (`cli::Args::forget_unverified`): the remedy is
    /// outside this tool, so nothing here can see that it happened.
    ///
    /// A set, so recording one is not a scan of the ones already held. It
    /// serialises as the array it reads back from, sorted rather than in the
    /// order they were found, which is the order a listing walks anyway.
    #[serde(default)]
    pub unverified: BTreeSet<String>,
}

/// What a state file cannot say for itself: the bound a run that set no flag
/// held objects to.
fn default_max_object_bytes() -> NonZeroU64 {
    crate::cli::DEFAULT_MAX_OBJECT_BYTES
}

/// What a state file is for, as one line. Built in one place so `held` and
/// `asked` cannot describe two different shapes.
fn describe(
    prefix: &str,
    selection: &Selection,
    insist: Insist,
    max_object_bytes: NonZeroU64,
    from: Option<&str>,
) -> String {
    let day = match from {
        Some(from) => format!(", from {from:?}"),
        None => String::new(),
    };
    // Before the bar rather than after it, so the bar stays the last thing on
    // the line where there is no day, which is what reads best and what
    // `a_bar_a_state_file_predates_is_another_run` holds it to.
    format!("prefix {prefix:?}, {selection}, max-object-bytes {max_object_bytes}, {insist}{day}")
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
    /// The cursor `path` holds for `filters` under `verification`, or a fresh
    /// one when there is no state file yet.
    pub fn read(
        path: &Path,
        filters: &Filters<'_>,
        verification: &Verification,
    ) -> Result<Cursor, CursorError> {
        let (insist, max_object_bytes) = (verification.insist, verification.max_object_bytes);
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Cursor {
                    prefix: filters.prefix.to_string(),
                    selection: filters.selection.clone(),
                    insist,
                    max_object_bytes,
                    from: filters.days.from.clone(),
                    after: String::new(),
                    unverified: BTreeSet::new(),
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
        //
        // What the run was asked for, and only that. `after` moves and
        // `unverified` grows as a run goes, so comparing either would refuse
        // every run after the first, and for `unverified` refuse exactly the
        // state files that have something to report.
        match held.prefix == filters.prefix
            && held.selection == *filters.selection
            && held.insist == insist
            && held.max_object_bytes == max_object_bytes
            && held.from == filters.days.from
        {
            true => Ok(held),
            false => Err(CursorError::OtherFilters {
                path: path.to_path_buf(),
                held: describe(
                    &held.prefix,
                    &held.selection,
                    held.insist,
                    held.max_object_bytes,
                    held.from.as_deref(),
                ),
                asked: describe(
                    filters.prefix,
                    filters.selection,
                    insist,
                    max_object_bytes,
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

    /// Record a key whose bytes did not verify, and advance past it. One write,
    /// so a run killed between the two cannot leave the cursor past a key this
    /// list does not name.
    pub fn advance_unverified(&mut self, path: &Path, key: &str) -> Result<(), CursorError> {
        self.unverified.insert(key.to_string());
        self.advance(path, key)
    }

    /// Forget the recorded keys, which is the operator saying they have been
    /// dealt with. Written even when there were none, so the flag is
    /// idempotent.
    pub fn forget_unverified(&mut self, path: &Path) -> Result<usize, CursorError> {
        let forgotten = self.unverified.len();
        self.unverified.clear();
        self.write(path)?;
        Ok(forgotten)
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
