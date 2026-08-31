//! The one path an upload takes. `submit` read top to bottom is the check
//! order: the pool is allowlisted, the key may speak for it, the signature
//! stands, the submission was sealed near this clock, the traffic is within its
//! budget.
//!
//! Nothing here decompresses and nothing reads a payload. The header frame is
//! plaintext inside the signed bytes, so what an object is filed under comes
//! out of bytes the signature already covered.

use std::sync::Mutex;

use metsuke_wire::envelope::{Ack, ContainerError, Header, HeaderError, PoolId, read_header};
use metsuke_wire::journal::INFO;
use time::{Duration, OffsetDateTime};

use crate::archive::{ArchiveError, Kind, ObjectName, Store, StoredSubmission};
use crate::authority::{Signed, Unauthorised};
use crate::config::IngestConfig;
use crate::ratelimit::{Charged, RateLimiter};
use crate::roster::Roster;

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
        window_secs: u32,
    },
    /// Separate from `RateLimited` because it says nothing about this pool:
    /// every pool together filled the window, and the fix is not the client's.
    #[error("the server is over its limit of {max} uploads per {window_secs}s")]
    ServerBusy { max: u32, window_secs: u32 },
    #[error("signature does not verify over the body as received")]
    BadSignature,
    /// The key presented does not speak for the pool claimed, whatever it
    /// signed (`authority::Signed::authorised`).
    #[error(transparent)]
    Unauthorised(#[from] Unauthorised),
    /// The signature stands, so the pool sealed these bytes, and it sealed
    /// them too far from this server's clock for this to be the upload they
    /// were sealed for. A replay is what it refuses; a drifted clock is what
    /// an operator meets, so the refusal states both clocks.
    #[error(
        "sealed at {sealed_at}, and this server's clock reads {now}: over the \
         {max_secs}s either way that a submission may be sealed from"
    )]
    StaleTimestamp {
        sealed_at: OffsetDateTime,
        now: OffsetDateTime,
        max_secs: u32,
    },
    /// The signature stands, so these bytes are the pool's, and its header
    /// frame is not one this build can read a name out of.
    #[error("header frame does not read: {0}")]
    UnreadableHeader(#[from] HeaderError),
    /// Not schema gating: an accepted submission is filed under what it carries.
    /// The key scheme's kind segment is `<metrics|logs>` (`archive::Kind`), and
    /// a version this build has no `Kind` for has nothing to put in that
    /// segment. The refusal is the key, not the schema, having no name.
    #[error("schema v{schema_version} names no <metrics|logs> segment, so no key can be formed")]
    KeylessSchema { schema_version: u32 },
}

/// A submission the server could not process. `Rejected` is the client's
/// problem and permanent; `Unavailable` is the server's and worth a retry.
/// That distinction is what the HTTP layer turns into 4xx versus 5xx.
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
    /// Absent where this server was given no roster, which is what makes a
    /// Leios-key submission refusable with a reason rather than by a
    /// deployment that forgot to configure one (ADR 0011).
    roster: Option<Roster>,
}

impl<A: Store> Intake<A> {
    pub fn new(config: IngestConfig, archive: A, roster: Option<Roster>) -> Self {
        let limiter = RateLimiter::new(
            config.rate_limit_uploads,
            config.rate_limit_uploads_total,
            config.rate_limit_window_secs,
        );
        Intake {
            config,
            limiter: Mutex::new(limiter),
            archive,
            roster,
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
        // Before the signature, because it is a lookup against a signature's
        // pairing and answers the same question earlier: a key that cannot
        // speak for this pool is refused whatever it signed.
        signed
            .authorised(self.roster.as_ref())
            .map_err(Rejection::Unauthorised)?;
        // Before the limiter rather than after it. A verification key is
        // public, so anyone can present an allowlisted pool's and spend that
        // pool's window on bodies it never signed, which reaches the pool's
        // agent as a 429. Only what the signature has proved is charged, and
        // the cost of the reordering is one verify per forged body.
        if !signed.verifies() {
            return Err(Rejection::BadSignature.into());
        }
        // The header is read once, here, and handed to `accept`: the freshness
        // check needs it before the limiter is charged, because a replayed body
        // carries a signature that verifies and would otherwise spend the
        // window of the pool it was captured from.
        let header = read_header(signed.wire_bytes, self.config.max_header_bytes.get())
            .map_err(Rejection::UnreadableHeader)?;
        let skew = now - header.timestamp;
        let max_secs = self.config.max_timestamp_skew_secs.get();
        if skew.abs() > Duration::seconds(max_secs.into()) {
            return Err(Rejection::StaleTimestamp {
                sealed_at: header.timestamp,
                now,
                max_secs,
            }
            .into());
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
        self.accept(signed, pool_id, now, header)
    }

    /// The post-signature half: the submission is the pool's, so what it says about
    /// itself is what the object is filed under.
    fn accept(
        &self,
        signed: &Signed<'_>,
        pool_id: PoolId,
        now: OffsetDateTime,
        header: Header,
    ) -> Result<Ack, IngestError> {
        let kind = Kind::of(header.schema_version).ok_or(Rejection::KeylessSchema {
            schema_version: header.schema_version,
        })?;
        // The pool comes from the key and the agent from the header: the pool
        // is what the allowlist admitted, and which of its Agents reported is
        // not something the server has any other account of.
        let stored = StoredSubmission {
            name: ObjectName::stamped(now, pool_id, header.provenance.agent_id, kind),
            attestation: signed.attestation.clone(),
            wire_bytes: signed.wire_bytes,
        };
        self.archive.store(&stored)?;
        // Said here rather than beside `http::refuse`, because what is worth
        // saying about an accepted submission is the object it became, and
        // this is where that name is. The key carries the pool, the agent and
        // the kind (`archive::ObjectName`), so one line answers whose it was,
        // which of their Agents sent it and what it held, and naming the pool
        // beside it would only be that key's own middle segment again. A
        // refusal states the pool because it has no key to carry one. Without
        // this a journal shows only refusals, and an operator watching a
        // working server sees nothing but whatever scans the internet.
        eprintln!(
            "{INFO}accepted {}, {} bytes",
            stored.object_key(),
            signed.wire_bytes.len()
        );
        Ok(Ack {
            latest_version: crate::CLIENT_VERSION.to_string(),
        })
    }
}
