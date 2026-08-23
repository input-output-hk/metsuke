//! Replay-counter store tests (ticket metsuke-4zo.6): the acceptance rule
//! is "strictly greater than everything accepted for this pool", and it
//! holds across restarts because the state is the SQLite row, not memory.

use metsuke_server::counters::{CounterError, CounterStore, Reservation};
use metsuke_wire::envelope::PoolId;

mod support;
use proptest::prelude::*;
use std::collections::HashMap;
use support::counter_store;
use time::OffsetDateTime;

/// Acceptance here is judged on counters alone, so the instant recorded
/// alongside them never varies.
fn test_now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

fn pool(index: u8) -> PoolId {
    let key = metsuke_wire::envelope::SigningKey::from_bytes(&[index; 32]);
    PoolId::from_cold_key(&key.verifying_key())
}

/// Reserve and immediately spend the counter, as a caller does once it has
/// stored the batch. `None` when the counter was replayed.
fn spend(
    store: &mut CounterStore,
    pool: PoolId,
    counter: u64,
) -> Result<Option<u64>, CounterError> {
    match store.reserve(pool, counter, test_now())? {
        Reservation::Reserved(reserved) => {
            reserved.commit()?;
            Ok(None)
        }
        Reservation::Replayed { last } => Ok(Some(last)),
    }
}

// State survives the process: a store reopened on the same file still
// refuses what it already accepted.
#[test]
fn accepted_counters_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counters.sqlite");
    let mut store = CounterStore::open(&path).unwrap();
    spend(&mut store, pool(1), 5).unwrap();
    drop(store);

    let mut store = CounterStore::open(&path).unwrap();
    assert_eq!(store.last_counter(pool(1)).unwrap(), Some(5));
    assert_eq!(spend(&mut store, pool(1), 5).unwrap(), Some(5));
    assert_eq!(spend(&mut store, pool(1), 6).unwrap(), None);
}

// A reservation the caller never commits — its batch failed to store —
// leaves the counter unspent, so the client's retry is not a replay.
#[test]
fn a_dropped_reservation_leaves_the_counter_unspent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = counter_store(dir.path());
    let Ok(Reservation::Reserved(reserved)) = store.reserve(pool(1), 5, test_now()) else {
        panic!("a first counter must reserve");
    };
    drop(reserved);

    assert_eq!(store.last_counter(pool(1)).unwrap(), None);
    assert_eq!(spend(&mut store, pool(1), 5).unwrap(), None);
}

// A database written by a newer build is refused: running it half-understood
// would silently drop whatever the newer schema records.
#[test]
fn a_database_from_a_newer_build_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counters.sqlite");
    CounterStore::open(&path).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99u32)
        .unwrap();

    let Err(error) = CounterStore::open(&path) else {
        panic!("opening a newer schema must fail");
    };

    assert!(
        error.to_string().contains("99"),
        "the error must name the version found, got {error}"
    );
}

#[test]
fn a_pool_with_no_history_has_no_counter() {
    let dir = tempfile::tempdir().unwrap();
    let store = counter_store(dir.path());
    assert_eq!(store.last_counter(pool(1)).unwrap(), None);
}

proptest! {
    // Acceptance: for any interleaving of pools and counters, a counter is
    // accepted exactly when it is above that pool's running maximum, and
    // the stored maximum is that maximum.
    #[test]
    fn acceptance_is_monotonic_per_pool(
        submissions in proptest::collection::vec((0u8..4, 0u64..40), 0..80)
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = counter_store(dir.path());
        let mut highest: HashMap<u8, u64> = HashMap::new();

        for (index, counter) in submissions {
            let replayed = spend(&mut store, pool(index), counter).unwrap();
            match highest.get(&index) {
                Some(&last) if counter <= last => prop_assert_eq!(replayed, Some(last)),
                _ => {
                    prop_assert_eq!(replayed, None);
                    highest.insert(index, counter);
                }
            }
        }

        for (index, expected) in highest {
            prop_assert_eq!(store.last_counter(pool(index)).unwrap(), Some(expected));
        }
    }
}
