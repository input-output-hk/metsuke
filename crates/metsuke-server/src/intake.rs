//! The one path an upload takes. `submit` read top to bottom is the check
//! order, and there are three checks: the pool is allowlisted, the traffic is
//! within its budget, the signature stands.
//!
//! Nothing here decompresses and nothing reads a payload. The header frame is
//! plaintext inside the signed bytes, so what an object is filed under comes
//! out of bytes the signature already covered.

use std::sync::Mutex;

use metsuke_wire::envelope::{Ack, ContainerError, HeaderError, PoolId, read_header};
use time::OffsetDateTime;

use crate::archive::{ArchiveError, Kind, ObjectName, Store, StoredSubmission};
use crate::authority::Signed;
use crate::config::IngestConfig;
use crate::ratelimit::{Charged, RateLimiter};

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
    /// Separate from `RateLimited` because it says nothing about this pool:
    /// every pool together filled the window, and the fix is not the client's.
    #[error("the server is over its limit of {max} uploads per {window_secs}s")]
    ServerBusy { max: u32, window_secs: u64 },
    #[error("signature does not verify over the body as received")]
    BadSignature,
    /// The signature stands, so these bytes are the pool's — and its header
    /// frame is not one this build can read a name out of.
    #[error("header frame does not read: {0}")]
    UnreadableHeader(#[from] HeaderError),
    /// Not schema gating: an accepted batch is filed under what it carries, and
    /// a version this build has no name for has no object key to be filed at.
    #[error("nothing here files a schema v{schema_version} batch")]
    UnnameableKind { schema_version: u32 },
}

/// A submission the server could not process. `Rejected` is the client's
/// problem and permanent; `Unavailable` is the server's and worth a retry —
/// the distinction the HTTP layer turns into 4xx versus 5xx.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Rejected(#[from] Rejection),
    #[error("archive unavailable: {0}")]
    Archive(#[from] ArchiveError),
}

/// The limiter is the server's only mutable state, so it is the only thing
/// behind a lock: an upload takes it for the charge alone and never across the
/// archive write, and a developer reading the archive never takes it at all.
pub struct Intake<A: Store> {
    config: IngestConfig,
    limiter: Mutex<RateLimiter>,
    archive: A,
}

impl<A: Store> Intake<A> {
    pub fn new(config: IngestConfig, archive: A) -> Self {
        let limiter = RateLimiter::new(
            config.rate_limit_uploads,
            config.rate_limit_uploads_total,
            config.rate_limit_window_secs,
        );
        Intake {
            config,
            limiter: Mutex::new(limiter),
            archive,
        }
    }

    /// The body cap, so the HTTP layer refuses at the number this checks
    /// against rather than holding a second copy of it.
    pub fn max_body_bytes(&self) -> u64 {
        self.config.max_body_bytes.get()
    }

    /// The archive the developer routes read. One handle rather than a second
    /// one built beside it, and read-only where the ingest path stores.
    pub fn archive(&self) -> &A {
        &self.archive
    }

    /// Run one upload through the chain. `now` is the server clock,
    /// taken once so every check in one upload judges the same instant and the
    /// object is stamped with what the limiter charged.
    pub fn submit(&self, signed: &Signed<'_>, now: OffsetDateTime) -> Result<Ack, IngestError> {
        let pool_id = signed.pool_id();
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
        metsuke_wire::envelope::split(signed.wire_bytes, self.config.max_header_bytes.get())
            .map_err(Rejection::NotASubmission)?;
        if !self.config.allowlist.contains_key(&pool_id) {
            return Err(Rejection::UnknownPool { pool_id }.into());
        }
        // A temporary of this statement, so the guard is released before the
        // match reads it rather than at the end of the block.
        let charged = self
            .limiter
            .lock()
            .expect("the rate limiter is never poisoned: charging cannot panic")
            .charge(pool_id, now);
        match charged {
            Charged::Allowed => (),
            Charged::PoolIsOver => {
                return Err(Rejection::RateLimited {
                    pool_id,
                    max: self.config.rate_limit_uploads.get(),
                    window_secs: self.config.rate_limit_window_secs.get(),
                }
                .into());
            }
            Charged::ServerIsOver => {
                return Err(Rejection::ServerBusy {
                    max: self.config.rate_limit_uploads_total.get(),
                    window_secs: self.config.rate_limit_window_secs.get(),
                }
                .into());
            }
        }
        if !signed.verifies() {
            return Err(Rejection::BadSignature.into());
        }
        self.accept(signed, pool_id, now)
    }

    /// The post-signature half: the batch is the pool's, so what it says about
    /// itself is what the object is filed under.
    fn accept(
        &self,
        signed: &Signed<'_>,
        pool_id: PoolId,
        now: OffsetDateTime,
    ) -> Result<Ack, IngestError> {
        let header = read_header(signed.wire_bytes, self.config.max_header_bytes.get())
            .map_err(Rejection::UnreadableHeader)?;
        let kind = Kind::of(header.schema_version).ok_or(Rejection::UnnameableKind {
            schema_version: header.schema_version,
        })?;
        // The pool comes from the key and the agent from the header: the pool
        // is what the allowlist admitted, and which of its machines reported is
        // not something the server has any other account of.
        let stored = StoredSubmission {
            name: ObjectName::stamped(now, pool_id, header.agent_id, kind),
            vkey: signed.vkey,
            signature: signed.signature,
            wire_bytes: signed.wire_bytes,
        };
        self.archive.store(&stored)?;
        Ok(Ack {
            latest_version: crate::CLIENT_VERSION.to_string(),
        })
    }
}
