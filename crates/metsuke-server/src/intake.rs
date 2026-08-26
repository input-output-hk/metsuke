//! The one path an upload takes. `submit` read top to bottom is the check
//! order; ADR 0002 fixes only what that order must satisfy.

use metsuke_wire::envelope::{
    Ack, ContainerError, Envelope, PoolId, SCHEMA_VERSION_LINES, SCHEMA_VERSION_SAMPLES, split,
};
use time::OffsetDateTime;

use crate::archive::{ArchiveError, Store, StoredSubmission};
use crate::authority::{AuthError, Authority, Refusal, Signed, Undecided, authenticate};
use crate::calidus::Resolution;
use crate::config::IngestConfig;
use crate::index::{Index, IndexError, Reservation};
use crate::ratelimit::RateLimiter;

/// Why the server refused. Every variant is the client's fault and its text
/// is what the client logs, so each names what to change.
#[derive(Debug, thiserror::Error)]
pub enum Rejection {
    #[error("body is {found} bytes, over the {max} byte limit")]
    OversizedBody { found: usize, max: u64 },
    /// The body is not a submission container at all, which is answerable
    /// from its first eight bytes and so is answered before anything else.
    #[error("body is not a submission: {0}")]
    NotASubmission(#[from] ContainerError),
    #[error("pool {pool_id} is not on the allowlist")]
    UnknownPool { pool_id: PoolId },
    #[error("pool {pool_id} is over its limit of {max} uploads per {window_secs}s")]
    RateLimited {
        pool_id: PoolId,
        max: u32,
        window_secs: u64,
    },
    #[error("the presented key does not speak for pool {pool_id}")]
    UnauthorizedKey { pool_id: PoolId, refusal: Refusal },
    /// Separate from `UnauthorizedKey` because it says nothing about the key:
    /// the server declined to read the pool's registrations at all.
    #[error(
        "more than {max} Calidus registrations scope pool {pool_id}; get in touch to resolve it"
    )]
    TooManyRegistrations { pool_id: PoolId, max: u32 },
    #[error("signature does not verify over the body as received")]
    BadSignature,
    #[error("payload inflates past the {max} byte limit")]
    OversizedPayload { max: u64 },
    #[error("payload is not a valid envelope: {reason}")]
    MalformedPayload { reason: String },
    #[error(
        "envelope schema version {found}, server speaks \
         v{SCHEMA_VERSION_SAMPLES} and v{SCHEMA_VERSION_LINES}"
    )]
    UnsupportedSchema { found: u32 },
    #[error("envelope is for pool {found}, submitted as {submitted}")]
    PoolIdMismatch { submitted: PoolId, found: PoolId },
    #[error("timestamp {timestamp} is more than {max_skew_secs}s from server time")]
    TimestampOutOfWindow {
        timestamp: OffsetDateTime,
        max_skew_secs: u64,
    },
    #[error("counter {found} does not advance past the accepted {last}")]
    ReplayedCounter { found: u64, last: u64 },
}

