//! Who a submission is from. One answer per scheme and both are checked, never
//! believed: a **Cold Key** hashes to the pool, so the pool is derived and a
//! claim beside it has to agree; a **Leios Key** hashes to nothing, so the
//! claim is all there is and it stands only where the **Key Roster** lists
//! that key for that pool and the signature stands over the bytes (ADR 0011).

use metsuke_wire::envelope::{Attestation, AttestationError, HEADER_POOL, PoolId, PoolIdError};

use crate::roster::Roster;

/// An attestation and the pool it is for, before the bytes are in hand. Built
/// from request headers, so a body is read for a pool that is already named.
#[derive(Debug)]
pub struct Attributed {
    pub attestation: Attestation,
    pool_id: PoolId,
}

#[derive(Debug, thiserror::Error)]
pub enum AttributionError {
    #[error(transparent)]
    Attestation(#[from] AttestationError),
    #[error("{HEADER_POOL} is not a pool id: {0}")]
    NotAPoolId(#[from] PoolIdError),
    #[error(
        "{HEADER_POOL} names {claimed}, and this cold key speaks for {derived}: \
         a pool id is its cold verification key's hash, so the two have to agree"
    )]
    NotItsPool { claimed: PoolId, derived: PoolId },
    #[error(
        "a Leios key names no pool, so a submission signed by one has to say \
         which pool it is from in {HEADER_POOL}"
    )]
    Unnamed,
}

impl Attributed {
    /// The three headers an upload presents. The pool header is what a Leios
    /// key has instead of a derivation, and a second copy of the derivation
    /// under a cold key: optional there, because an agent older than this
    /// build sends none (ADR 0006), and checked wherever it is sent.
    pub fn decode(
        vkey: Option<&str>,
        signature: Option<&str>,
        pool: Option<&str>,
    ) -> Result<Attributed, AttributionError> {
        let attestation = Attestation::decode(vkey, signature)?;
        let claimed = pool.map(PoolId::from_bech32).transpose()?;
        let pool_id = match (attestation.attributes(), claimed) {
            (Some(derived), None) => derived,
            (Some(derived), Some(claimed)) if derived == claimed => derived,
            (Some(derived), Some(claimed)) => {
                return Err(AttributionError::NotItsPool { claimed, derived });
            }
            (None, Some(claimed)) => claimed,
            (None, None) => return Err(AttributionError::Unnamed),
        };
        Ok(Attributed {
            attestation,
            pool_id,
        })
    }

    /// An object's attestation against the pool its key files it under, for
    /// the audit, which reads both off the archive rather than off a request.
    pub fn filed(attestation: Attestation, pool_id: PoolId) -> Attributed {
        Attributed {
            attestation,
            pool_id,
        }
    }

    /// Whose submission this is, as far as the headers go. What the allowlist
    /// is checked against and what a refusal names, before anything has proved
    /// it; `Signed::authorised` and `Signed::verifies` are what prove it.
    pub fn pool_id(&self) -> PoolId {
        self.pool_id
    }

    /// The same attribution over the bytes it was presented with.
    pub fn over<'a>(&self, wire_bytes: &'a [u8]) -> Signed<'a> {
        Signed {
            attestation: self.attestation.clone(),
            pool_id: self.pool_id,
            wire_bytes,
        }
    }
}

/// An upload as it was signed: the key it presents, the signature, the pool
/// both are for, and the bytes they are over. The ingest path builds one from
/// the request headers, the audit from a stored object.
pub struct Signed<'a> {
    pub attestation: Attestation,
    pool_id: PoolId,
    /// The body as sent, byte for byte as received.
    pub wire_bytes: &'a [u8],
}

/// Why a key does not speak for the pool the submission is filed under.
#[derive(Debug, thiserror::Error)]
pub enum Unauthorised {
    #[error("this server accepts no Leios-key submissions: it has no key roster")]
    NoRoster,
    #[error("the key roster lists no such Leios key for pool {pool_id}")]
    Unregistered { pool_id: PoolId },
}

impl Signed<'_> {
    /// Whose submission this is. The one answer, so the pool the allowlist is
    /// checked against, the pool the limiter charges and the pool the object
    /// is filed under cannot come out three different pools.
    pub fn pool_id(&self) -> PoolId {
        self.pool_id
    }

    /// Whether this key may speak for this pool at all. A cold key does by
    /// arithmetic, checked when the attribution was built; a Leios key does
    /// only where the roster says the chain registers it for this pool.
    ///
    /// Cheaper than `verifies` and independent of it, so it runs first: a
    /// forged body under an unregistered key costs a lookup rather than a
    /// pairing.
    pub fn authorised(&self, roster: Option<&Roster>) -> Result<(), Unauthorised> {
        let Attestation::LeiosKey { key, .. } = &self.attestation else {
            return Ok(());
        };
        let roster = roster.ok_or(Unauthorised::NoRoster)?;
        match roster.registers(self.pool_id, key) {
            true => Ok(()),
            false => Err(Unauthorised::Unregistered {
                pool_id: self.pool_id,
            }),
        }
    }

    /// Whether the signature stands over the bytes as given.
    pub fn verifies(&self) -> bool {
        self.attestation.verifies(self.wire_bytes)
    }
}
