//! Every value an operator sets, in one file's worth of structs. No field has
//! a default: a missing limit is a deployment mistake, not a value to guess,
//! and zero is the same mistake — every field where it means nothing is a
//! `NonZero`, so serde refuses it at load rather than at first use.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use metsuke_wire::envelope::Limits;
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
    /// The rebuildable index: replay counters and one row per stored object
    /// (ADR 0005). Created if absent.
    pub index_path: PathBuf,
    pub archive: ArchiveConfig,
    pub ingest: IngestConfig,
    pub calidus: CalidusConfig,
    pub developer: DeveloperConfig,
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
/// db-sync holding the registered codes. No password field — see `Chain`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationsConfig {
    pub applications_csv: AbsolutePath,
    /// A directory and not a host, because the connection is the unix socket
    /// ADR 0008 fixes: anything reachable over the network would need TLS and
    /// a threat model this deployment does not have.
    pub socket_dir: AbsolutePath,
    pub dbname: String,
    /// Read-only, and nothing here can widen it: the query is fixed at compile
    /// time.
    pub role: String,
    /// Bounds one chain read twice over: it reaches Postgres as
    /// `statement_timeout` and is also how long the connect may take, so a
    /// db-sync that is down costs it once and a query that hangs costs it
    /// again.
    pub query_timeout_secs: NonZeroU64,
}

/// The db-sync the Calidus half reads registrations out of, and how long one
/// resolution stands. No `[applications]`-style `Option`: every serving host
/// must be able to answer ADR 0003's Calidus half, and a missing section would
/// silently leave it on the cold key alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalidusConfig {
    /// Why a directory and not a host: `ApplicationsConfig::socket_dir`.
    pub socket_dir: AbsolutePath,
    pub dbname: String,
    /// Read-only, and nothing here can widen it: the query is fixed at compile
    /// time.
    pub role: String,
    /// A file holding the role's password and nothing else. The path is
    /// config, the password is whatever systemd's `LoadCredential` put there,
    /// so no password reaches the environment (metsuke-4zo.50).
    pub password_file: AbsolutePath,
    /// What it bounds: `ApplicationsConfig::query_timeout_secs`.
    pub query_timeout_secs: NonZeroU64,
    /// The network's Shelley genesis, which is the only place k is written
    /// (ADR 0008).
    pub shelley_genesis_path: AbsolutePath,
    /// How long a resolved registration stands before the server asks again.
    /// What it is chosen against: ADR 0008. Seconds as a `NonZeroU32` so the
    /// cast to a signed duration cannot wrap.
    pub resolution_ttl_secs: NonZeroU32,
    /// Rows under one pool's scope the server will verify before it refuses the
    /// pool (ADR 0008).
    pub max_registrations: NonZeroU32,
}

/// The one account that may pull the archive back out (ticket metsuke-4zo.10).
/// Not optional: a serving host either has the credential or refuses to start,
/// where an absent section would leave the routes quietly open or quietly gone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeveloperConfig {
    /// The user developers authenticate as. Public, like `CalidusConfig::role`.
    pub user: String,
    /// Same shape and same reason as `CalidusConfig::password_file`.
    pub password_file: AbsolutePath,
    /// Rows one listing may answer with. What a page at the bound reports:
    /// `index::Listing`.
    pub list_max_rows: NonZeroU32,
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
    /// Cap on the body as sent, header frame and data frame together.
    /// `intake` measures what it was handed; `http::read_body` is what refuses
    /// an oversized upload before reading it.
    pub max_body_bytes: NonZeroU64,
    /// Cap on the header frame a submission declares, checked against the
    /// declared length before any of it is read.
    pub max_header_bytes: NonZeroU64,
    /// Ceiling the decompressor is allowed to inflate to.
    pub max_decompressed_bytes: NonZeroU64,
    /// Uploads one pool may make per `rate_limit_window_secs`.
    pub rate_limit_uploads: NonZeroU32,
    pub rate_limit_window_secs: NonZeroU64,
    /// How far an envelope timestamp may sit from the server clock in
    /// either direction. The ADR-0002 backstop for lost counter state.
    pub max_timestamp_skew_secs: NonZeroU64,
}

impl IngestConfig {
    /// The two bounds `envelope::open` puts on a submission. Paired because no
    /// caller wants one of them without the other.
    pub fn limits(&self) -> Limits {
        Limits {
            max_header_bytes: self.max_header_bytes.get(),
            max_decompressed_bytes: self.max_decompressed_bytes.get(),
        }
    }
}
