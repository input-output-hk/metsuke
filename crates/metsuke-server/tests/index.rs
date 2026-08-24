//! Index tests. The counter half (ticket metsuke-4zo.6): the acceptance rule
//! is "strictly greater than everything accepted for this pool", and it
//! holds across restarts because the state is the SQLite row, not memory. The
//! submission half (metsuke-4zo.10): what the developer listing reads.

use metsuke_server::archive::ObjectName;
use metsuke_server::index::{Index, IndexError, Reservation};
use metsuke_wire::envelope::PoolId;

mod support;
use proptest::prelude::*;
use std::collections::HashMap;
use support::{index_store, nonzero_u32};
use time::OffsetDateTime;

/// An object of `pool`, keyed at `counter` seconds past the epoch so its key
/// sorts by counter.
fn object(pool: PoolId, counter: u64) -> ObjectName {
    ObjectName {
        pool_id: pool,
        counter,
        timestamp: OffsetDateTime::from_unix_timestamp(counter as i64).unwrap(),
    }
}

/// The listing filters and the bound the developer endpoint passes.
fn listed(index: &Index, prefix: &str, after: &str) -> Vec<String> {
    index
        .submissions(prefix, after, nonzero_u32(100))
        .unwrap()
        .objects
        .iter()
        .map(ObjectName::to_key)
        .collect()
}

/// The listing is what a developer pull reads instead of scanning the bucket
/// (ADR 0005), so a stored object has to reach it.
#[test]
fn a_recorded_object_is_listed_back() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let name = object(pool(1), 7);

    index.record(&name).unwrap();

    assert_eq!(listed(&index, "", ""), vec![name.to_key()]);
}

/// A developer pulling one pool's submissions asks by key prefix, which is
/// what the ADR-0005 key makes a pool-and-day filter.
#[test]
fn a_prefix_lists_only_the_keys_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let mine = object(pool(1), 1);
    let theirs = object(pool(2), 1);
    index.record(&mine).unwrap();
    index.record(&theirs).unwrap();

    let prefix = format!("v1/{}/", mine.pool_id);
    assert_eq!(listed(&index, &prefix, ""), vec![mine.to_key()]);
}

/// `after` is the page cursor: handing back the last key of a page must
/// answer the next one, or a developer paging a pool's history loops.
#[test]
fn after_resumes_at_the_key_that_follows_it() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let pool = pool(1);
    for counter in 1..=3 {
        index.record(&object(pool, counter)).unwrap();
    }

    let second = object(pool, 2).to_key();
    assert_eq!(
        listed(&index, "", &second),
        vec![object(pool, 3).to_key()],
        "the cursor key itself is behind the page it names"
    );
}

/// A page cut off by the bound says so. A developer reading a short page as
/// the whole archive would miss every object past it.
#[test]
fn a_listing_over_the_limit_is_reported_as_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let pool = pool(1);
    for counter in 1..=3 {
        index.record(&object(pool, counter)).unwrap();
    }

    let page = index.submissions("", "", nonzero_u32(2)).unwrap();
    assert!(page.truncated);
    assert_eq!(
        page.objects
            .iter()
            .map(ObjectName::to_key)
            .collect::<Vec<_>>(),
        vec![object(pool, 1).to_key(), object(pool, 2).to_key()],
        "a truncated page carries the bound's worth, not one more"
    );

    let whole = index.submissions("", "", nonzero_u32(3)).unwrap();
    assert!(!whole.truncated, "an exact fit is not truncated");
    assert_eq!(whole.objects.len(), 3);
}

/// A row the listing cannot parse fails it rather than shortening the page
/// (`IndexError::ObjectName`).
#[test]
fn a_key_the_listing_cannot_parse_fails_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite");
    let index = Index::open(&path).unwrap();
    index.record(&object(pool(1), 1)).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO submissions (object_key) VALUES ('v1/not-a-key')",
            [],
        )
        .unwrap();

    let Err(error) = index.submissions("", "", nonzero_u32(100)) else {
        panic!("an unparseable key must not be dropped from the page");
    };

    assert!(
        error.to_string().contains("not-a-key"),
        "the error must name the key it could not read, got {error}"
    );
}

/// `rebuild-index` walks the whole bucket, so it re-records keys the index
/// already holds. That is a no-op, not a failure.
#[test]
fn recording_the_same_object_twice_leaves_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let name = object(pool(1), 1);

    index.record(&name).unwrap();
    index.record(&name).unwrap();

    assert_eq!(listed(&index, "", ""), vec![name.to_key()]);
}

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
fn spend(store: &mut Index, pool: PoolId, counter: u64) -> Result<Option<u64>, IndexError> {
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
    let path = dir.path().join("index.sqlite");
    let mut store = Index::open(&path).unwrap();
    spend(&mut store, pool(1), 5).unwrap();
    drop(store);

    let mut store = Index::open(&path).unwrap();
    assert_eq!(store.last_counter(pool(1)).unwrap(), Some(5));
    assert_eq!(spend(&mut store, pool(1), 5).unwrap(), Some(5));
    assert_eq!(spend(&mut store, pool(1), 6).unwrap(), None);
}

// A reservation the caller never commits — its batch failed to store —
// leaves the counter unspent, so the client's retry is not a replay.
#[test]
fn a_dropped_reservation_leaves_the_counter_unspent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = index_store(dir.path());
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
    let path = dir.path().join("index.sqlite");
    Index::open(&path).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99u32)
        .unwrap();

    let Err(error) = Index::open(&path) else {
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
    let store = index_store(dir.path());
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
        let mut store = index_store(dir.path());
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
