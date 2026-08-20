//! The ingest binary: startup fails loudly, then one loop serves submissions
//! until something it cannot serve through stops it. Both exit nonzero, so
//! systemd restarts rather than supervising a process that accepts nothing.
//! The subcommands are the exception: they run once against the same config
//! and exit, zero only if what they were asked to check holds.

use metsuke_server::archive::{Archive, FilesystemArchive};
use metsuke_server::cli::{Args, ArgsError, Command};
use metsuke_server::config::{ArchiveConfig, ConfigError, IngestConfig, S3Config, ServerConfig};
use metsuke_server::counters::{CounterError, CounterStore};
use metsuke_server::intake::Intake;
use metsuke_server::rebuild::{RebuildError, RebuiltIndex, rebuild};
use metsuke_server::s3::{S3Archive, S3Error};
use metsuke_server::verify::{Audit, AuditError, audit};
use metsuke_server::{ERR, INFO, http};
use rusty_s3::Credentials;

#[derive(Debug, thiserror::Error)]
enum Fatal {
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error("cannot read config {path}: {source}")]
    ReadConfig {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot open the counter database: {0}")]
    Counters(#[from] CounterError),
    #[error(transparent)]
    S3(#[from] S3Error),
    #[error("the S3 archive needs AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY in the environment")]
    MissingCredentials,
    #[error("cannot rebuild the index: {0}")]
    Rebuild(#[from] RebuildError),
    #[error("cannot audit the archive: {0}")]
    Audit(#[from] AuditError),
    #[error("a filesystem archive stores no metadata to re-verify against")]
    CannotVerifyFilesystem,
    #[error("{failed} objects did not verify, {unreadable} could not be read")]
    ArchiveNotVerified { failed: usize, unreadable: usize },
    #[error("the archive holds no objects, so nothing was verified")]
    ArchiveEmpty,
    #[error("cannot listen on {listen}: {reason}")]
    Listen { listen: String, reason: String },
    /// Mid-life, not startup: why it is fatal is `http::serve`.
    #[error("the listener stopped accepting: {0}")]
    Accept(#[from] std::io::Error),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{ERR}metsuke-server stopped: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Fatal> {
    let args = Args::parse(std::env::args().skip(1))?;
    let text = std::fs::read_to_string(&args.config).map_err(|source| Fatal::ReadConfig {
        path: args.config.display().to_string(),
        source,
    })?;
    let ServerConfig {
        listen,
        counters_path,
        archive,
        ingest,
    } = ServerConfig::from_toml(&text)?;
    let mut counters = CounterStore::open(&counters_path)?;
    match (args.command, archive) {
        (Command::Serve, ArchiveConfig::Filesystem { root }) => {
            serve(FilesystemArchive::new(&root), &listen, ingest, counters)
        }
        (Command::Serve, ArchiveConfig::S3(config)) => {
            serve(s3_archive(&config)?, &listen, ingest, counters)
        }
        (Command::RebuildIndex, ArchiveConfig::Filesystem { root }) => {
            report(rebuild(&FilesystemArchive::new(&root), &mut counters)?)
        }
        (Command::RebuildIndex, ArchiveConfig::S3(config)) => {
            report(rebuild(&s3_archive(&config)?, &mut counters)?)
        }
        (Command::VerifyArchive, ArchiveConfig::Filesystem { .. }) => {
            Err(Fatal::CannotVerifyFilesystem)
        }
        (Command::VerifyArchive, ArchiveConfig::S3(config)) => {
            verified(audit(&s3_archive(&config)?, ingest.max_decompressed_bytes)?)
        }
    }
}

/// The audit's findings on stdout, and the exit code a monitor reads. Only a
/// bucket that held objects and verified every one of them exits zero: an
/// empty bucket checked nothing, and a failure carries both counts because
/// "did not verify" and "could not be read" are different news.
fn verified(found: Audit) -> Result<(), Fatal> {
    println!("verified {} objects", found.verified);
    for failure in &found.failures {
        println!("{failure}");
    }
    match (found.failed(), found.unreadable(), found.verified) {
        (0, 0, 0) => Err(Fatal::ArchiveEmpty),
        (0, 0, _) => Ok(()),
        (failed, unreadable, _) => Err(Fatal::ArchiveNotVerified { failed, unreadable }),
    }
}

fn s3_archive(config: &S3Config) -> Result<S3Archive, Fatal> {
    let credentials = Credentials::from_env().ok_or(Fatal::MissingCredentials)?;
    Ok(S3Archive::new(config, credentials)?)
}

/// The rebuild's findings on stdout: it is the command's output, not a log
/// line.
fn report(summary: RebuiltIndex) -> Result<(), Fatal> {
    println!(
        "rebuilt the index from {} objects across {} pools",
        summary.objects,
        summary.pools.len()
    );
    for pool in &summary.pools {
        println!(
            "{pool_id} counter {counter} at {timestamp} ({state})",
            pool_id = pool.newest.pool_id,
            counter = pool.newest.counter,
            timestamp = pool.newest.timestamp,
            state = if pool.seeded {
                "seeded"
            } else {
                "already ahead"
            },
        );
    }
    Ok(())
}

fn serve<A: Archive>(
    archive: A,
    listen: &str,
    ingest: IngestConfig,
    counters: CounterStore,
) -> Result<(), Fatal> {
    let server = tiny_http::Server::http(listen).map_err(|source| Fatal::Listen {
        listen: listen.to_string(),
        reason: source.to_string(),
    })?;
    // The bound address, not the configured one: `:0` is how the tests and
    // any readiness check learn the port.
    eprintln!(
        "{INFO}metsuke-server {} accepting {} pools at http://{}{}",
        env!("CARGO_PKG_VERSION"),
        ingest.allowlist.len(),
        server.server_addr(),
        http::SUBMIT_PATH,
    );
    let mut intake = Intake::new(ingest, counters, archive);
    match http::serve(&server, &mut intake)? {}
}
