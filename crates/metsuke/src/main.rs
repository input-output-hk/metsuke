//! The agent binary: startup fails loudly, then two decoupled ticks, scrape
//! and upload, run forever on their configured cadences. Log lines carry
//! sd-daemon `<level>` prefixes so journald records severities.

use std::time::{Duration, Instant};

use metsuke::agent::{Agent, Uploaded};
use metsuke::cli::{Args, ArgsError, USAGE, VERSION};
use metsuke::config::{Config, ConfigError, LogConfig, LogSource};
use metsuke::delivery::Delivery;
use metsuke::identity::{self, IdentityError};
use metsuke::keys::{self, KeyError};
use metsuke::logselect::{OutsideRoots, SelectConfig};
use metsuke::logsource::{JournalSource, PipeSource, StartError};
use metsuke::logtail::{self, DrainEnd};
use metsuke::report::{Line, ScrapeReport};
use metsuke::schedule::{self, Schedule, ScheduleConfig};
use metsuke::scrape::ScrapeConfig;
use metsuke::scraper::ScraperConfig;
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
    /// An operator who wrote `[log]` asked for these lines, so a journalctl
    /// that never follows fails the start rather than being retried every
    /// backoff forever (`logsource::StartError`).
    #[error("cannot follow the node's journal: {0}")]
    TraceSource(#[from] StartError),
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Answered wherever they appear, because that is where an operator writes
    // them, and on stdout, because they are the run's answer rather than a
    // refusal.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        std::process::exit(0);
    }
    if argv.iter().any(|a| a == "--version") {
        println!("{}", metsuke_wire::version_line(VERSION));
        std::process::exit(0);
    }
    let args = Args::parse(argv.into_iter())?;
    // Before the config is even read: the start that most needs its build
    // named is the one about to fail, and everything below here can.
    eprintln!(
        "{INFO}metsuke {} starting",
        metsuke_wire::version_line(VERSION)
    );
    let text =
        std::fs::read_to_string(&args.config).map_err(|source| StartupError::ReadConfig {
            path: args.config.display().to_string(),
            source,
        })?;
    let config = Config::from_toml(&text)?;
    let key =
        keys::resolve_signing_key(args.signing_key.as_deref(), config.signing_key.as_deref())?;
    identity::check_pool_id(config.pool_id, &key)?;
    let agent_id = identity::agent_id(config.agent_id.as_deref())?;
    // Resolved once and handed to both spool writers: it is what stamps every
    // line and what a submission's header names, so one value or they could
    // disagree.
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
    eprintln!(
        "{INFO}metsuke on {agent_id} scraping {} for {}",
        config.metrics_url, config.pool_id,
    );
    let mut agent = Agent::new(
        ScraperConfig {
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
            max_submissions: config.upload_max_submissions,
        },
        config.pool_id,
    );
    if let Some(log) = &config.log {
        start_trace_collection(log, &config.spool_path, busy_timeout, provenance)?;
    }

    let scrape_interval = Duration::from_secs(config.scrape_interval_secs.get());
    let schedule_config = ScheduleConfig {
        upload_interval: Duration::from_secs(config.upload_interval_secs.get()),
        jitter_max: Duration::from_secs(config.upload_jitter_max_secs),
        backoff_max: Duration::from_secs(config.upload_backoff_max_secs),
    };
    let mut schedule = Schedule::new();
    let mut scrapes = ScrapeReport::default();
    // In pipe mode the node starts alongside the agent, so a scrape at once
    // would meet an endpoint that is not bound yet and ship a failure nothing
    // was wrong with. Waiting first is what avoids it; retrying on the same
    // cadence is what covers a node slower than that. Both stage work and
    // neither bounds anything, and neither is ever longer than the interval
    // the operator configured, so a fast scrape cadence stays fast.
    const FIRST_SCRAPE_DELAY: Duration = Duration::from_secs(10);
    const FIRST_ANSWER_RETRY: Duration = Duration::from_secs(5);
    let waited = FIRST_SCRAPE_DELAY.min(scrape_interval);
    // Said rather than left as a gap: this is the last line before the wait,
    // and in pipe mode an operator is watching a terminal when it happens.
    // Without a duration, because the wait follows the configured interval and
    // the recording the page shows is made on a short one.
    eprintln!("{INFO}the first scrape waits for the node to bind its metrics endpoint");
    let starting_until = Instant::now() + waited + scrape_interval;
    let mut next_scrape = Instant::now() + waited;
    // The first upload waits for that scrape rather than going out before
    // there is one, so what an operator sees within seconds of starting is a
    // submission carrying a scrape. Rows left over from the previous run go
    // with it, which is what ADR 0004 asks of a start.
    let mut next_upload = next_scrape;
    loop {
        let now = Instant::now();
        if now >= next_scrape {
            let starting = now < starting_until;
            match agent.scrape_once() {
                Ok(news) => log_report(scrapes.record(&news, starting)),
                Err(error) => eprintln!("{ERR}scrape not spooled: {error}"),
            }
            next_scrape = now
                + match starting && !scrapes.answered() {
                    true => FIRST_ANSWER_RETRY.min(scrape_interval),
                    false => scrape_interval,
                };
        }
        if now >= next_upload {
            next_upload =
                now + upload_tick(&mut agent, &mut scrapes, &mut schedule, &schedule_config);
        }
        let wake = next_scrape.min(next_upload);
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
            // Started here rather than on the thread, so a journalctl that
            // never follows is a startup failure (`StartupError::TraceSource`).
            let first = JournalSource::spawn(&journal)?.confirm_following()?;
            std::thread::spawn(move || {
                let error = logtail::run(journal, selection, backoff, spool, first);
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
                // left to read: the journald path respawns instead, because
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

/// The scrape report's lines, each at the severity the report gave it.
fn log_report(lines: Vec<Line>) {
    for line in lines {
        match line {
            Line::Info(text) => eprintln!("{INFO}{text}"),
            Line::Warning(text) => eprintln!("{WARNING}{text}"),
        }
    }
}

/// One upload tick: attempt, log the outcome, return the delay until the
/// next attempt.
fn upload_tick(
    agent: &mut Agent,
    scrapes: &mut ScrapeReport,
    schedule: &mut Schedule,
    config: &ScheduleConfig,
) -> Duration {
    // Before the attempt, so every path out of this function has said it:
    // sustained overload is exactly the case where the attempt fails.
    log_report(scrapes.drain());
    let dropped = agent.take_dropped_report();
    if dropped > 0 {
        eprintln!(
            "{WARNING}the scrape spool cap dropped {dropped} rows since the last report; \
             uploads are not keeping up, or the server has been unreachable"
        );
    }
    let attempted = agent.upload_once();
    // After the attempt, because taking the submission is what drops them.
    let uncarriable = agent.take_uncarriable_report();
    if uncarriable.oversized > 0 {
        eprintln!(
            "{WARNING}dropped {} rows no submission could ever carry, the largest {} bytes: \
             raise upload_batch_max_bytes past it, plus the envelope's framing",
            uncarriable.oversized, uncarriable.largest_bytes,
        );
    }
    // Every path out of a tick says where the next one is. A tick that sends
    // nothing is every tick of an agent uploading faster than it scrapes, and
    // silence past the time the last line named reads as a stopped agent.
    let nothing_sent = |wait: Duration| {
        eprintln!(
            "{INFO}{}",
            schedule::nothing_sent_line(time::OffsetDateTime::now_utc(), wait)
        );
        wait
    };
    let sent = match attempted {
        Ok(sent) => sent,
        Err(error) => {
            eprintln!("{ERR}{error}");
            return nothing_sent(config.upload_interval);
        }
    };
    let Some(last) = sent.last() else {
        return nothing_sent(config.upload_interval);
    };
    // Every submission of the tick, because which one a line is about is what
    // ties it to an archived object and to what stays spooled.
    for Uploaded {
        outcome,
        counter,
        lines,
        carried,
        bytes,
        payload_digest,
    } in &sent
    {
        match outcome {
            UploadOutcome::Acked(ack) => {
                eprintln!(
                    "{INFO}submission {counter} payload {payload_digest} accepted: \
                     {lines} {carried}, {bytes} bytes"
                );
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
                eprintln!(
                    "{WARNING}submission {counter} payload {payload_digest} was not taken, \
                     and the spool keeps its {lines} {carried}: {reason}"
                );
            }
            UploadOutcome::Rejected { status, reason } => {
                eprintln!(
                    "{WARNING}the server refused submission {counter} payload \
                     {payload_digest} with {status}, the spool keeps its {lines} {carried}, \
                     backing off: {reason}"
                );
            }
        }
    }
    let now = time::OffsetDateTime::now_utc();
    let wait = schedule.after(&last.outcome, config, now.unix_timestamp_nanos() as u64);
    // Only where the tick had something to send: a line saying when the next
    // one is, after a round that said nothing, is a line about nothing.
    eprintln!("{INFO}{}", schedule::next_submission_line(now, wait));
    wait
}
