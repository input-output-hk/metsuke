//! The fetch binary: one command against one archive, then exit. Keys go to
//! stdout, so a run pipes into whatever reads the corpus next; everything
//! about the run itself goes to stderr.

use std::path::Path;
use std::time::Duration;

use metsuke_fetch::cli::{Args, ArgsError, Command, Invocation, USAGE, VERSION};
use metsuke_fetch::pull::Archive;
use metsuke_fetch::recipe;
use metsuke_fetch::select::Filters;
use metsuke_fetch::sync::{self, Destination, SyncError};

#[derive(Debug, thiserror::Error)]
enum Fatal {
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error("cannot read the password file {path}: {source}")]
    Password {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The server would refuse an empty credential, and saying so here names
    /// the file instead of reporting a 401 the operator has to trace back.
    #[error("the password file {path} is empty")]
    EmptyPassword { path: String },
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// Not a failure to sync: the run did what it could and the objects it
    /// refused are named above this. The code is what a script reads.
    #[error("{count} object(s) did not verify and were not written")]
    NotWritten { count: usize },
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("metsuke-fetch stopped: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Fatal> {
    match Invocation::parse(std::env::args().skip(1))? {
        // Asked for, so it is the run's answer and goes to stdout; the same
        // text reaching stderr under an error is a refusal, not an answer.
        Invocation::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Invocation::Version => {
            println!("{VERSION}");
            Ok(())
        }
        Invocation::Run(args) => fetch(*args),
    }
}

fn fetch(args: Args) -> Result<(), Fatal> {
    let Args {
        command,
        access,
        prefix,
        selection,
        verification,
    } = args;
    // Before anything that can fail, so a run that stops on its password file
    // or on the server has still named the build it stopped from.
    eprintln!("metsuke-fetch {VERSION} against {}", access.server);
    let archive = Archive::new(
        &access.server,
        &access.user,
        &password(&access.password_file)?,
        Duration::from_millis(access.timeout_ms.get()),
    );
    let filters = Filters {
        prefix: &prefix,
        selection: &selection,
    };
    let (report, what, read) = match command {
        Command::List => (
            sync::list(&archive, &filters, |key| println!("{key}"))?,
            "listed".to_string(),
            None,
        ),
        Command::Sync { state, into } => {
            let destination = Destination {
                into: &into,
                state: &state,
            };
            let report = sync::run(&archive, &filters, &destination, verification, |key| {
                println!("{key}")
            })?;
            let what = format!("into {}, {} bytes", into.display(), report.bytes);
            (report, what, Some(recipe::read(&into, selection.kind)))
        }
    };
    // Before the summary, because a key nobody may trust is the news and the
    // counts are the context for it.
    for rejected in &report.rejected {
        eprintln!("not written: {} {}", rejected.key, rejected.reason);
    }
    eprintln!(
        "{} objects {what}; {} verified, {} unverifiable, {} not written; \
         {} outside the filters, {} this build cannot name",
        report.objects,
        report.verified,
        report.unverifiable,
        report.rejected.len(),
        report.passed,
        report.unnameable
    );
    // Printed rather than left to the reader, because the read a consumer
    // reaches for first is lossy here. See docs/reading-the-archive.md.
    if let Some(read) = read {
        eprintln!("read them with: duckdb -c \"select * from {read}\"");
    }
    // The objects that did land are on disk and the cursor is past every key
    // this run saw, so the exit code is the only thing left to say that what a
    // reader will find is short of what was listed.
    match report.rejected.is_empty() {
        true => Ok(()),
        false => Err(Fatal::NotWritten {
            count: report.rejected.len(),
        }),
    }
}

/// The account's password, with the trailing newline an editor leaves trimmed
/// off.
fn password(path: &Path) -> Result<String, Fatal> {
    let named = || path.display().to_string();
    let password = std::fs::read_to_string(path).map_err(|source| Fatal::Password {
        path: named(),
        source,
    })?;
    let password = password.trim_end_matches(['\r', '\n']).to_string();
    match password.is_empty() {
        true => Err(Fatal::EmptyPassword { path: named() }),
        false => Ok(password),
    }
}
