//! A submission signed by a **Leios Key**: what the roster has to say before
//! the signature is worth checking, and what it is filed under once both hold
//! (ADR 0011).

use metsuke_server::archive::{FilesystemArchive, List};
use metsuke_server::intake::{IngestError, Intake, Rejection};
use metsuke_server::roster::Roster;
use metsuke_wire::envelope::{PoolId, SubmissionKey};

mod support;
use support::{
    envelope_for, permissive_config, pool_of, seal_with, submission, test_key, test_leios_key,
    test_now,
};

/// A server holding a roster that lists `keys` for `pool`, with `pool`
/// allowlisted. The directory is returned because dropping it takes the
/// archive and the roster with it.
fn intake_listing(
    pool: PoolId,
    keys: &[&SubmissionKey],
) -> (Intake<FilesystemArchive>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let listed: Vec<String> = keys
        .iter()
        .map(|key| format!("\"{}\"", key.public_key_hex()))
        .collect();
    std::fs::write(
        &path,
        format!(
            r#"{{"epoch": 1, "slot": 2, "pools": {{"{pool}": [{}]}}}}"#,
            listed.join(", "),
            pool = metsuke_wire::hex::encode(pool.as_hash())
        ),
    )
    .expect("the roster writes");
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    let roster = Roster::load(&path).expect("the roster loads");
    (
        Intake::new(permissive_config(&[pool]), archive, Some(roster)),
        dir,
    )
}

fn rejection(error: IngestError) -> Rejection {
    match error {
        IngestError::Rejected(rejection) => rejection,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// The whole point: an agent holding no cold key still reports, and the object
/// lands under the pool it claimed, because the roster and the signature both
/// said so.
#[test]
fn a_leios_signed_submission_is_accepted_and_filed_under_the_pool_it_claims() {
    let pool = pool_of(&test_key());
    let key = test_leios_key(1);
    let (intake, _dir) = intake_listing(pool, &[&key]);
    let (body, attestation) = seal_with(&key, &envelope_for(&test_key(), 1));

    intake
        .submit(&submission(attestation, pool, &body), test_now())
        .expect("the roster lists this key for this pool");

    let stored = intake.archive().keys().unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].contains(&pool.to_bech32()),
        "filed under the claimed pool: {}",
        stored[0]
    );
}

/// The claim is a lookup hint and nothing more. A pool cannot be spoken for by
/// a key the chain does not register for it, whatever that key signed.
#[test]
fn a_key_the_roster_does_not_list_for_the_claimed_pool_is_refused() {
    let pool = pool_of(&test_key());
    let (intake, _dir) = intake_listing(pool, &[&test_leios_key(1)]);
    let impostor = test_leios_key(2);
    let (body, attestation) = seal_with(&impostor, &envelope_for(&test_key(), 1));

    let error = intake
        .submit(&submission(attestation, pool, &body), test_now())
        .unwrap_err();

    match rejection(error) {
        Rejection::Unauthorised(reason) => {
            let text = reason.to_string();
            assert!(text.contains(&pool.to_bech32()), "got: {text}");
        }
        other => panic!("expected an unauthorised key, got {other:?}"),
    }
    assert!(intake.archive().keys().unwrap().is_empty());
}

/// Off the allowlist is refused before the roster is consulted, as it is
/// before the signature: participation is prior to identity.
#[test]
fn a_pool_off_the_allowlist_is_refused_though_the_roster_lists_its_key() {
    let pool = pool_of(&test_key());
    let key = test_leios_key(1);
    let (intake, _dir) = intake_listing(pool, &[&key]);
    let stranger = pool_of(&support::other_key());
    let (body, attestation) = seal_with(&key, &envelope_for(&test_key(), 1));

    let error = intake
        .submit(&submission(attestation, stranger, &body), test_now())
        .unwrap_err();

    assert!(matches!(rejection(error), Rejection::UnknownPool { .. }));
}

/// A server with no roster cannot judge a Leios key at all, and says so rather
/// than accepting one against nothing.
#[test]
fn a_server_without_a_roster_refuses_every_leios_key() {
    let pool = pool_of(&test_key());
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    let intake = Intake::new(permissive_config(&[pool]), archive, None);
    let key = test_leios_key(1);
    let (body, attestation) = seal_with(&key, &envelope_for(&test_key(), 1));

    let error = intake
        .submit(&submission(attestation, pool, &body), test_now())
        .unwrap_err();

    let text = rejection(error).to_string();
    assert!(text.contains("no key roster"), "got: {text}");
}

/// The roster says the key may speak for the pool; only the signature says
/// these bytes are the pool's.
#[test]
fn a_registered_key_over_bytes_it_did_not_sign_is_refused() {
    let pool = pool_of(&test_key());
    let key = test_leios_key(1);
    let (intake, _dir) = intake_listing(pool, &[&key]);
    let (mut body, attestation) = seal_with(&key, &envelope_for(&test_key(), 1));
    let last = body.len() - 1;
    body[last] ^= 0xff;

    let error = intake
        .submit(&submission(attestation, pool, &body), test_now())
        .unwrap_err();

    assert!(matches!(rejection(error), Rejection::BadSignature));
}

/// The roster and the claim settle which pool the key speaks for; the header
/// inside the signed bytes claims a pool of its own, and every payload line
/// carries that one. A Leios key derives nothing, so the two are asked to
/// agree or nothing catches them disagreeing.
#[test]
fn a_leios_submission_whose_header_names_another_pool_is_refused() {
    let pool = pool_of(&test_key());
    let key = test_leios_key(1);
    let (intake, _dir) = intake_listing(pool, &[&key]);
    let stranger = pool_of(&support::other_key());
    let (body, attestation) = seal_with(&key, &support::envelope_claiming(stranger, 1));

    let error = intake
        .submit(&submission(attestation, pool, &body), test_now())
        .unwrap_err();

    let rejection = rejection(error);
    assert!(
        matches!(rejection, Rejection::NotItsProvenance { .. }),
        "got: {rejection:?}"
    );
    assert!(intake.archive().keys().unwrap().is_empty());
}

/// Both keys listed at once is a rotation in flight, and both are accepted for
/// as long as the chain registers both.
#[test]
fn either_of_a_pools_listed_keys_is_accepted() {
    let pool = pool_of(&test_key());
    let (intake, _dir) = intake_listing(pool, &[&test_leios_key(1), &test_leios_key(2)]);

    for (counter, seed) in [(1u64, 1u8), (2, 2)] {
        let key = test_leios_key(seed);
        let (body, attestation) = seal_with(&key, &envelope_for(&test_key(), counter));
        intake
            .submit(&submission(attestation, pool, &body), test_now())
            .expect("both listed keys speak for the pool");
    }

    assert_eq!(intake.archive().keys().unwrap().len(), 2);
}
