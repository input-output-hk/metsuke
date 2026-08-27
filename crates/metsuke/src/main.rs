//! The agent binary: startup fails loudly, then two decoupled ticks —
//! sample and upload — run forever on their configured cadences. Log lines
//! carry sd-daemon `<level>` prefixes so journald records severities.

use std::time::{Duration, Instant};

use metsuke::agent::Agent;
use metsuke::cli::{Args, ArgsError};
use metsuke::config::{Config, ConfigError, LogConfig, LogSource};
use metsuke::delivery::Delivery;
use metsuke::identity::{self, IdentityError};
use metsuke::keys::{self, KeyError};
use metsuke::logselect::{OutsideRoots, SelectConfig};
use metsuke::logsource::PipeSource;
use metsuke::logtail::{self, DrainEnd};
use metsuke::sampler::SamplerConfig;
use metsuke::schedule::{Schedule, ScheduleConfig};
use metsuke::scrape::ScrapeConfig;
use metsuke::sntp::SntpConfig;
use metsuke::spool::{LogSpool, LogSpoolConfig, Spool, SpoolConfig, SpoolError};
use metsuke::uploader::{UploadConfig, UploadOutcome, newer_version_available};
use metsuke_wire::envelope::Provenance;
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
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("cannot open spool: {0}")]
    Spool(#[from] SpoolError),
    #[error("[log] selection rules: {0}")]
    Selection(#[from] OutsideRoots),
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
    identity::check_pool_id(config.pool_id, &key.verifying_key())?;
    let agent_id = identity::agent_id(config.agent_id.as_deref())?;
    // Resolved once and handed to both spool writers: it is what stamps every
    // line and what a batch's header names, so one value or they could disagree.
    let provenance = Provenance {
        pool_id: config.pool_id,
        agent_id: agent_id.clone(),
    };
    let busy_timeout = Duration::from_secs(config.spool_busy_timeout_secs);
    let spool = Spool::open(&SpoolConfig {
        path: config.spool_path.clone(),
        max_bytes: config.spool_max_bytes,
        busy_timeout,
        provenance: provenance.clone(),
    })?;
    let discarded = spool.discarded_by_upgrade();
    if discarded > 0 {
        eprintln!(
            "{WARNING}the spool upgrade discarded {discarded} rows written before rows carried \
             a stamp; no server would have accepted them"
        );
    }
    let vkey = key.verifying_key();
    eprintln!(
        "{INFO}metsuke {} on {agent_id} sampling {} for {}",
        env!("CARGO_PKG_VERSION"),
        config.metrics_url,
        config.pool_id,
    );
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
        Delivery::new(
            spool,
            key,
            config.compression_level,
            config.upload_batch_max_bytes,
        ),
        UploadConfig {
            upload_url: config.upload_url.clone(),
            timeout: Duration::from_secs(config.upload_timeout_secs),
        },
        vkey,
    );
    if let Some(log) = &config.log {
        start_trace_collection(log, &config.spool_path, busy_timeout, provenance)?;
    }

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

/// Open the trace-line half of the spool and hand it to a thread reading the
/// configured source. Failing to open it is a startup failure like any other
/// spool failure: an operator who configured `[log]` asked for these lines.
///
/// The thread is never joined. systemd's default `KillMode=control-group`
/// takes the journalctl it spawned down with the agent, so there is nothing to
/// wind up that outlives the process.
fn start_trace_collection(
    log: &LogConfig,
    spool_path: &std::path::Path,
    busy_timeout: Duration,
    provenance: Provenance,
) -> Result<(), StartupError> {
    let spool = LogSpool::open(&LogSpoolConfig {
        path: spool_path.to_path_buf(),
        max_bytes: log.log_max_bytes,
        busy_timeout,
        provenance,
    })?;
    let selection = SelectConfig::new(&log.namespace_roots, log.namespaces.clone())?;
    let backoff = Duration::from_secs(log.respawn_backoff_secs);
    let namespaces = log.namespaces.join(", ");
    match &log.source {
        LogSource::Journald(journal) => {
            eprintln!(
                "{INFO}collecting trace lines from {}: namespaces {namespaces}",
                journal.journal_unit,
            );
            let journal = journal.clone();
            std::thread::spawn(move || {
                let error = logtail::run(journal, selection, backoff, spool);
                eprintln!("{ERR}the trace-line spool is not writable: {error}");
                std::process::exit(1);
            });
        }
        LogSource::Pipe(pipe) => {
            eprintln!(
                "{INFO}collecting trace lines from the node's stdout: namespaces {namespaces}"
            );
            let pipe = pipe.clone();
            std::thread::spawn(move || {
                let mut spool = spool;
                let mut source = PipeSource::spawn(&pipe);
                // The pipe ends when the node exits, and this agent has nothing
                // left to sample: the journald path respawns instead, because
                // there the stream ending says nothing about the node.
                match logtail::drain(&mut source, &selection, &mut spool) {
                    Err(error) => {
                        eprintln!("{ERR}the trace-line spool is not writable: {error}");
                        std::process::exit(1);
                    }
                    Ok(DrainEnd::SourceFailed) => {
                        eprintln!("{ERR}the node's output stopped without the node exiting");
                        std::process::exit(1);
                    }
                    Ok(DrainEnd::NodeExited) => {
                        eprintln!("{INFO}the node's output ended; metsuke is stopping with it");
                        std::process::exit(0);
                    }
                }
            });
        }
    }
    Ok(())
}

/// One upload tick: attempt, log the outcome, return the delay until the
/// next attempt.
fn upload_tick(agent: &mut Agent, schedule: &mut Schedule, config: &ScheduleConfig) -> Duration {
    // Before the attempt, so every path out of this function has said it:
    // sustained overload is exactly the case where the attempt fails.
    let dropped = agent.take_dropped_report();
    if dropped > 0 {
        eprintln!(
            "{WARNING}the sample spool cap dropped {dropped} rows since the last report; \
             uploads are not keeping up, or the server has been unreachable"
        );
    }
    let attempted = agent.upload_once();
    // After the attempt, because taking the batch is what drops them.
    let uncarriable = agent.take_uncarriable_report();
    if uncarriable.oversized > 0 {
        eprintln!(
            "{WARNING}dropped {} rows no batch could ever carry, the largest {} bytes: \
             raise upload_batch_max_bytes past it, plus the envelope's framing",
            uncarriable.oversized, uncarriable.largest_bytes,
        );
    }
    let outcome = match attempted {
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
