//! Every value an operator sets, in one file's worth of structs. No field has
//! a default: a missing limit is a deployment mistake, not a value to guess,
//! and zero is the same mistake. Every field where it means nothing is a
//! `NonZero`, so serde refuses it at load rather than at first use.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

use crate::applications::Codes;

/// The whole config file: where to listen, where the archive is, and the
/// ingest limits under `[ingest]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// `host:port` to bind, plain HTTP (what fronts it: `http`).
    pub listen: String,
    /// Where operators reach this server, which `listen` does not say: behind a
    /// proxy the bound address is not the one anyone types. The onboarding page
    /// hands out configs pointing at it, so an operator edits their pool id and
    /// nothing else.
    pub public_url: Url,
    pub http: HttpConfig,
    pub archive: ArchiveConfig,
    pub ingest: IngestConfig,
    pub developer: DeveloperConfig,
    /// The static agent builds this deployment offers, so an operator needs no
    /// nix to get one. Optional as a whole rather than defaulted, because a
    /// deployment that ships none is a deployment whose page must say so, and
    /// because requiring it would make every VM test build two cross-compiled
    /// agents to stand up a server.
    #[serde(default)]
    pub downloads: Option<DownloadsConfig>,
}

/// Where each architecture's static agent is on this host. Both, because the
/// page names both and an operator on either should not have to build.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadsConfig {
    pub x86_64_linux: AbsolutePath,
    pub aarch64_linux: AbsolutePath,
}

/// What the transport refuses, as against what the intake refuses. Every field
/// bounds one way a client can hold a connection open without finishing with
/// it; `serve` is where each is applied.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// How long a connection may take to deliver a complete request head. It
    /// is also the keep-alive bound, because a connection waiting for its next
    /// request is a connection that has delivered no head yet.
    pub idle_timeout_ms: NonZeroU64,
    /// Deadline for the whole request body, not for one read of it. A client
    /// trickling a byte at a time renews any per-read deadline forever
    /// (metsuke-a3a), so the bound has to be on the body.
    pub read_timeout_ms: NonZeroU64,
    /// How long a write may make no progress, which is what a client that has
    /// stopped reading its answer costs.
    pub write_timeout_ms: NonZeroU64,
    /// Connections served at once, held from accept to close. HTTP/1.1 carries
    /// one request per connection at a time, so this is the request cap too.
    pub max_concurrent_requests: NonZeroU32,
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

/// The one account that may pull the archive back out (ticket metsuke-4zo.10).
/// Not optional: a serving host either has the credential or refuses to start,
/// where an absent section would leave the routes quietly open or quietly gone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeveloperConfig {
    /// The user developers authenticate as. Public, as this whole file is.
    pub user: String,
    /// A file holding the account's password and nothing else. The path is
    /// config, the password is whatever systemd's `LoadCredential` put there,
    /// so no password reaches the environment (metsuke-4zo.50).
    pub password_file: AbsolutePath,
    /// Keys one listing may answer with. A value above the upstream cap is
    /// clamped rather than refused (`developer::Developer::list_max_rows`).
    pub list_max_rows: NonZeroU32,
}

/// Where accepted submissions go. S3 is what production runs (ADR 0005). The
/// kind is the table's own name, `[archive.s3]`, rather than a `kind` field,
/// because serde buffers an internally-tagged table and every value under it
/// then reports at `[archive]` instead of at the line that set it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ArchiveConfig {
    Filesystem { root: PathBuf },
    S3(S3Config),
}

/// The bucket and how long the server waits on it. Credentials are not here.
/// They come from the process environment, which is what keeps this file
/// Nix-managed and public.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Endpoint URL, bucket name excluded: the AWS regional endpoint in
    /// production, a Garage or MinIO address in a test. It has to answer a GET
    /// with a `Content-Length` rather than chunked, as S3 does
    /// (`archive::ObjectStream`).
    pub endpoint: Url,
    /// Deadline for one S3 request.
    pub request_timeout_ms: NonZeroU64,
    /// How long a presigned URL stays usable. Separate from the deadline:
    /// tightening the ops timeout would otherwise shrink signature validity to
    /// the same window, and clock skew against the endpoint then rejects every
    /// request with an error naming neither cause.
    pub signature_validity_secs: NonZeroU64,
    /// Extra PUT attempts after the first. The one number here that is not a
    /// `NonZero`: zero is the deliberate choice to let the client's spool be
    /// the only retry layer (ADR 0004).
    pub put_retries: u32,
    /// Waited between PUT attempts. The failures a retry is for, 503
    /// SlowDown or a transport reset, need the endpoint given time.
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
    /// on, as the offline allowlist generator emits them (metsuke-jfb.7). A
    /// pool outside it is rejected before any cryptography runs; the code is
    /// what says why the pool is here, and nothing at ingest reads it.
    pub allowlist: Codes,
    /// Where the **Key Roster** is read from (ADR 0011): which Leios keys the
    /// chain registers for each pool, written by something outside this
    /// server. The one field here that may be absent, because a network with
    /// no Leios keys has no roster to point at, and absent is what refuses a
    /// Leios-key submission with a reason instead of accepting one against
    /// nothing.
    #[serde(default)]
    pub leios_roster: Option<AbsolutePath>,
    /// Cap on the body as sent, header frame and data frame together.
    /// `intake` measures what it was handed; `http::read_body` is what refuses
    /// an oversized upload before reading it.
    pub max_body_bytes: NonZeroU64,
    /// Cap on the header frame a submission declares, checked against the
    /// declared length before any of it is read.
    pub max_header_bytes: NonZeroU64,
    /// How far either way a submission's sealed timestamp may sit from this
    /// server's clock. It bounds how long a captured submission stays
    /// replayable, and it is what an agent whose host clock has drifted is
    /// refused by, so it is a clock tolerance as much as a limit.
    pub max_timestamp_skew_secs: NonZeroU32,
    /// Uploads one pool may make per `rate_limit_window_secs`.
    pub rate_limit_uploads: NonZeroU32,
    /// Uploads every pool together may make in the same window.
    pub rate_limit_uploads_total: NonZeroU32,
    /// Seconds wide, in the width `time::Duration::seconds` takes without a
    /// cast: a wider one wrapped negative and the limiter then refused nothing.
    pub rate_limit_window_secs: NonZeroU32,
}
