//! Reconstruct the replay index from the archive listing (ADR 0005). The
//! bucket is the source of truth, so this is what a server whose disk was lost
//! runs before it accepts again.

use std::collections::HashMap;

use metsuke::envelope::PoolId;

use crate::archive::{Archive, ArchiveError, ObjectName, ObjectNameError};
use crate::counters::{CounterError, CounterStore, Reservation};

/// What the rebuild found, for the operator to compare against what they
/// expected to be in the bucket.
#[derive(Debug)]
pub struct RebuiltIndex {
    pub objects: usize,
    /// One entry per pool that has an object, ordered by pool id.
    pub pools: Vec<SeededPool>,
}

/// A pool's newest object and what the index did with it.
#[derive(Debug, PartialEq, Eq)]
pub struct SeededPool {
    pub newest: ObjectName,
    /// `false` when the index already held a counter past this object, so the
    /// listing did not become the pool's state.
    pub seeded: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    ObjectName(#[from] ObjectNameError),
    #[error(transparent)]
    Counters(#[from] CounterError),
}

/// Seed every pool's counter from its newest stored object, writing through
/// `reserve` (see `SeededPool::seeded`).
pub fn rebuild(
    archive: &impl Archive,
    counters: &mut CounterStore,
) -> Result<RebuiltIndex, RebuildError> {
    let keys = archive.list_keys()?;
    let mut newest: HashMap<PoolId, ObjectName> = HashMap::new();
    for key in &keys {
        let name = ObjectName::parse(key)?;
        newest
            .entry(name.pool_id)
            .and_modify(|held| {
                if name.counter > held.counter {
                    *held = name;
                }
            })
            .or_insert(name);
    }
    let mut newest: Vec<ObjectName> = newest.into_values().collect();
    newest.sort_by_key(|name| name.pool_id.to_bech32());
    let mut pools = Vec::with_capacity(newest.len());
    for name in newest {
        let seeded = match counters.reserve(name.pool_id, name.counter, name.timestamp)? {
            Reservation::Reserved(reserved) => {
                reserved.commit()?;
                true
            }
            Reservation::Replayed { .. } => false,
        };
        pools.push(SeededPool {
            newest: name,
            seeded,
        });
    }
    Ok(RebuiltIndex {
        objects: keys.len(),
        pools,
    })
}
