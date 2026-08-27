//! The fetch binary: one command against one archive, then exit. Keys go to
//! stdout, so a run pipes into whatever reads the corpus next; everything
//! about the run itself goes to stderr.

use std::path::Path;
use std::time::Duration;

use metsuke_fetch::cli::{Args, ArgsError, Command};
use metsuke_fetch::pull::Archive;
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
    let Args {
        command,
        access,
        prefix,
        selection,
    } = Args::parse(std::env::args().skip(1))?;
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
    let (report, what) = match command {
        Command::List => (
            sync::list(&archive, &filters, |key| println!("{key}"))?,
            "listed".to_string(),
        ),
        Command::Sync { state, into } => {
            let destination = Destination {
                into: &into,
                state: &state,
            };
            let report = sync::run(&archive, &filters, &destination, |key| println!("{key}"))?;
            let what = format!("into {}, {} bytes", into.display(), report.bytes);
            (report, what)
        }
    };
    eprintln!(
        "{} objects {what}; {} outside the filters, {} this build cannot name",
        report.objects, report.passed, report.unnameable
    );
    Ok(())
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
