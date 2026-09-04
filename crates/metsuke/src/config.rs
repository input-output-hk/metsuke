//! Agent configuration: the TOML file an SPO edits, validated into `Config`.
//! Required fields (`pool_id`, `metrics_url`, `upload_url`) fail loudly when
//! absent or malformed; every cadence and limit is a config knob with a
//! shipped default (see contrib/config.example.toml).

use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::endpoint::{MetricsUrl, UploadUrl};
use crate::logsource::{JournalConfig, PipeConfig};
use crate::sntp;
use metsuke_wire::envelope::PoolId;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The pool this agent reports for, bech32 `pool1…`. Configured rather
    /// than derived from the key, and checked against it at startup
    /// (`identity::check_pool_id`).
    pub pool_id: PoolId,
    /// What to call this Agent on every line it ships. Absent means its own
    /// hostname, slugified (`identity::agent_id`); a value here is slugified
    /// the same way.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The node's PrometheusSimple endpoint.
    pub metrics_url: MetricsUrl,
    /// The metsuke-server submission endpoint.
    pub upload_url: UploadUrl,
    /// Path to the signing key; `--signing-key` overrides it.
    #[serde(default)]
    pub signing_key: Option<PathBuf>,
    /// Zero would leave no interval to wait out, and a 429 is retryable rather
    /// than a backoff, so a zero cadence hammers the node or this server
    /// without ever slowing down. The type refuses it.
    #[serde(default = "default_scrape_interval_secs")]
    pub scrape_interval_secs: NonZeroU64,
    #[serde(default = "default_upload_interval_secs")]
    pub upload_interval_secs: NonZeroU64,
    /// How many submissions one upload tick may send. One is not enough for a
    /// trace stream: what a node emits between ticks can be more than a single
    /// submission carries, and the difference accumulates until the spool's cap
    /// drops it.
    #[serde(default = "default_upload_max_submissions")]
    pub upload_max_submissions: NonZeroUsize,
    /// SNTP servers as `host:port`, tried in order.
    #[serde(default = "default_sntp_servers")]
    pub sntp_servers: Vec<String>,
    #[serde(default = "default_sntp_timeout_secs")]
    pub sntp_timeout_secs: u64,
    /// The SQLite spool (ADR 0004). The default lives under the systemd
    /// StateDirectory the shipped units create (nix/agent-module.nix).
    #[serde(default = "default_spool_path")]
    pub spool_path: PathBuf,
    /// Scrape spool size cap (semantics: `spool::SpoolConfig`).
    #[serde(default = "default_spool_max_bytes")]
    pub spool_max_bytes: u64,
    /// How long one spool write waits for the other connection to the file
    /// (semantics: `spool::SpoolConfig`).
    #[serde(default = "default_spool_busy_timeout_secs")]
    pub spool_busy_timeout_secs: u64,
    /// Per-envelope payload ceiling (semantics: `delivery::Delivery`). Nothing
    /// ties it to the server's `[ingest].max_body_bytes`, because the two are
    /// different operators' files.
    #[serde(default = "default_upload_batch_max_bytes")]
    pub upload_batch_max_bytes: u64,
    #[serde(default = "default_scrape_timeout_secs")]
    pub scrape_timeout_secs: u64,
    /// A metrics body larger than this is treated as a failed scrape.
    #[serde(default = "default_scrape_max_body_bytes")]
    pub scrape_max_body_bytes: u64,
    #[serde(default = "default_upload_timeout_secs")]
    pub upload_timeout_secs: u64,
    /// Upper bound on the spread that places this agent within the interval,
    /// and on the spread a retry adds, so agents installed together do not
    /// upload in step (`schedule::Schedule::after`). A ceiling rather than the
    /// spread: one wider than the interval is taken as the interval, so a
    /// cadence shorter than this default keeps it.
    #[serde(default = "default_upload_jitter_max_secs")]
    pub upload_jitter_max_secs: u64,
    /// Clamp on the exponential backoff after 4xx rejections.
    #[serde(default = "default_upload_backoff_max_secs")]
    pub upload_backoff_max_secs: u64,
    /// zstd level for the upload body (meaning: `delivery::Delivery`).
    #[serde(default)]
    pub compression_level: i32,
    /// Trace-line collection. Absent means the agent reads only the metrics
    /// endpoint, holds no journal grant, and behaves exactly as it did before
    /// this section existed (ADR 0010).
    #[serde(default)]
    pub log: Option<LogConfig>,
}

