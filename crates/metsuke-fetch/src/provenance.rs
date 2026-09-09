//! What bar the objects in a download directory were written under, recorded
//! in the directory itself.
//!
//! `cursor` already refuses a run whose bar differs from the one its state
//! file was taken under, and says why: raising the bar leaves objects on disk
//! the new bar would never have written, so the directory stops matching what
//! the flags say it holds. That reasoning is about the directory, and the
//! check it justifies sat on the state file, which is a narrower thing: one
//! state file per set of filters, and they may share one `--into`
//! (`cli::USAGE`). Two state files at two bars into one directory left it
//! holding proven and assumed objects with nothing to tell them apart, and
//! nothing refused it.
//!
//! So the same guarantee, at the granularity the argument is about: a
//! directory is written under one bar, and a run that asks for another is
//! refused before it downloads anything.

use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::staged;
use crate::sync::{Insist, Verification};

/// The record's name inside the download directory. Dotted and at the root, so
/// no glob a reader writes over the tree can reach it: every documented one
/// names a day folder and an object suffix (docs/reading-the-archive.md).
pub const FILE: &str = ".metsuke-verification.json";

/// What every object under this directory was held to.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Verified {
    pub insist: Insist,
    pub max_object_bytes: NonZeroU64,
}

impl Verified {
    fn of(verification: &Verification) -> Verified {
        Verified {
            insist: verification.insist,
            max_object_bytes: verification.max_object_bytes,
        }
    }

    /// One line, for a refusal that has to show two of these.
    fn describe(&self) -> String {
        format!(
            "max-object-bytes {}, {}",
            self.max_object_bytes, self.insist
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} does not parse: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    /// Refused before anything is downloaded, so a directory never gains an
    /// object under a bar it does not record.
    #[error(
        "{path} records another bar\n  \
         holds: {held}\n  \
         asked: {asked}\n  \
         name a directory of its own for this one, or ask for what it holds"
    )]
    OtherBar {
        path: PathBuf,
        held: String,
        asked: String,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Claim `into` for this run's bar, or refuse the run.
///
/// A directory with no record is claimed: it is either empty or it predates
/// this record, and there is nothing to read the bar of what is already there
/// off. From the claim onward the directory is held to it.
pub fn claim(into: &Path, verification: &Verification) -> Result<(), ProvenanceError> {
    let path = into.join(FILE);
    let asked = Verified::of(verification);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return write(&path, &asked),
        Err(source) => {
            return Err(ProvenanceError::Read {
                path: path.clone(),
                source,
            });
        }
    };
    let held: Verified =
        serde_json::from_str(&text).map_err(|error| ProvenanceError::Unreadable {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    match held == asked {
        true => Ok(()),
        // Any difference, for the reason `cursor::Cursor::read` gives: a
        // lower bar adds objects the recorded one refused, and a higher one
        // leaves the directory holding objects it would never have written.
        false => Err(ProvenanceError::OtherBar {
            held: held.describe(),
            asked: asked.describe(),
            path,
        }),
    }
}

fn write(path: &Path, verified: &Verified) -> Result<(), ProvenanceError> {
    let body = serde_json::to_vec_pretty(verified).expect("two fields serialize");
    staged::replacing(path, |file| std::io::Write::write_all(file, &body)).map_err(|source| {
        ProvenanceError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}
