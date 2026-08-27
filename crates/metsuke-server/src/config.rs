//! Every value an operator sets, in one file's worth of structs. No field has
//! a default: a missing limit is a deployment mistake, not a value to guess,
//! and zero is the same mistake — every field where it means nothing is a
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
    pub http: HttpConfig,
    pub archive: ArchiveConfig,
    pub ingest: IngestConfig,
    pub developer: DeveloperConfig,
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
/// kind is the table's own name — `[archive.s3]` — rather than a `kind` field,
/// because serde buffers an internally-tagged table and every value under it
/// then reports at `[archive]` instead of at the line that set it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    /// on, as the offline allowlist generator emits them (metsuke-jfb.7). A
    /// pool outside it is rejected before any cryptography runs; the code is
    /// what says why the pool is here, and nothing at ingest reads it.
    pub allowlist: Codes,
    /// Cap on the body as sent, header frame and data frame together.
    /// `intake` measures what it was handed; `http::read_body` is what refuses
    /// an oversized upload before reading it.
    pub max_body_bytes: NonZeroU64,
    /// Cap on the header frame a submission declares, checked against the
    /// declared length before any of it is read.
    pub max_header_bytes: NonZeroU64,
    /// Uploads one pool may make per `rate_limit_window_secs`.
    pub rate_limit_uploads: NonZeroU32,
    /// Uploads every pool together may make in the same window.
    pub rate_limit_uploads_total: NonZeroU32,
    pub rate_limit_window_secs: NonZeroU64,
}
