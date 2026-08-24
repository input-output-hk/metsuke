//! Turning recorded label-867 blobs into the key a pool has registered
//! (ADR 0008), and how long the server reuses that answer (ADR 0003).

use metsuke_server::calidus::{CalidusKeys, Resolution, current};
use metsuke_server::cip151::{self, RegistrationError};

mod support;
use support::{
    CannedDirectory, TEST_TTL_SECS, calidus_key, crafted, nonzero_u32, other_key, pool_of,
    registered_pool, registration, rotated_calidus_key, test_now,
};

/// What a `Directory` hands up, run through the witness check the way
/// `CalidusKeys` does.
fn witnessed(blobs: &[Vec<u8>]) -> Vec<cip151::Registration> {
    blobs
        .iter()
        .filter_map(|blob| cip151::verify(registered_pool(), blob).ok())
        .collect()
}

// The fixtures are recorded against the suite's own signing keys, and every
// assertion below rests on that: a recorder pointed at other keys would leave
// the tests passing about a pool nothing else here mentions.
#[test]
fn the_recorded_registrations_scope_the_suites_own_pool() {
    let registration = cip151::verify(registered_pool(), &registration("nonce-1-key-a")).unwrap();
    assert_eq!(registration.nonce(), 1);
    assert_eq!(registration.key(), calidus_key().verifying_key().to_bytes());
}

// cardano-cli's metadata JSON is what an operator submits a registration as,
// and it is lossy about COSE's unprotected header. That the bytes db-sync
// hands back are the signer's own is what makes the offline recordings
// describe a chain.
#[test]
fn the_chain_hands_back_the_bytes_cardano_signer_emitted() {
    assert_eq!(
        registration("on-chain-nonce-1-key-a"),
        registration("nonce-1-key-a")
    );
}

#[test]
fn a_registration_scoping_another_pool_is_not_this_pools() {
    assert!(matches!(
        cip151::verify(registered_pool(), &registration("other-pool-nonce-1")),
        Err(RegistrationError::ScopesAnotherPool)
    ));
    assert!(cip151::verify(pool_of(&other_key()), &registration("other-pool-nonce-1")).is_ok());
}

// The scope names this pool and every signature in the blob is real, so only
// hashing the witness key against the scope refuses it.
#[test]
fn a_witness_that_is_not_the_scoped_pools_cold_key_registers_nothing() {
    assert!(matches!(
        cip151::verify(registered_pool(), &crafted("scope-mismatch")),
        Err(RegistrationError::Unwitnessed)
    ));
}

// `bytes` holds whatever a transaction put under some label, so both halves of
// "not a registration" arrive here: bytes that do not decode, and bytes that
// decode to something else entirely.
#[test]
fn bytes_that_are_not_a_registration_register_nothing() {
    assert!(matches!(
        cip151::verify(registered_pool(), &[0xa1]),
        Err(RegistrationError::NotCbor(_))
    ));
    assert!(matches!(
        cip151::verify(registered_pool(), b"\x6dnot a registration"),
        Err(RegistrationError::NotARegistration)
    ));
}

#[test]
fn a_pool_that_registered_nothing_has_no_calidus_key() {
    assert_eq!(current(&[]), Resolution::NeverRegistered);
}

// Rotation is by nonce, not by arrival: the highest wins wherever it sits.
#[test]
fn the_highest_nonce_registration_wins() {
    let registrations = witnessed(&[registration("nonce-1-key-a"), registration("nonce-5-key-b")]);
    assert_eq!(
        current(&registrations),
        Resolution::Key(rotated_calidus_key().verifying_key().to_bytes())
    );
}

#[test]
fn an_all_zero_key_revokes_the_pools_calidus_path() {
    let registrations = witnessed(&[
        registration("nonce-1-key-a"),
        registration("revoked-nonce-9"),
    ]);
    assert_eq!(current(&registrations), Resolution::Revoked);
}

