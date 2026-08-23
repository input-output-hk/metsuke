//! Choosing a pool's Calidus key out of what the chain holds, and how often
//! the server is willing to ask for it (ADR 0003).

use metsuke_server::calidus::{CalidusKeys, Refreshed, Registration, Resolution, current};

mod support;
use support::{
    CannedDirectory, nonzero_u32, nonzero_u64, other_key, pool_of, registration, test_key, test_now,
};

fn revocation(nonce: u64) -> Registration {
    Registration {
        nonce,
        key: [0u8; 32],
    }
}

#[test]
fn a_pool_that_registered_nothing_has_no_calidus_key() {
    assert_eq!(current(&[]), None);
}

// Rotation is by nonce, not by arrival: the highest wins wherever it sits.
#[test]
fn the_highest_nonce_registration_wins() {
    let newest = other_key();
    let registrations = [
        registration(&test_key(), 5),
        registration(&newest, 9),
        registration(&test_key(), 1),
    ];
    assert_eq!(current(&registrations), Some(newest.verifying_key()));
}

#[test]
fn an_all_zero_key_revokes_the_pools_calidus_path() {
    let registrations = [registration(&test_key(), 1), revocation(2)];
    assert_eq!(current(&registrations), None);
}

// Two different keys at the same highest nonce name no winner, and the reading
// of an ambiguous registration that cannot grant upload rights is no key.
#[test]
fn two_keys_sharing_the_highest_nonce_name_none() {
    let registrations = [registration(&test_key(), 4), registration(&other_key(), 4)];
    assert_eq!(current(&registrations), None);
}

#[test]
fn the_same_key_registered_twice_at_one_nonce_is_not_ambiguous() {
    let key = test_key();
    let registrations = [registration(&key, 4), registration(&key, 4)];
    assert_eq!(current(&registrations), Some(key.verifying_key()));
}

// Anyone can post metadata, so the 32 bytes need not be a point at all. These
// are y = 2, which no point on the curve has.
#[test]
fn a_registration_that_is_not_a_key_names_none() {
    let mut key = [0u8; 32];
    key[0] = 2;
    let registrations = [Registration { nonce: 1, key }];
    assert_eq!(current(&registrations), None);
}

/// A cache over a directory holding `registrations` for the test pool, and the
/// directory it asks.
fn keys_for(registrations: Vec<Registration>) -> (CalidusKeys<CannedDirectory>, CannedDirectory) {
    let directory = CannedDirectory::holding(pool_of(&test_key()), registrations);
    let keys = CalidusKeys::new(directory.clone(), nonzero_u32(1), nonzero_u64(3600));
    (keys, directory)
}

// Cache-forever: the second ask is answered without touching the directory.
// Which of the two happened is the answer itself, because a caller deciding
// whether to spend a refresh cannot tell from the key alone.
#[test]
fn a_pool_is_resolved_once_and_reused() {
    let (mut keys, directory) = keys_for(vec![registration(&other_key(), 1)]);
    let pool = pool_of(&test_key());
    let registered = Some(other_key().verifying_key());

    assert_eq!(keys.key_for(pool).unwrap(), Resolution::Fetched(registered));
    assert_eq!(keys.key_for(pool).unwrap(), Resolution::Cached(registered));
    assert_eq!(directory.lookups(), 1);
}

// The other half of cache-forever: a rotation reaches the server only through
// a refresh.
#[test]
fn a_refresh_asks_again_and_replaces_the_cached_key() {
    let (mut keys, directory) = keys_for(vec![registration(&test_key(), 1)]);
    let pool = pool_of(&test_key());
    keys.key_for(pool).unwrap();

    directory.rotate(pool, vec![registration(&other_key(), 2)]);
    let rotated = Some(other_key().verifying_key());

    assert_eq!(
        keys.refresh(pool, test_now()).unwrap(),
        Refreshed::Fetched(rotated)
    );
    assert_eq!(keys.key_for(pool).unwrap(), Resolution::Cached(rotated));
    assert_eq!(directory.lookups(), 2);
}

#[test]
fn a_pool_that_has_spent_its_refresh_budget_is_not_asked_again() {
    let (mut keys, directory) = keys_for(vec![registration(&test_key(), 1)]);
    let pool = pool_of(&test_key());
    keys.refresh(pool, test_now()).unwrap();

    assert_eq!(
        keys.refresh(pool, test_now()).unwrap(),
        Refreshed::Throttled
    );
    assert_eq!(directory.lookups(), 1);
}

#[test]
fn one_pool_spending_its_refreshes_does_not_spend_anothers() {
    let (mut keys, _directory) = keys_for(vec![registration(&test_key(), 1)]);
    let busy = pool_of(&test_key());
    let quiet = pool_of(&other_key());
    keys.refresh(busy, test_now()).unwrap();
    assert_eq!(
        keys.refresh(busy, test_now()).unwrap(),
        Refreshed::Throttled
    );

    assert!(matches!(
        keys.refresh(quiet, test_now()).unwrap(),
        Refreshed::Fetched(_)
    ));
}
