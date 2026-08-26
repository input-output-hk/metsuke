//! Agent configuration: the TOML file an SPO edits, validated into `Config`.
//! Required fields (`pool_id`, `metrics_url`, `upload_url`) fail loudly when
//! absent or malformed; every cadence and limit is a config knob with a
//! shipped default (see contrib/config.example.toml).

use std::path::PathBuf;

use serde::Deserialize;

use crate::endpoint::{MetricsUrl, UploadUrl};
use crate::logselect::Severity;
use crate::sntp;
use metsuke_wire::envelope::PoolId;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The pool this agent reports for, bech32 `pool1…` (why it is
    /// configured rather than derived: `delivery::Delivery`).
    pub pool_id: PoolId,
    /// The node's PrometheusSimple endpoint.
    pub metrics_url: MetricsUrl,
    /// The metsuke-server submission endpoint.
    pub upload_url: UploadUrl,
    /// Path to the signing key; `--signing-key` overrides it.
    #[serde(default)]
    pub signing_key: Option<PathBuf>,
    #[serde(default = "default_sample_interval_secs")]
    pub sample_interval_secs: u64,
    #[serde(default = "default_upload_interval_secs")]
    pub upload_interval_secs: u64,
    /// SNTP servers as `host:port`, tried in order.
    #[serde(default = "default_sntp_servers")]
    pub sntp_servers: Vec<String>,
    #[serde(default = "default_sntp_timeout_secs")]
    pub sntp_timeout_secs: u64,
    /// The SQLite spool (ADR 0004). The default lives under the systemd
    /// StateDirectory the shipped units create (nix/agent-module.nix).
    #[serde(default = "default_spool_path")]
    pub spool_path: PathBuf,
    /// Sample spool size cap (semantics: `spool::SpoolConfig`).
    #[serde(default = "default_spool_max_bytes")]
    pub spool_max_bytes: u64,
    /// How long one spool write waits for the other connection to the file
    /// (semantics: `spool::SpoolConfig`).
    #[serde(default = "default_spool_busy_timeout_secs")]
    pub spool_busy_timeout_secs: u64,
    /// Per-envelope payload ceiling (semantics: `delivery::Delivery`). Keep it
    /// at or under the server's `max_decompressed_bytes`; nothing enforces
    /// the agreement, because the two are different operators' files.
    #[serde(default = "default_upload_batch_max_bytes")]
    pub upload_batch_max_bytes: u64,
    #[serde(default = "default_scrape_timeout_secs")]
    pub scrape_timeout_secs: u64,
    /// A metrics body larger than this is treated as a failed scrape.
    #[serde(default = "default_scrape_max_body_bytes")]
    pub scrape_max_body_bytes: u64,
    #[serde(default = "default_upload_timeout_secs")]
    pub upload_timeout_secs: u64,
    /// Upper bound on the random spread added when retrying after a 5xx or
    /// transport failure.
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

/// The `[log]` section. The unit and the journalctl have no sensible default:
/// which service the node runs as is not something this crate can guess, and
/// neither is where journalctl lives (`logsource::JournalConfig`).
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    pub journal_unit: String,
    pub journalctl_path: PathBuf,
    /// The ceiling on every namespace rule this host will ever honour
    /// (semantics: `logselect::SelectConfig::new`).
    #[serde(default = "default_log_namespace_roots")]
    pub namespace_roots: Vec<String>,
    /// Namespace prefixes to ship (semantics: `logselect::SelectConfig`).
    #[serde(default = "default_log_namespaces")]
    pub namespaces: Vec<String>,
    #[serde(default = "default_log_min_severity")]
    pub min_severity: Severity,
    /// Trace-line spool cap (semantics: `spool::LogSpoolConfig`).
    #[serde(default = "default_log_max_bytes")]
    pub log_max_bytes: u64,
    #[serde(default = "default_log_respawn_backoff_secs")]
    pub respawn_backoff_secs: u64,
}

fn default_sample_interval_secs() -> u64 {
    300
}

fn default_upload_interval_secs() -> u64 {
    3600
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
        "Consensus.".to_string(),
        "ChainDB.".to_string(),
        "Forge.".to_string(),
    ]
}

/// What the rewards program asked for by name, as the namespaces a node
/// actually emits: the Leios prefix covers the announcement, body, closure and
/// quorum events in one rule, and block adoption sits outside it.
fn default_log_namespaces() -> Vec<String> {
    vec![
        "Consensus.Leios".to_string(),
        "ChainDB.AddBlockEvent.AddedToCurrentChain".to_string(),
        "Forge.Loop.AdoptedBlock".to_string(),
    ]
}

fn default_log_min_severity() -> Severity {
    Severity::Notice
}

fn default_log_max_bytes() -> u64 {
    256 * 1024 * 1024
}

fn default_log_respawn_backoff_secs() -> u64 {
    30
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
