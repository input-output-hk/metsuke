//! The ingest binary: startup fails loudly, then one loop serves submissions
//! until something it cannot serve through stops it. Both exit nonzero, so
//! systemd restarts rather than supervising a process that accepts nothing.
//! The subcommands are the exception: they run once against the same config
//! and exit, zero only if what they were asked to check holds.

use metsuke_server::applications::{ApplicationsCsvError, Chain, Excluded, Gate, gate, read_codes};
use metsuke_server::archive::{Bytes, FilesystemArchive, List, Store};
use metsuke_server::authority::{ColdKey, ColdKeyOrCalidus};
use metsuke_server::calidus::CalidusKeys;
use metsuke_server::cli::{ArchiveCommand, Args, ArgsError, Command, GENERATE_ALLOWLIST};
use metsuke_server::config::{
    ApplicationsConfig, ArchiveConfig, CalidusConfig, ConfigError, DeveloperConfig, IngestConfig,
    S3Config, ServerConfig,
};
use metsuke_server::db::DbError;
use metsuke_server::dbsync::{DbSync, GenesisError, security_parameter};
use metsuke_server::developer::Developer;
use metsuke_server::http;
use metsuke_server::index::{Index, IndexError};
use metsuke_server::instructions;
use metsuke_server::intake::Intake;
use metsuke_server::rebuild::{EmptyArchive, RebuildError, RebuiltIndex, rebuild};
use metsuke_server::s3::{S3Archive, S3Error};
use metsuke_server::verify::{Audit, AuditError, audit};
use metsuke_wire::journal::{ERR, INFO};
use rusty_s3::Credentials;
use time::OffsetDateTime;

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
    #[error("cannot open the index: {0}")]
    Index(#[from] IndexError),
    #[error("cannot read the developer password {path}: {source}")]
    DeveloperPassword {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the developer password {path} is empty, which would authorize anyone")]
    EmptyDeveloperPassword { path: String },
    #[error(transparent)]
    Genesis(#[from] GenesisError),
    #[error(transparent)]
    S3(#[from] S3Error),
    #[error("the S3 archive needs AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY in the environment")]
    MissingCredentials,
    #[error("cannot rebuild the index: {0}")]
    Rebuild(#[from] RebuildError),
    #[error("{GENERATE_ALLOWLIST} needs an [applications] section in the config")]
    NoApplications,
    #[error("cannot open the applications {path}: {source}")]
    OpenApplications {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read the applications {path}: {source}")]
    ReadApplications {
        path: String,
        #[source]
        source: ApplicationsCsvError,
    },
    #[error("cannot read the registered application codes: {0}")]
    Chain(#[from] DbError),
    #[error("no pool both applied and registered its code, so the allowlist would accept nobody")]
    NobodyMatched,
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
        index_path,
        archive,
        ingest,
        calidus,
        developer,
        applications,
    } = ServerConfig::from_toml(&text)?;
    let command = match args.command {
        Command::GenerateAllowlist => {
            return generate_allowlist(applications.as_ref().ok_or(Fatal::NoApplications)?);
        }
        Command::Archive(command) => command,
    };
    let index = Index::open(&index_path)?;
    let serving = Serving {
        listen,
        ingest,
        calidus,
        developer,
    };
    // The archive kind is matched once: pairing it with the subcommand would
    // multiply the arms by every kind this grows.
    match archive {
        ArchiveConfig::Filesystem { root } => dispatch(
            FilesystemArchive::new(&root),
            command,
            serving,
            index,
            // A filesystem archive stores no metadata, so there is nothing to
            // re-verify an object against.
            |_, _| Err(Fatal::CannotVerifyFilesystem),
        ),
        ArchiveConfig::S3(config) => dispatch(
            s3_archive(&config)?,
            command,
            serving,
            index,
            |archive, max_decompressed_bytes| {
                report_audit(audit(
                    archive,
                    max_decompressed_bytes,
                    &mut ColdKey,
                    OffsetDateTime::now_utc(),
                )?)
            },
        ),
    }
}

/// Everything the config says that is not about the archive or the index.
/// Grouped because it travels through `dispatch` as one thing and only
/// `serve` reads all of it.
struct Serving {
    listen: String,
    ingest: IngestConfig,
    calidus: CalidusConfig,
    developer: DeveloperConfig,
}

/// Run the named subcommand against one archive. `verify_archive` is the only
/// step that is not the same work for every kind, so it is the parameter.
fn dispatch<A: Store + List + Bytes>(
    archive: A,
    command: ArchiveCommand,
    serving: Serving,
    mut index: Index,
    verify_archive: impl FnOnce(&A, u64) -> Result<(), Fatal>,
) -> Result<(), Fatal> {
    match command {
        ArchiveCommand::Serve => serve(archive, serving, index),
        ArchiveCommand::RebuildIndex { allow_empty } => {
            let empty = match allow_empty {
                true => EmptyArchive::Accept,
                false => EmptyArchive::Refuse,
            };
            report_rebuild(rebuild(&archive, &mut index, empty)?)
        }
        ArchiveCommand::VerifyArchive => {
            verify_archive(&archive, serving.ingest.max_decompressed_bytes.get())
        }
    }
}

/// The pairs on stdout, the summary on stderr: stdout is the artifact another
/// config reads, so anything meant for the operator has to stay out of it.
fn generate_allowlist(config: &ApplicationsConfig) -> Result<(), Fatal> {
    let applications = config.applications_csv.as_path();
    let path = applications.display().to_string();
    let text = std::fs::read_to_string(applications).map_err(|source| Fatal::OpenApplications {
        path: path.clone(),
        source,
    })?;
    let applied = read_codes(&text).map_err(|source| Fatal::ReadApplications { path, source })?;
    let found = gate(applied, Chain::new(config).registered_codes()?);
    print!("{}", found.to_toml());
    report_gate(&found)
}

fn report_gate(found: &Gate) -> Result<(), Fatal> {
    eprintln!(
        "{} pools allowlisted, {} applicants excluded",
        found.allowed.len(),
        found.excluded.len()
    );
    for (pool_id, why) in &found.excluded {
        let reason = match why {
            Excluded::NotRegistered => "applied, but registered no application code".to_string(),
            Excluded::CodeMismatch { registered } => {
                format!("applied with a code that is not its registered {registered}")
            }
            Excluded::ContradictoryCodes => {
                "applied, and has more than one code registered".to_string()
            }
        };
        eprintln!("excluded {pool_id}: {reason}");
    }
    if found.did_not_apply > 0 {
        eprintln!(
            "{} pools have a registered code and never applied",
            found.did_not_apply
        );
    }
    if found.unreadable > 0 {
        eprintln!(
            "{} registered rows are not a pool and a code",
            found.unreadable
        );
    }
    match found.allowlists_nobody() {
        true => Err(Fatal::NobodyMatched),
        false => Ok(()),
    }
}

/// The audit's findings on stdout, and the exit code a monitor reads. Only a
/// bucket that held objects and verified every one of them exits zero: an
/// empty bucket checked nothing, and a failure carries both counts because
/// "did not verify" and "could not be read" are different news.
fn report_audit(found: Audit) -> Result<(), Fatal> {
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
fn report_rebuild(summary: RebuiltIndex) -> Result<(), Fatal> {
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

fn serve<A: Store + Bytes>(archive: A, serving: Serving, index: Index) -> Result<(), Fatal> {
    let Serving {
        listen,
        ingest,
        calidus,
        developer: credentials,
    } = serving;
    let listen = listen.as_str();
    // Read here rather than at load: `rebuild-index` and `verify-archive`
    // answer no developer request, so a credential file only this path needs
    // must not decide whether they run.
    let developer = developer(&credentials)?;
    // Before the listener: k is what decides which registrations count, and a
    // server that cannot read it would accept uploads on an unbounded rule.
    let security_parameter = security_parameter(calidus.shelley_genesis_path.as_path())?;
    let ttl_secs = calidus.resolution_ttl_secs;
    let authority = ColdKeyOrCalidus::new(CalidusKeys::new(
        DbSync::new(calidus, security_parameter),
        ttl_secs,
    ));
    // Built from files compiled in, so a broken one is a build that must not
    // reach an operator asking for it.
    let page = instructions::page();
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
    let mut intake = Intake::new(ingest, index, archive, authority);
    match http::serve(&server, &mut intake, &developer, &page)? {}
}

/// The developer account, with the password read off the file the config
/// names. A file that is missing, unreadable or empty stops startup: the
/// routes would otherwise be open on a credential nobody set, and `user` is
/// public in a config an operator publishes.
fn developer(config: &DeveloperConfig) -> Result<Developer, Fatal> {
    let path = config.password_file.as_path();
    let named = || path.display().to_string();
    let password = std::fs::read_to_string(path).map_err(|source| Fatal::DeveloperPassword {
        path: named(),
        source,
    })?;
    // Trailing newline trimmed, as the Calidus password is (`db`): an editor
    // adds one and it is not part of the secret.
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return Err(Fatal::EmptyDeveloperPassword { path: named() });
    }
    Ok(Developer::new(config, password))
}
