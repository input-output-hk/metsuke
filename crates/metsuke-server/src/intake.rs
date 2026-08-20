//! The one path an upload takes. `submit` read top to bottom is the check
//! order; ADR 0002 fixes only what that order must satisfy.

use metsuke::envelope::{self, Ack, Envelope, PoolId, SCHEMA_VERSION, Signature, VerifyingKey};
use time::OffsetDateTime;

use crate::archive::{ArchiveError, Store, StoredSubmission};
use crate::config::IngestConfig;
use crate::counters::{CounterError, CounterStore, Reservation};
use crate::ratelimit::RateLimiter;

/// One upload as it arrives: the ADR-0001 headers plus the body as sent.
pub struct Submission<'a> {
    pub pool_id: PoolId,
    pub vkey: VerifyingKey,
    pub signature: Signature,
    /// The compressed body, byte for byte as received.
    pub wire_bytes: &'a [u8],
}

/// Why the server refused. Every variant is the client's fault and its text
/// is what the client logs, so each names what to change.
#[derive(Debug, thiserror::Error)]
pub enum Rejection {
    #[error("body is {found} bytes, over the {max} byte limit")]
    OversizedBody { found: usize, max: u64 },
    #[error("pool {pool_id} is not on the allowlist")]
    UnknownPool { pool_id: PoolId },
    #[error("pool {pool_id} is over its limit of {max} uploads per {window_secs}s")]
    RateLimited {
        pool_id: PoolId,
        max: u32,
        window_secs: u64,
    },
    #[error("the presented key does not speak for pool {pool_id}")]
    UnauthorizedKey { pool_id: PoolId },
    #[error("signature does not verify over the body as received")]
    BadSignature,
    #[error("payload inflates past the {max} byte limit")]
    OversizedPayload { max: u64 },
    #[error("payload is not a schema v{SCHEMA_VERSION} envelope: {reason}")]
    MalformedPayload { reason: String },
    #[error("envelope schema version {found}, server speaks v{SCHEMA_VERSION}")]
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
    CounterState(#[from] CounterError),
    #[error("archive unavailable: {0}")]
    Archive(#[from] ArchiveError),
}

pub struct Intake<A: Store> {
    config: IngestConfig,
    counters: CounterStore,
    limiter: RateLimiter,
    archive: A,
}

impl<A: Store> Intake<A> {
    pub fn new(config: IngestConfig, counters: CounterStore, archive: A) -> Self {
        let limiter = RateLimiter::new(config.rate_limit_uploads, config.rate_limit_window_secs);
        Intake {
            config,
            counters,
            limiter,
            archive,
        }
    }

    /// The body cap, so the HTTP layer refuses at the number this checks
    /// against rather than holding a second copy of it.
    pub fn max_body_bytes(&self) -> u64 {
        self.config.max_body_bytes.get()
    }

    /// Run one submission through the chain. `now` is the server clock,
    /// taken once so every check in a submission judges the same instant.
    pub fn submit(
        &mut self,
        submission: &Submission<'_>,
        now: OffsetDateTime,
    ) -> Result<Ack, IngestError> {
        let pool_id = submission.pool_id;
        if submission.wire_bytes.len() as u64 > self.config.max_body_bytes.get() {
            return Err(Rejection::OversizedBody {
                found: submission.wire_bytes.len(),
                max: self.config.max_body_bytes.get(),
            }
            .into());
        }
        if !self.config.allowlist.contains(&pool_id) {
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
        // The cold-key path of ADR 0003; the Calidus path (ticket
        // metsuke-4zo.8) joins here.
        if PoolId::from_cold_key(&submission.vkey) != pool_id {
            return Err(Rejection::UnauthorizedKey { pool_id }.into());
        }
        let envelope = envelope::open(
            &submission.vkey,
            submission.wire_bytes,
            &submission.signature,
            self.config.max_decompressed_bytes.get(),
        )
        .map_err(|error| match error {
            envelope::OpenError::Signature(_) => Rejection::BadSignature,
            envelope::OpenError::TooLarge {
                max_decompressed_bytes,
            } => Rejection::OversizedPayload {
                max: max_decompressed_bytes,
            },
            envelope::OpenError::Decompress(error) => Rejection::MalformedPayload {
                reason: error.to_string(),
            },
            envelope::OpenError::Json(error) => Rejection::MalformedPayload {
                reason: error.to_string(),
            },
        })?;
        self.accept(submission, envelope, now)
    }

    /// The post-decompression half: what the signed payload itself claims
    /// must hold before the bytes are archived.
    fn accept(
        &mut self,
        submission: &Submission<'_>,
        envelope: Envelope,
        now: OffsetDateTime,
    ) -> Result<Ack, IngestError> {
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(Rejection::UnsupportedSchema {
                found: envelope.schema_version,
            }
            .into());
        }
        if envelope.pool_id != submission.pool_id {
            return Err(Rejection::PoolIdMismatch {
                submitted: submission.pool_id,
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
            .counters
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
        self.archive.store(&StoredSubmission {
            pool_id: envelope.pool_id,
            counter: envelope.counter,
            timestamp: envelope.timestamp,
            schema_version: envelope.schema_version,
            vkey: submission.vkey,
            signature: submission.signature,
            wire_bytes: submission.wire_bytes,
        })?;
        reserved.commit()?;
        Ok(Ack {
            latest_version: metsuke::AGENT_VERSION.to_string(),
        })
    }
}
