//! Who may speak for a pool (ADR 0003), and the one check-then-open sequence
//! both the ingest path and the archive audit run. Whether the two reach the
//! same verdict is their `Authority`, not this sequence.

use metsuke_wire::envelope::{self, Envelope, PoolId, SCHEMA_VERSION, Signature, VerifyingKey};
use time::OffsetDateTime;

use crate::calidus::{CalidusKeys, Directory, DirectoryError, Refreshed, Resolution};

/// An upload as it was signed: the pool it claims, the key it presents, the
/// signature, and the bytes both are over. The ingest path builds one from the
/// request headers, the audit from a stored object.
pub struct Signed<'a> {
    pub pool_id: PoolId,
    pub vkey: VerifyingKey,
    pub signature: Signature,
    /// The compressed body, byte for byte as received.
    pub wire_bytes: &'a [u8],
}

/// Why the server could not tell whether a key speaks for a pool. Neither is
/// the upload's fault: no answer was reached, so both are worth a retry.
#[derive(Debug, thiserror::Error)]
pub enum Undecided {
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    #[error("pool {pool_id} has spent its Calidus refreshes for this window")]
    RefreshThrottled { pool_id: PoolId },
}

/// Whether a verification key may speak for a pool. A trait rather than a
/// function because the Calidus half needs a directory the cold-key half has
/// no use for.
pub trait Authority {
    fn speaks_for(
        &mut self,
        pool_id: PoolId,
        vkey: &VerifyingKey,
        now: OffsetDateTime,
    ) -> Result<bool, Undecided>;
}

/// The cold-key half alone, which is every server without a directory.
pub struct ColdKey;

impl Authority for ColdKey {
    fn speaks_for(
        &mut self,
        pool_id: PoolId,
        vkey: &VerifyingKey,
        _now: OffsetDateTime,
    ) -> Result<bool, Undecided> {
        Ok(PoolId::from_cold_key(vkey) == pool_id)
    }
}

/// Both halves, cold key first. Which of the two an SPO chose is not something
/// the upload states, so both are tried.
pub struct ColdKeyOrCalidus<D: Directory> {
    keys: CalidusKeys<D>,
}

impl<D: Directory> ColdKeyOrCalidus<D> {
    pub fn new(keys: CalidusKeys<D>) -> Self {
        ColdKeyOrCalidus { keys }
    }
}

impl<D: Directory> Authority for ColdKeyOrCalidus<D> {
    fn speaks_for(
        &mut self,
        pool_id: PoolId,
        vkey: &VerifyingKey,
        now: OffsetDateTime,
    ) -> Result<bool, Undecided> {
        if PoolId::from_cold_key(vkey) == pool_id {
            return Ok(true);
        }
        let resolved = self.keys.key_for(pool_id)?;
        if resolved.key() == Some(*vkey) {
            return Ok(true);
        }
        if let Resolution::Fetched(_) = resolved {
            return Ok(false);
        }
        match self.keys.refresh(pool_id, now)? {
            Refreshed::Fetched(key) => Ok(key == Some(*vkey)),
            Refreshed::Throttled => Err(Undecided::RefreshThrottled { pool_id }),
        }
    }
}

/// Why an upload is not the envelope it claims to be. The ingest path and the
/// audit report these differently, so the mapping is theirs and the
/// distinctions are here.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the presented key does not speak for pool {pool_id}")]
    UnauthorizedKey { pool_id: PoolId },
    #[error("signature does not verify over the bytes as given")]
    BadSignature,
    #[error("payload inflates past the {max} byte limit")]
    OversizedPayload { max: u64 },
    #[error("payload is not a schema v{SCHEMA_VERSION} envelope: {reason}")]
    MalformedPayload { reason: String },
    #[error(transparent)]
    Undecided(#[from] Undecided),
}

/// Check the key against the pool, then open the envelope: nothing is
/// decompressed before its signature verifies, which is the ADR-0002 invariant
/// this order answers to.
pub fn authenticate(
    authority: &mut impl Authority,
    signed: &Signed<'_>,
    max_decompressed_bytes: u64,
    now: OffsetDateTime,
) -> Result<Envelope, AuthError> {
    if !authority.speaks_for(signed.pool_id, &signed.vkey, now)? {
        return Err(AuthError::UnauthorizedKey {
            pool_id: signed.pool_id,
        });
    }
    envelope::open(
        &signed.vkey,
        signed.wire_bytes,
        &signed.signature,
        max_decompressed_bytes,
    )
    .map_err(|error| match error {
        envelope::OpenError::Signature(_) => AuthError::BadSignature,
        envelope::OpenError::TooLarge {
            max_decompressed_bytes,
        } => AuthError::OversizedPayload {
            max: max_decompressed_bytes,
        },
        envelope::OpenError::Decompress(error) => AuthError::MalformedPayload {
            reason: error.to_string(),
        },
        envelope::OpenError::Json(error) => AuthError::MalformedPayload {
            reason: error.to_string(),
        },
    })
}
