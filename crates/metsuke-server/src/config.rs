//! Every value an operator sets, in one file's worth of structs. No field has
//! a default: a missing limit is a deployment mistake, not a value to guess,
//! and zero is the same mistake — every field where it means nothing is a
//! `NonZero`, so serde refuses it at load rather than at first use.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

use crate::applications::Codes;

/// The whole config file: where to listen, where the two stores live, and the
/// ingest limits under `[ingest]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// `host:port` to bind, plain HTTP (what fronts it: `http`).
    pub listen: String,
    /// The replay counter database (ADR 0002). Created if absent.
    pub counters_path: PathBuf,
    pub archive: ArchiveConfig,
    pub ingest: IngestConfig,
    pub calidus: CalidusConfig,
    /// Read by `generate-allowlist` alone, so a server that never onboards
    /// pools carries neither export path nor db-sync connection. Absent is
    /// what that server's config looks like, and the command is what refuses
    /// when it is.
    pub applications: Option<ApplicationsConfig>,
}

/// A path the config states in full. Relative would be resolved against
/// whatever directory the process happened to start in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsolutePath(PathBuf);

#[derive(Debug, thiserror::Error)]
#[error("{found} must be an absolute path")]
pub struct NotAbsolute {
    found: String,
}

impl AbsolutePath {
    pub fn new(path: PathBuf) -> Result<Self, NotAbsolute> {
        match path.is_absolute() {
            true => Ok(AbsolutePath(path)),
            false => Err(NotAbsolute {
                found: path.display().to_string(),
            }),
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AbsolutePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        AbsolutePath::new(PathBuf::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Where each half of the gate comes from: the applications export, and the
/// db-sync holding the registered codes. No password field — see `Psql`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationsConfig {
    pub applications_csv: AbsolutePath,
    /// Which binary reads the database is not the operator's shell's to decide.
    /// `Command` resolves a bare name against the environment it inherits, and
    /// the `env_clear` in `Psql` does not stop it, so the refusal has to be
    /// here.
    pub psql_path: AbsolutePath,
    /// `psql --host` reads a value as a socket directory only when it starts
    /// with `/`; anything else it resolves as a hostname and connects to over
    /// the network.
    pub socket_dir: AbsolutePath,
    pub dbname: String,
    /// Read-only, and nothing here can widen it: the query is fixed at compile
    /// time.
    pub role: String,
    /// Reaches Postgres as `statement_timeout`.
    pub query_timeout_secs: NonZeroU64,
}

/// The db-sync the Calidus half reads registrations out of, and how long one
/// resolution stands. No `[applications]`-style `Option`: every serving host
/// must be able to answer ADR 0003's Calidus half, and a missing section would
/// silently leave it on the cold key alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalidusConfig {
    /// Why the path and not a bare name: `ApplicationsConfig::psql_path`.
    pub psql_path: AbsolutePath,
    /// Why a directory and not a host: `ApplicationsConfig::socket_dir`.
    pub socket_dir: AbsolutePath,
    pub dbname: String,
    /// Read-only, and nothing here can widen it: the query is fixed at compile
    /// time.
    pub role: String,
    /// A `.pgpass` reaching psql as `PGPASSFILE`. The path is config, the
    /// password is whatever systemd's `LoadCredential` put there.
    pub password_file: AbsolutePath,
    /// Reaches Postgres as `statement_timeout`.
    pub query_timeout_secs: NonZeroU64,
    /// The network's Shelley genesis, which is the only place k is written
    /// (ADR 0008).
    pub shelley_genesis_path: AbsolutePath,
    /// How long a resolved registration stands before the server asks again.
    /// What it is chosen against: ADR 0008. Seconds as a `NonZeroU32` so the
    /// cast to a signed duration cannot wrap.
    pub resolution_ttl_secs: NonZeroU32,
}

/// Where accepted submissions go. S3 is what production runs (ADR 0005).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArchiveConfig {
    Filesystem { root: PathBuf },
    S3(S3Config),
}

/// The bucket and how long the server waits on it. Credentials are not here —
/// they come from the process environment, which is what keeps this file
/// Nix-managed and public.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Endpoint URL, bucket name excluded: the AWS regional endpoint in
    /// production, a Garage or MinIO address in a test.
    pub endpoint: Url,
    /// Deadline for one S3 request.
    pub request_timeout_secs: NonZeroU64,
    /// How long a presigned URL stays usable. Separate from the deadline:
    /// tightening the ops timeout would otherwise shrink signature validity to
    /// the same window, and clock skew against the endpoint then rejects every
    /// request with an error naming neither cause.
    pub signature_validity_secs: NonZeroU64,
    /// Extra PUT attempts after the first. The one number here that is not a
    /// `NonZero`: zero is the deliberate choice to let the client's spool be
    /// the only retry layer (ADR 0004).
    pub put_retries: u32,
    /// Waited between PUT attempts. The failures a retry is for — 503
    /// SlowDown, a transport reset — need the endpoint given time.
    pub put_retry_backoff_ms: NonZeroU64,
    /// Pages a bucket listing may take before it fails naming the bound. An
    /// endpoint that keeps handing back a continuation token would otherwise
    /// list forever.
    pub list_max_pages: NonZeroU32,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config does not parse: {0}")]
    Toml(#[from] toml::de::Error),
}

impl ServerConfig {
    pub fn from_toml(text: &str) -> Result<ServerConfig, ConfigError> {
        Ok(toml::from_str(text)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestConfig {
    /// The participating pools against the application code each was onboarded
    /// on, as `generate-allowlist` emits them. A pool outside it is rejected
    /// before any cryptography runs; the code is what says why the pool is
    /// here, and nothing at ingest reads it.
    pub allowlist: Codes,
    /// Cap on the compressed body. `intake` measures what it was handed;
    /// `http::read_body` is what refuses an oversized upload before reading
    /// it.
    pub max_body_bytes: NonZeroU64,
    /// Ceiling the decompressor is allowed to inflate to.
    pub max_decompressed_bytes: NonZeroU64,
    /// Uploads one pool may make per `rate_limit_window_secs`.
    pub rate_limit_uploads: NonZeroU32,
    pub rate_limit_window_secs: NonZeroU64,
    /// How far an envelope timestamp may sit from the server clock in
    /// either direction. The ADR-0002 backstop for lost counter state.
    pub max_timestamp_skew_secs: NonZeroU64,
}
