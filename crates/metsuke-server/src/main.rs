//! The ingest binary: startup fails loudly, then one loop serves submissions
//! until something it cannot serve through stops it. Both exit nonzero, so
//! systemd restarts rather than supervising a process that accepts nothing.
//! The subcommand is the exception: it runs once against the same config and
//! exits, zero only if what it was asked to check holds.

use metsuke_server::archive::{Bytes, FilesystemArchive, List, Store};
use metsuke_server::cli::{Args, ArgsError, Command, USAGE, VERSION};
use metsuke_server::config::{
    ArchiveConfig, ConfigError, DeveloperConfig, DownloadsConfig, HttpConfig, IngestConfig,
    S3Config, ServerConfig,
};
use metsuke_server::developer::Developer;
use metsuke_server::http;
use metsuke_server::instructions;
use metsuke_server::intake::Intake;
use metsuke_server::roster::{Roster, RosterError};
use metsuke_server::s3::{S3Archive, S3Error};
use metsuke_server::serve;
use metsuke_server::verify::{Audit, AuditError, audit};
use metsuke_wire::journal::{ERR, INFO, WARNING};
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
    #[error("cannot read the agent build at {path}: {source}")]
    ReadDownload {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot read the developer password {path}: {source}")]
    DeveloperPassword {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the developer password {path} is empty, which would authorize anyone")]
    EmptyDeveloperPassword { path: String },
    #[error(transparent)]
    Roster(#[from] RosterError),
    #[error(transparent)]
    S3(#[from] S3Error),
    #[error("the S3 archive needs AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY in the environment")]
    MissingCredentials,
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
    /// Mid-life, not startup: why it is fatal is `serve::Listener::serve`.
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Answered wherever they appear, because that is where an operator writes
    // them, and on stdout, because they are the run's answer rather than a
    // refusal.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    if argv.iter().any(|a| a == "--version") {
        println!("{VERSION}");
        return Ok(());
    }
    let args = Args::parse(argv.into_iter())?;
    // Before the config is even read: the start that most needs its build
    // named is the one about to fail, and everything below here can.
    eprintln!("{INFO}metsuke-server {VERSION} starting");
    let text = std::fs::read_to_string(&args.config).map_err(|source| Fatal::ReadConfig {
        path: args.config.display().to_string(),
        source,
    })?;
    let ServerConfig {
        listen,
        public_url,
        http,
        archive,
        ingest,
        developer,
        downloads,
    } = ServerConfig::from_toml(&text)?;
    let serving = Serving {
        listen,
        public_url,
        http,
        ingest,
        developer,
        downloads,
    };
    // The archive kind is matched once: pairing it with the subcommand would
    // multiply the arms by every kind this grows.
    match archive {
        ArchiveConfig::Filesystem { root } => {
            // Said once, loudly, because nothing downstream can: the pair is
            // dropped at the moment of storing, so no route, no audit and no
            // consumer can ever recover it. An operator who reads this line
            // and meant it has lost nothing; one who did not has an archive
            // that cannot be told from a fabricated one.
            eprintln!(
                "{WARNING}archiving to the filesystem at {}: this stores the submission bytes \
                 alone and drops the key and signature they were checked with, so nothing can \
                 verify this archive afterwards, verify-archive refuses it, and every download \
                 reaches a consumer unattested. S3 is what production runs (ADR 0005).",
                root.display()
            );
            dispatch(
                FilesystemArchive::new(&root),
                args.command,
                serving,
                // A filesystem archive stores no metadata, so there is nothing
                // to re-verify an object against.
                |_, _| Err(Fatal::CannotVerifyFilesystem),
            )
        }
        ArchiveConfig::S3(config) => dispatch(
            s3_archive(&config)?,
            args.command,
            serving,
            |archive, max_header_bytes| report_audit(audit(archive, max_header_bytes)?),
        ),
    }
}

/// Everything the config says that is not about the archive. Grouped because
/// it travels through `dispatch` as one thing and only `serve` reads all of
/// it.
struct Serving {
    listen: String,
    public_url: url::Url,
    http: HttpConfig,
    ingest: IngestConfig,
    developer: DeveloperConfig,
    downloads: Option<DownloadsConfig>,
}

/// The static agent builds the page offers, read off disk. Empty where the
/// deployment configures none, which the page then says.
fn agent_builds(config: Option<&DownloadsConfig>) -> Result<Vec<instructions::Binary>, Fatal> {
    let Some(config) = config else {
        return Ok(Vec::new());
    };
    [
        (instructions::BINARIES[0], &config.x86_64_linux),
        (instructions::BINARIES[1], &config.aarch64_linux),
    ]
    .into_iter()
    .map(|(name, path)| {
        Ok(instructions::Binary {
            name,
            bytes: std::fs::read(path.as_path()).map_err(|source| Fatal::ReadDownload {
                path: path.as_path().display().to_string(),
                source,
            })?,
        })
    })
    .collect()
}

/// Run the named subcommand against one archive. `verify_archive` is the only
/// step that is not the same work for every kind, so it is the parameter.
fn dispatch<A: Store + List + Bytes + Send + Sync + 'static>(
    archive: A,
    command: Command,
    serving: Serving,
    verify_archive: impl FnOnce(&A, u64) -> Result<(), Fatal>,
) -> Result<(), Fatal> {
    match command {
        Command::Serve => serve(archive, serving),
        Command::VerifyArchive => verify_archive(&archive, serving.ingest.max_header_bytes.get()),
    }
}

/// The audit's findings on stdout, and the exit code a monitor reads. Only a
/// bucket that held objects and verified every one of them exits zero: an
/// empty bucket checked nothing, and a failure carries both counts because
/// "did not verify" and "could not be read" are different news.
fn report_audit(found: Audit) -> Result<(), Fatal> {
    println!("verified {} objects", found.verified);
    if found.unattributed > 0 {
        println!(
            "{} objects verified in every part but the pool they are filed under, \
             which a Leios key cannot re-derive (ADR 0011)",
            found.unattributed
        );
    }
    for failure in &found.failures {
        println!("{failure}");
    }
    // An object checked in every part but its filing is still an object that
    // was read and verified, so a bucket holding only those is not empty.
    match (
        found.failed(),
        found.unreadable(),
        found.verified + found.unattributed,
    ) {
        (0, 0, 0) => Err(Fatal::ArchiveEmpty),
        (0, 0, _) => Ok(()),
        (failed, unreadable, _) => Err(Fatal::ArchiveNotVerified { failed, unreadable }),
    }
}

fn s3_archive(config: &S3Config) -> Result<S3Archive, Fatal> {
    let credentials = Credentials::from_env().ok_or(Fatal::MissingCredentials)?;
    Ok(S3Archive::new(config, credentials)?)
}

fn serve<A: Store + Bytes + List + Send + Sync + 'static>(
    archive: A,
    serving: Serving,
) -> Result<(), Fatal> {
    let Serving {
        listen,
        public_url,
        http: limits,
        ingest,
        developer: credentials,
        downloads,
    } = serving;
    // Read here rather than at load: `verify-archive` answers no developer
    // request, so a credential file only this path needs must not decide
    // whether it runs.
    let developer = developer(&credentials)?;
    // Same reason, and the same loud failure: a server told where its roster
    // is and unable to read it would otherwise start and refuse every pool
    // that signs with a Leios key (ADR 0011).
    let roster = ingest
        .leios_roster
        .as_ref()
        .map(|path| Roster::load(path.as_path()))
        .transpose()?;
    // Built from files compiled in, so a broken one is a build that must not
    // reach an operator asking for it. The agent builds are the exception:
    // read here, because a path that cannot be read is the deployment's
    // mistake and this is where it should stop.
    let pages = instructions::pages(&public_url, agent_builds(downloads.as_ref())?);
    let listener = serve::bind(&listen).map_err(|source| Fatal::Listen {
        listen: listen.clone(),
        reason: source.to_string(),
    })?;
    // The bound address, not the configured one: `:0` is how the tests and
    // any readiness check learn the port.
    eprintln!(
        "{INFO}metsuke-server accepting {} pools at http://{}{}",
        ingest.allowlist.len(),
        listener.address(),
        http::SUBMIT_PATH,
    );
    match &roster {
        Some(roster) => {
            let (epoch, slot) = roster.position();
            eprintln!("{INFO}Leios keys from a roster taken in epoch {epoch} at slot {slot}");
        }
        None => eprintln!("{INFO}cold-key submissions only: no Leios key roster is configured"),
    }
    let intake = Intake::new(ingest, archive, roster);
    match listener.serve(limits, intake, developer, pages)? {}
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
    // Trailing newline trimmed: an editor adds one and it is not part of the
    // secret.
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return Err(Fatal::EmptyDeveloperPassword { path: named() });
    }
    Ok(Developer::new(config, password))
}
