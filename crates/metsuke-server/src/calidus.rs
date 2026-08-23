//! The Calidus half of ADR 0003: which hot key a pool has registered on chain,
//! and how often the server is willing to ask. Resolving registrations is the
//! `Directory`'s job; choosing among them and remembering the answer is this
//! module's.

use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64};

use metsuke_wire::envelope::{PoolId, VerifyingKey};
use time::OffsetDateTime;

use crate::ratelimit::RateLimiter;

/// One CIP-151 registration, reduced to what the choice rests on. `key` is the
/// 32 bytes as registered rather than a `VerifyingKey`: metadata is written by
/// whoever pays for the transaction, so the bytes need not be a key at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub nonce: u64,
    pub key: [u8; 32],
}

/// The key CIP-151 revokes with.
const REVOKED: [u8; 32] = [0u8; 32];

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("cannot resolve pool {pool_id}'s Calidus registrations: {reason}")]
    Unavailable { pool_id: PoolId, reason: String },
}

/// Where a pool's CIP-151 registrations come from. A trait because the answer
/// is off-box: everything above it is decided from the rows alone.
pub trait Directory {
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Registration>, DirectoryError>;
}

/// The key a pool's registrations currently name, if any.
///
/// Highest nonce wins. Two different keys sharing that nonce name none: the
/// rows do not say which rotation is later, and a key that cannot be shown to
/// be the pool's must not speak for it.
pub fn current(registrations: &[Registration]) -> Option<VerifyingKey> {
    let newest = registrations.iter().max_by_key(|found| found.nonce)?;
    let contested = registrations
        .iter()
        .any(|found| found.nonce == newest.nonce && found.key != newest.key);
    if contested || newest.key == REVOKED {
        return None;
    }
    VerifyingKey::from_bytes(&newest.key).ok()
}

/// Every pool's Calidus key as last resolved.
///
/// Cache-forever with refresh-on-fail (ADR 0003): nothing expires, so db-sync
/// sees no per-upload load, and a rotation reaches the server through the
/// first upload the cached key cannot explain. `refresh` is what that upload
/// spends, and the budget is what stops a key nobody registered from costing a
/// query per attempt.
pub struct CalidusKeys<D: Directory> {
    directory: D,
    resolved: HashMap<PoolId, Option<VerifyingKey>>,
    refreshes: RateLimiter,
}

impl<D: Directory> CalidusKeys<D> {
    pub fn new(directory: D, max_refreshes: NonZeroU32, window_secs: NonZeroU64) -> Self {
        CalidusKeys {
            directory,
            resolved: HashMap::new(),
            refreshes: RateLimiter::new(max_refreshes, window_secs),
        }
    }

    /// The pool's key, resolved on the first ask and reused after that.
    /// `Cached` is what makes a refresh worth spending: an answer this call
    /// just fetched cannot change one line later.
    pub fn key_for(&mut self, pool_id: PoolId) -> Result<Resolution, DirectoryError> {
        if let Some(known) = self.resolved.get(&pool_id) {
            return Ok(Resolution::Cached(*known));
        }
        Ok(Resolution::Fetched(self.fetch(pool_id)?))
    }

    /// Resolve the pool again, spending one of its refreshes.
    pub fn refresh(
        &mut self,
        pool_id: PoolId,
        now: OffsetDateTime,
    ) -> Result<Refreshed, DirectoryError> {
        if !self.refreshes.allow(pool_id, now) {
            return Ok(Refreshed::Throttled);
        }
        Ok(Refreshed::Fetched(self.fetch(pool_id)?))
    }

    fn fetch(&mut self, pool_id: PoolId) -> Result<Option<VerifyingKey>, DirectoryError> {
        let resolved = current(&self.directory.registrations(pool_id)?);
        self.resolved.insert(pool_id, resolved);
        Ok(resolved)
    }
}

/// A pool's key, and whether answering cost a directory lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Cached(Option<VerifyingKey>),
    Fetched(Option<VerifyingKey>),
}

impl Resolution {
    pub fn key(self) -> Option<VerifyingKey> {
        match self {
            Resolution::Cached(key) | Resolution::Fetched(key) => key,
        }
    }
}

/// What a refresh cost. `Throttled` decided nothing: the cached answer stands
/// but was not re-checked, and a caller that reads it as a refusal turns a
/// spent budget into the pool's fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    Fetched(Option<VerifyingKey>),
    Throttled,
}