/// Which stream the trace lines come from. Named in the config rather than
/// inferred from stdin: an agent that guessed would collect nothing, or tee
/// nothing, and say neither.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogSourceKind {
    Journald,
    Pipe,
}

/// The configured source with the fields that source needs, and no others.
#[derive(Debug, PartialEq)]
pub enum LogSource {
    Journald(JournalConfig),
    Pipe(PipeConfig),
}

/// The `[log]` section. The unit and the journalctl have no sensible default:
/// which service the node runs as is not something this crate can guess, and
/// neither is where journalctl lives (`logsource::JournalConfig`).
#[derive(Debug, PartialEq, Deserialize)]
#[serde(try_from = "LogToml")]
pub struct LogConfig {
    pub source: LogSource,
    /// The ceiling on every namespace rule this host will ever honour
    /// (semantics: `logselect::SelectConfig::new`).
    pub namespace_roots: Vec<String>,
    /// Namespace prefixes to ship (semantics: `logselect::SelectConfig`).
    pub namespaces: Vec<String>,
    /// Trace-line spool cap (semantics: `spool::LogSpoolConfig`).
    pub log_max_bytes: u64,
    pub respawn_backoff_secs: u64,
}

/// The `[log]` table as written, before the source's own fields are checked
/// against the source.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogToml {
    source: LogSourceKind,
    journal_unit: Option<String>,
    journalctl_path: Option<PathBuf>,
    /// Zero would confirm nothing, so the type refuses it rather than the
    /// agent starting with the check silently off.
    start_grace_secs: Option<NonZeroU64>,
    pipe_queue_capacity: Option<NonZeroUsize>,
    #[serde(default = "default_log_namespace_roots")]
    namespace_roots: Vec<String>,
    #[serde(default = "default_log_namespaces")]
    namespaces: Vec<String>,
    #[serde(default = "default_log_max_bytes")]
    log_max_bytes: u64,
    #[serde(default = "default_log_respawn_backoff_secs")]
    respawn_backoff_secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LogSourceError {
    #[error("[log] source = \"journald\" needs {field}")]
    JournaldNeeds { field: &'static str },
    #[error(
        "[log] source = \"pipe\" reads the node's own stdout and never the journal, \
         so {field} must not be set"
    )]
    PipeRefuses { field: &'static str },
    #[error("[log] source = \"journald\" does not queue lines, so {field} must not be set")]
    JournaldRefuses { field: &'static str },
}

impl TryFrom<LogToml> for LogConfig {
    type Error = LogSourceError;

    fn try_from(toml: LogToml) -> Result<LogConfig, LogSourceError> {
        let source = match toml.source {
            LogSourceKind::Journald => {
                if toml.pipe_queue_capacity.is_some() {
                    return Err(LogSourceError::JournaldRefuses {
                        field: "pipe_queue_capacity",
                    });
                }
                LogSource::Journald(JournalConfig {
                    journal_unit: toml.journal_unit.ok_or(LogSourceError::JournaldNeeds {
                        field: "journal_unit",
                    })?,
                    journalctl_path: toml.journalctl_path.ok_or(LogSourceError::JournaldNeeds {
                        field: "journalctl_path",
                    })?,
                    start_grace: Duration::from_secs(
                        toml.start_grace_secs
                            .unwrap_or_else(default_log_start_grace_secs)
                            .get(),
                    ),
                })
            }
            LogSourceKind::Pipe => {
                for (field, set) in [
                    ("journal_unit", toml.journal_unit.is_some()),
                    ("journalctl_path", toml.journalctl_path.is_some()),
                    ("start_grace_secs", toml.start_grace_secs.is_some()),
                ] {
                    if set {
                        return Err(LogSourceError::PipeRefuses { field });
                    }
                }
                LogSource::Pipe(PipeConfig {
                    queue_capacity: toml
                        .pipe_queue_capacity
                        .unwrap_or_else(default_pipe_queue_capacity),
                })
            }
        };
        Ok(LogConfig {
            source,
            namespace_roots: toml.namespace_roots,
            namespaces: toml.namespaces,
            log_max_bytes: toml.log_max_bytes,
            respawn_backoff_secs: toml.respawn_backoff_secs,
        })
    }
}

fn default_scrape_interval_secs() -> NonZeroU64 {
    NonZeroU64::new(300).expect("300 is not zero")
}

fn default_upload_interval_secs() -> NonZeroU64 {
    NonZeroU64::new(3600).expect("3600 is not zero")
}

/// Room for several times what a Leios producer was measured to spool between
/// ticks, so a backlog drains rather than only holding steady. What bounds it
/// is the server's own upload rate limit, which this stays well inside.
fn default_upload_max_submissions() -> NonZeroUsize {
    NonZeroUsize::new(16).expect("16 is not zero")
}

fn default_sntp_servers() -> Vec<String> {
    vec![sntp::DEFAULT_SERVER.to_string()]
}

fn default_sntp_timeout_secs() -> u64 {
    5
}

fn default_spool_path() -> PathBuf {
    PathBuf::from("/var/lib/metsuke/spool.sqlite")
}

fn default_spool_max_bytes() -> u64 {
    32 * 1024 * 1024
}

fn default_spool_busy_timeout_secs() -> u64 {
    5
}

fn default_upload_batch_max_bytes() -> u64 {
    4 * 1024 * 1024
}

/// The node's own trace roots, which is as far as any namespace rule on this
/// host may reach. Wider than the shipped `namespaces` on purpose: the point of
/// a ceiling is to leave room under it while still naming a bound.
fn default_log_namespace_roots() -> Vec<String> {
    vec![
        "Consensus".to_string(),
        "ChainDB".to_string(),
        "Forge".to_string(),
    ]
}

/// What the rewards program asked for, as the namespaces a node actually emits.
/// Which ask each covers, and why this list is the whole selection rule:
/// docs/adr/0010.
fn default_log_namespaces() -> Vec<String> {
    vec![
        "Consensus.LeiosKernel".to_string(),
        "Consensus.LeiosPeer".to_string(),
        "ChainDB.AddBlockEvent.AddedToCurrentChain".to_string(),
        "Forge.Loop.AdoptedBlock".to_string(),
    ]
}

/// Lines the tee may hold for the spool worker (semantics:
/// `logsource::PipeConfig`).
fn default_pipe_queue_capacity() -> NonZeroUsize {
    NonZeroUsize::new(4096).expect("4096 is not zero")
}

fn default_log_max_bytes() -> u64 {
    256 * 1024 * 1024
}

fn default_log_respawn_backoff_secs() -> u64 {
    30
}

/// A chosen bound, not a measured one: no recording says how long a refused
/// journalctl takes to exit. Paid at every start and respawn (semantics:
/// `logsource::Spawned::confirm_following`).
fn default_log_start_grace_secs() -> NonZeroU64 {
    NonZeroU64::new(1).expect("1 is not zero")
}

fn default_scrape_timeout_secs() -> u64 {
    5
}

fn default_scrape_max_body_bytes() -> u64 {
    4 * 1024 * 1024
}

fn default_upload_timeout_secs() -> u64 {
    60
}

fn default_upload_jitter_max_secs() -> u64 {
    300
}

fn default_upload_backoff_max_secs() -> u64 {
    86_400
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config does not parse: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        Ok(toml::from_str(text)?)
    }
}