/// A submission the server could not process. `Rejected` is the client's
/// problem and permanent; `Unavailable` is the server's and worth a retry —
/// the distinction the HTTP layer turns into 4xx versus 5xx.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Rejected(#[from] Rejection),
    #[error("counter state unavailable: {0}")]
    CounterState(#[from] IndexError),
    #[error("archive unavailable: {0}")]
    Archive(#[from] ArchiveError),
    #[error("{0}")]
    Undecided(#[from] Undecided),
}

impl IngestError {
    /// What the log line says and the answer does not. Which chain state
    /// refused a key is a different fix per state for the operator, and every
    /// one of them is on chain — but nothing asked for a client to learn it
    /// from a 403.
    pub fn withheld(&self) -> Option<String> {
        match self {
            IngestError::Rejected(Rejection::UnauthorizedKey { refusal, .. }) => {
                Some(refusal.to_string())
            }
            _ => None,
        }
    }
}

pub struct Intake<A: Store, K: Authority> {
    config: IngestConfig,
    index: Index,
    limiter: RateLimiter,
    archive: A,
    authority: K,
}

impl<A: Store, K: Authority> Intake<A, K> {
    pub fn new(config: IngestConfig, index: Index, archive: A, authority: K) -> Self {
        let limiter = RateLimiter::new(config.rate_limit_uploads, config.rate_limit_window_secs);
        Intake {
            config,
            index,
            limiter,
            archive,
            authority,
        }
    }

    /// The body cap, so the HTTP layer refuses at the number this checks
    /// against rather than holding a second copy of it.
    pub fn max_body_bytes(&self) -> u64 {
        self.config.max_body_bytes.get()
    }

    /// The index the developer listing reads. Borrowed from here rather than
    /// opened a second time, so a listing and an ingest write can never be two
    /// connections racing on one file.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// The archive the download route fetches from. Same reason as `index`:
    /// one handle, and this one is read-only where the ingest path stores.
    pub fn archive(&self) -> &A {
        &self.archive
    }

    /// Run one upload through the chain. `now` is the server clock,
    /// taken once so every check in one upload judges the same instant.
    pub fn submit(&mut self, signed: &Signed<'_>, now: OffsetDateTime) -> Result<Ack, IngestError> {
        let pool_id = signed.pool_id;
        if signed.wire_bytes.len() as u64 > self.config.max_body_bytes.get() {
            return Err(Rejection::OversizedBody {
                found: signed.wire_bytes.len(),
                max: self.config.max_body_bytes.get(),
            }
            .into());
        }
        // Before the allowlist, the limiter and the key: whether these bytes
        // are a submission at all costs eight bytes to answer, and every check
        // after this one costs more.
        split(signed.wire_bytes, self.config.max_header_bytes.get())
            .map_err(Rejection::NotASubmission)?;
        if !self.config.allowlist.contains_key(&pool_id) {
            return Err(Rejection::UnknownPool { pool_id }.into());
        }
        if !self.limiter.allow(pool_id, now) {
            return Err(Rejection::RateLimited {
                pool_id,
                max: self.config.rate_limit_uploads.get(),
                window_secs: self.config.rate_limit_window_secs.get(),
            }
            .into());
        }
        let envelope = authenticate(&mut self.authority, signed, self.config.limits(), now)
            .map_err(|error| match error {
                AuthError::UnauthorizedKey { pool_id, refusal } => match refusal {
                    Refusal::Chain(Resolution::TooMany { max }) => {
                        IngestError::from(Rejection::TooManyRegistrations { pool_id, max })
                    }
                    refusal => IngestError::from(Rejection::UnauthorizedKey { pool_id, refusal }),
                },
                AuthError::BadSignature => Rejection::BadSignature.into(),
                AuthError::OversizedPayload { max } => Rejection::OversizedPayload { max }.into(),
                AuthError::MalformedPayload { reason } => {
                    Rejection::MalformedPayload { reason }.into()
                }
                AuthError::UnsupportedSchemaVersion { found } => {
                    Rejection::UnsupportedSchema { found }.into()
                }
                AuthError::Undecided(error) => error.into(),
            })?;
        self.accept(signed, envelope, now)
    }

    /// The post-decompression half: what the signed payload itself claims
    /// must hold before the bytes are archived.
    fn accept(
        &mut self,
        signed: &Signed<'_>,
        envelope: Envelope,
        now: OffsetDateTime,
    ) -> Result<Ack, IngestError> {
        // No schema check here: `open` returns only an envelope whose version
        // this build reads and whose payload shape agrees with it.
        if envelope.pool_id != signed.pool_id {
            return Err(Rejection::PoolIdMismatch {
                submitted: signed.pool_id,
                found: envelope.pool_id,
            }
            .into());
        }
        let skew = (now - envelope.timestamp).abs();
        if skew > time::Duration::seconds(self.config.max_timestamp_skew_secs.get() as i64) {
            return Err(Rejection::TimestampOutOfWindow {
                timestamp: envelope.timestamp,
                max_skew_secs: self.config.max_timestamp_skew_secs.get(),
            }
            .into());
        }
        let reserved = match self
            .index
            .reserve(envelope.pool_id, envelope.counter, now)?
        {
            Reservation::Reserved(reserved) => reserved,
            Reservation::Replayed { last } => {
                return Err(Rejection::ReplayedCounter {
                    found: envelope.counter,
                    last,
                }
                .into());
            }
        };
        let stored = StoredSubmission {
            pool_id: envelope.pool_id,
            counter: envelope.counter,
            timestamp: envelope.timestamp,
            schema_version: envelope.schema_version(),
            vkey: signed.vkey,
            signature: signed.signature,
            wire_bytes: signed.wire_bytes,
        };
        self.archive.store(&stored)?;
        // Indexed through the reservation, which is the write already open:
        // the row lands with the counter or with neither, so nothing is listed
        // that a rolled-back store left out of the bucket.
        reserved.record(&stored.name())?;
        reserved.commit()?;
        Ok(Ack {
            latest_version: crate::CLIENT_VERSION.to_string(),
        })
    }
}
