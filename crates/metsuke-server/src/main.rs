//! The ingest binary: startup fails loudly, then one loop serves submissions
//! until something it cannot serve through stops it. Both exit nonzero, so
//! systemd restarts rather than supervising a process that accepts nothing.

use metsuke_server::archive::FilesystemArchive;
use metsuke_server::cli::{Args, ArgsError};
use metsuke_server::config::{ConfigError, ServerConfig};
use metsuke_server::counters::{CounterError, CounterStore};
use metsuke_server::intake::Intake;
use metsuke_server::{ERR, INFO, http};

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
    #[error("cannot listen on {listen}: {reason}")]
    Listen { listen: String, reason: String },
    /// Mid-life, not startup: why it is fatal is `http::serve`.
    #[error("the listener stopped accepting: {0}")]
    Accept(#[from] std::io::Error),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("{ERR}metsuke-server stopped: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<std::convert::Infallible, Fatal> {
    let args = Args::parse(std::env::args().skip(1))?;
    let text = std::fs::read_to_string(&args.config).map_err(|source| Fatal::ReadConfig {
        path: args.config.display().to_string(),
        source,
    })?;
    let config = ServerConfig::from_toml(&text)?;
    let counters = CounterStore::open(&config.counters_path)?;
    let archive = FilesystemArchive::new(&config.archive_root);
    let server = tiny_http::Server::http(&config.listen).map_err(|source| Fatal::Listen {
        listen: config.listen.clone(),
        reason: source.to_string(),
    })?;
    // The bound address, not the configured one: `:0` is how the tests and
    // any readiness check learn the port.
    eprintln!(
        "{INFO}metsuke-server {} accepting {} pools at http://{}{}",
        env!("CARGO_PKG_VERSION"),
        config.ingest.allowlist.len(),
        server.server_addr(),
        http::SUBMIT_PATH,
    );
    let mut intake = Intake::new(config.ingest, counters, archive);
    Ok(http::serve(&server, &mut intake)?)
}
