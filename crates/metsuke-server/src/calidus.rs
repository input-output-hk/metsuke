//! Which Calidus key a pool's registrations name, and how long the server
//! reuses that answer (ADR 0003). Finding the rows is the `Directory`'s job and
//! checking their witnesses is `cip151`'s; choosing among what passed is this
//! module's.

use std::collections::HashMap;
use std::num::NonZeroU32;

use metsuke_wire::envelope::{PoolId, VerifyingKey};
use time::{Duration, OffsetDateTime};

use crate::cip151::{self, Registration};

/// The key CIP-151 revokes with.
const REVOKED: [u8; 32] = [0u8; 32];

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("cannot resolve pool {pool_id}'s Calidus registrations: {reason}")]
    Unavailable { pool_id: PoolId, reason: String },
}

/// What a directory found under one pool's scope. Why the bound past which it
/// hands up nothing is a refusal and not a truncation: ADR 0008.
#[derive(Debug, PartialEq, Eq)]
pub enum Registrations {
    Rows(Vec<Vec<u8>>),
    TooMany { max: u32 },
}

impl Registrations {
    /// One bounded read. `query` is handed the row bound to ask a chain for and
    /// answers with what came back; the bound is named nowhere else, so no
    /// caller can ask for one the answer is not then judged against.
    pub fn bounded<E>(
        max: NonZeroU32,
        query: impl FnOnce(i64) -> Result<Vec<Vec<u8>>, E>,
    ) -> Result<Registrations, E> {
        // The smallest bound that tells a scope at the cap from one over it.
        // Which is which: tests/calidus.rs.
        let rows = query(i64::from(max.get()) + 1)?;
        Ok(match rows.len() as u64 > u64::from(max.get()) {
            true => Registrations::TooMany { max: max.get() },
            false => Registrations::Rows(rows),
        })
    }
}

/// Where a pool's label-867 rows come from, as the bytes the chain carries. A
/// directory that handed up `Registration`s would be asserting a witness it
/// never checked, and a test double could then fabricate authority.
pub trait Directory {
    fn registrations(&self, pool_id: PoolId) -> Result<Registrations, DirectoryError>;
}

/// What a pool's witnessed registrations say. The refusals are separate because
/// each is a different thing for the operator to do, and collapsing them would
/// make a contested rotation read as a pool that never registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The registered key: witnessed, highest nonce, and a point on the curve.
    /// The bytes rather than a `VerifyingKey` because this travels inside a
    /// refusal, and a decompressed point there would make every error the
    /// server returns carry one.
    Key([u8; 32]),
    NeverRegistered,
    Revoked,
    /// Two keys share the highest nonce, so the rows do not say which rotation
    /// is later. Re-posting one registration above them is the fix.
    Contested {
        nonce: u64,
    },
    /// The highest nonce names 32 bytes that are not a point on the curve.
    NotAKey {
        nonce: u64,
    },
    /// More rows scope this pool than the server verifies, so none of them was
    /// read. The one variant here that is not something the rows say.
    TooMany {
        max: u32,
    },
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Resolution::Key(_) => f.write_str("a Calidus key is registered"),
            Resolution::NeverRegistered => f.write_str("no Calidus key is registered"),
            Resolution::Revoked => f.write_str("the Calidus key was revoked"),
            Resolution::Contested { nonce } => {
                write!(f, "two Calidus keys share nonce {nonce}")
            }
            Resolution::NotAKey { nonce } => {
                write!(f, "the key registered at nonce {nonce} is not on the curve")
            }
            Resolution::TooMany { max } => {
                write!(f, "more than {max} registrations scope this pool")
            }
        }
    }
}

/// The key a pool's registrations currently name.
///
/// Highest nonce wins. Two different keys sharing that nonce name none: the
/// rows do not say which rotation is later, and a key that cannot be shown to
/// be the pool's must not speak for it.
pub fn current(registrations: &[Registration]) -> Resolution {
    let Some(newest) = registrations.iter().max_by_key(|found| found.nonce()) else {
        return Resolution::NeverRegistered;
    };
    let nonce = newest.nonce();
    if registrations
        .iter()
        .any(|found| found.nonce() == nonce && found.key() != newest.key())
    {
        return Resolution::Contested { nonce };
    }
    if newest.key() == REVOKED {
        return Resolution::Revoked;
    }
    match VerifyingKey::from_bytes(&newest.key()) {
        Ok(_) => Resolution::Key(newest.key()),
        Err(_) => Resolution::NotAKey { nonce },
    }
}

/// Every pool's Calidus key as last resolved, for as long as the TTL says.
/// What the TTL is chosen against: ADR 0008.
pub struct CalidusKeys<D: Directory> {
    directory: D,
    ttl: Duration,
    resolved: HashMap<PoolId, (OffsetDateTime, Resolution)>,
}

impl<D: Directory> CalidusKeys<D> {
    pub fn new(directory: D, ttl_secs: NonZeroU32) -> Self {
        CalidusKeys {
            directory,
            ttl: Duration::seconds(i64::from(ttl_secs.get())),
            resolved: HashMap::new(),
        }
    }

    /// The pool's key, resolved again once the last answer is older than the
    /// TTL.
    pub fn key_for(
        &mut self,
        pool_id: PoolId,
        now: OffsetDateTime,
    ) -> Result<Resolution, DirectoryError> {
        if let Some((resolved_at, resolution)) = self.resolved.get(&pool_id)
            && now - *resolved_at < self.ttl
        {
            return Ok(*resolution);
        }
        let resolution = match self.directory.registrations(pool_id)? {
            Registrations::TooMany { max } => Resolution::TooMany { max },
            Registrations::Rows(rows) => current(&witnessed(pool_id, &rows)),
        };
        self.resolved.insert(pool_id, (now, resolution));
        Ok(resolution)
    }
}

/// The rows that are this pool's own. A row whose witness does not check is a
/// stranger's transaction, so it is dropped rather than reported: anyone can
/// post one, and naming them per pool would let a stranger fill the log.
fn witnessed(pool_id: PoolId, rows: &[Vec<u8>]) -> Vec<Registration> {
    rows.iter()
        .filter_map(|blob| cip151::verify(pool_id, blob).ok())
        .collect()
}