// Two different keys at the same highest nonce name no winner, and an
// ambiguous registration that cannot grant upload rights is not the same news
// as never having registered.
#[test]
fn two_keys_sharing_the_highest_nonce_name_none() {
    let registrations = witnessed(&[registration("nonce-5-key-a"), registration("nonce-5-key-b")]);
    assert_eq!(current(&registrations), Resolution::Contested { nonce: 5 });
}

#[test]
fn the_same_key_registered_twice_at_one_nonce_is_not_ambiguous() {
    let registrations = witnessed(&[registration("nonce-5-key-a"), registration("nonce-5-key-a")]);
    assert_eq!(
        current(&registrations),
        Resolution::Key(calidus_key().verifying_key().to_bytes())
    );
}

// Anyone can post metadata, so the 32 bytes need not be a point at all.
#[test]
fn a_registration_that_is_not_a_key_names_none() {
    let registrations = witnessed(&[registration("not-a-key-nonce-3")]);
    assert_eq!(current(&registrations), Resolution::NotAKey { nonce: 3 });
}

/// A cache over a directory holding `registrations` for the recorded pool, and
/// the directory it asks.
fn keys_for(registrations: Vec<Vec<u8>>) -> (CalidusKeys<CannedDirectory>, CannedDirectory) {
    let directory = CannedDirectory::holding(registered_pool(), registrations);
    let keys = CalidusKeys::new(directory.clone(), nonzero_u32(TEST_TTL_SECS));
    (keys, directory)
}

#[test]
fn a_pool_is_resolved_once_and_reused_until_the_ttl_expires() {
    let (mut keys, directory) = keys_for(vec![registration("nonce-1-key-a")]);
    let pool = registered_pool();
    let registered = Resolution::Key(calidus_key().verifying_key().to_bytes());

    assert_eq!(keys.key_for(pool, test_now()).unwrap(), registered);
    assert_eq!(
        keys.key_for(pool, test_now() + time::Duration::seconds(1))
            .unwrap(),
        registered
    );
    assert_eq!(directory.lookups(), 1);
}

// The other half of the TTL: a rotation reaches a running server when the
// cached answer ages out, and not before.
#[test]
fn a_rotation_reaches_the_server_once_the_ttl_has_passed() {
    let (mut keys, directory) = keys_for(vec![registration("nonce-1-key-a")]);
    let pool = registered_pool();
    keys.key_for(pool, test_now()).unwrap();

    directory.rotate(pool, vec![registration("nonce-5-key-b")]);
    let stale = test_now() + time::Duration::seconds(i64::from(TEST_TTL_SECS) - 1);
    assert_eq!(
        keys.key_for(pool, stale).unwrap(),
        Resolution::Key(calidus_key().verifying_key().to_bytes())
    );

    let expired = test_now() + time::Duration::seconds(i64::from(TEST_TTL_SECS));
    assert_eq!(
        keys.key_for(pool, expired).unwrap(),
        Resolution::Key(rotated_calidus_key().verifying_key().to_bytes())
    );
    assert_eq!(directory.lookups(), 2);
}

// A row anyone could have posted must not cost the pool the registration it
// did make.
#[test]
fn an_unwitnessed_row_beside_a_real_one_is_dropped_rather_than_contesting_it() {
    let (mut keys, _directory) = keys_for(vec![
        registration("nonce-1-key-a"),
        crafted("scope-mismatch"),
        registration("other-pool-nonce-1"),
    ]);
    assert_eq!(
        keys.key_for(registered_pool(), test_now()).unwrap(),
        Resolution::Key(calidus_key().verifying_key().to_bytes())
    );
}

// One pool's cached answer is not another's, so a pool nobody registered does
// not inherit the key of one that did.
#[test]
fn one_pools_resolution_is_not_anothers() {
    let (mut keys, directory) = keys_for(vec![registration("nonce-1-key-a")]);
    keys.key_for(registered_pool(), test_now()).unwrap();

    assert_eq!(
        keys.key_for(pool_of(&other_key()), test_now()).unwrap(),
        Resolution::NeverRegistered
    );
    assert_eq!(directory.lookups(), 2);
}
