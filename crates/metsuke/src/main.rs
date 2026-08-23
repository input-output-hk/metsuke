//! The agent binary: startup fails loudly, then two decoupled ticks —
//! sample and upload — run forever on their configured cadences. Log lines
//! carry sd-daemon `<level>` prefixes so journald records severities.

use std::time::{Duration, Instant};

use metsuke::agent::Agent;
use metsuke::cli::{Args, ArgsError};
use metsuke::config::{Config, ConfigError};
use metsuke::delivery::Delivery;
use metsuke::keys::{self, KeyError};
use metsuke::sampler::SamplerConfig;
use metsuke::schedule::{Schedule, ScheduleConfig};
use metsuke::scrape::ScrapeConfig;
use metsuke::sntp::SntpConfig;
use metsuke::spool::{Spool, SpoolConfig, SpoolError};
use metsuke::uploader::{UploadConfig, UploadOutcome, newer_version_available};
use metsuke_wire::journal::{ERR, INFO, WARNING};

#[derive(Debug, thiserror::Error)]
enum StartupError {
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
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error("cannot open spool: {0}")]
    Spool(#[from] SpoolError),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("{ERR}metsuke failed to start: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<std::convert::Infallible, StartupError> {
    let args = Args::parse(std::env::args().skip(1))?;
    let text =
        std::fs::read_to_string(&args.config).map_err(|source| StartupError::ReadConfig {
            path: args.config.display().to_string(),
            source,
        })?;
    let config = Config::from_toml(&text)?;
    let key =
        keys::resolve_signing_key(args.signing_key.as_deref(), config.signing_key.as_deref())?;
    let spool = Spool::open(&SpoolConfig {
        path: config.spool_path.clone(),
        max_samples: config.spool_max_samples,
    })?;
    let vkey = key.verifying_key();
    let mut agent = Agent::new(
        SamplerConfig {
            scrape: ScrapeConfig {
                metrics_url: config.metrics_url.clone(),
                timeout: Duration::from_secs(config.scrape_timeout_secs),
                max_body_bytes: config.scrape_max_body_bytes,
            },
            sntp: SntpConfig {
                servers: config.sntp_servers.clone(),
                timeout: Duration::from_secs(config.sntp_timeout_secs),
            },
        },
        Delivery::new(spool, key, config.pool_id, config.compression_level),
        UploadConfig {
            upload_url: config.upload_url.clone(),
            pool_id: config.pool_id,
            timeout: Duration::from_secs(config.upload_timeout_secs),
        },
        vkey,
    );
    eprintln!(
        "{INFO}metsuke {} sampling {} for {}",
        env!("CARGO_PKG_VERSION"),
        config.metrics_url,
        config.pool_id,
    );

    let sample_interval = Duration::from_secs(config.sample_interval_secs);
    let schedule_config = ScheduleConfig {
        upload_interval: Duration::from_secs(config.upload_interval_secs),
        jitter_max: Duration::from_secs(config.upload_jitter_max_secs),
        backoff_max: Duration::from_secs(config.upload_backoff_max_secs),
    };
    let mut schedule = Schedule::new();
    let mut next_sample = Instant::now();
    // First upload immediately: rows left over from the previous run retry
    // at startup (ADR 0004).
    let mut next_upload = Instant::now();
    loop {
        let now = Instant::now();
        if now >= next_sample {
            if let Err(error) = agent.sample_once() {
                eprintln!("{ERR}sample not spooled: {error}");
            }
            next_sample = now + sample_interval;
        }
        if now >= next_upload {
            next_upload = now + upload_tick(&mut agent, &mut schedule, &schedule_config);
        }
        let wake = next_sample.min(next_upload);
        std::thread::sleep(wake.saturating_duration_since(Instant::now()));
    }
}

/// One upload tick: attempt, log the outcome, return the delay until the
/// next attempt.
fn upload_tick(agent: &mut Agent, schedule: &mut Schedule, config: &ScheduleConfig) -> Duration {
    let outcome = match agent.upload_once() {
        Ok(Some(outcome)) => outcome,
        Ok(None) => return config.upload_interval,
        Err(error) => {
            eprintln!("{ERR}{error}");
            return config.upload_interval;
        }
    };
    match &outcome {
        UploadOutcome::Acked(ack) => {
            eprintln!("{INFO}batch acked");
            if newer_version_available(env!("CARGO_PKG_VERSION"), &ack.latest_version) {
                eprintln!(
                    "{WARNING}client {} is available (this is {}); \
                     see the instructions page for the update procedure",
                    ack.latest_version,
                    env!("CARGO_PKG_VERSION"),
                );
            }
        }
        UploadOutcome::Retryable(reason) => {
            eprintln!("{WARNING}upload failed, samples stay spooled: {reason}");
        }
        UploadOutcome::Rejected { status, reason } => {
            eprintln!(
                "{WARNING}server rejected the upload ({status}), \
                 samples stay spooled, backing off: {reason}"
            );
        }
    }
    let entropy = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as u64;
    schedule.after(&outcome, config, entropy)
}
