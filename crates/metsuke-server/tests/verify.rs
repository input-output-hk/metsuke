//! Re-verifying a stored object. Each test builds the object the archive would
//! hand back and breaks exactly one thing about it, so what the verification
//! rests on is visible one failure at a time.

use metsuke_server::archive::{FetchedObject, ObjectName};
use metsuke_server::authority::{ColdKey, ColdKeyOrCalidus};
use metsuke_server::calidus::CalidusKeys;
use metsuke_server::verify::{Audit, AuditFailure, VerifyError, verify};
use metsuke_wire::envelope::{Envelope, Limits, SigningKey};

mod support;
use support::{
    CannedDirectory, TEST_LIMITS, TEST_TTL_SECS, UnavailableDirectory, calidus_authority,
    calidus_key, envelope_for, envelope_of_pool, nonzero_u32, other_key, pool_of, registration,
    seal, test_key, test_now,
};

/// The object the archive holds for `envelope`, signed by `signer`.
fn object_of(signer: &SigningKey, envelope: &Envelope) -> FetchedObject {
    let (wire_bytes, signature) = seal(signer, envelope);
    FetchedObject {
        name: ObjectName {
            pool_id: envelope.pool_id,
            counter: envelope.counter,
            timestamp: envelope.timestamp,
        },
        vkey: signer.verifying_key(),
        signature,
        metadata_schema_version: envelope.schema_version(),
        metadata_counter: envelope.counter,
        wire_bytes,
    }
}

fn stored_object() -> FetchedObject {
    let key = test_key();
    object_of(&key, &envelope_for(&key, 7))
}

fn verified(object: &FetchedObject) -> Result<Envelope, VerifyError> {
    verify(object, TEST_LIMITS, &mut ColdKey, test_now())
}

/// `TEST_LIMITS` with a decompression ceiling no payload fits under, for the
/// tests about the ceiling itself.
fn tight_limits(max_decompressed_bytes: u64) -> Limits {
    Limits {
        max_decompressed_bytes,
        ..TEST_LIMITS
    }
}

#[test]
fn an_object_as_stored_verifies_to_the_envelope_it_holds() {
    let envelope = verified(&stored_object()).unwrap();
    assert_eq!(envelope.counter, 7);
    assert_eq!(envelope.pool_id, pool_of(&test_key()));
    assert_eq!(envelope.timestamp, test_now());
}

/// The check that makes the archive an authenticated record rather than a pile
/// of blobs: the key it is filed under must be the key that signed it (the
/// cold-key path of ADR 0003).
#[test]
fn an_object_signed_by_another_key_does_not_verify() {
    let stranger = other_key();
    let mut object = object_of(&stranger, &envelope_for(&stranger, 7));
    // Filed under the pool it claims to be, signed by someone else.
    object.name.pool_id = pool_of(&test_key());
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::UnauthorizedKey { .. }),
        "got: {error}"
    );
}

#[test]
fn bytes_changed_after_storing_do_not_verify() {
    let mut object = stored_object();
    *object.wire_bytes.last_mut().unwrap() ^= 0xff;
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::BadSignature { .. }),
        "got: {error}"
    );
}

/// Verification order, as ADR 0002 fixes it for the ingest path: bytes nobody
/// signed are refused before anything tries to decompress them.
#[test]
fn bytes_that_were_never_signed_fail_before_decompression() {
    let mut object = stored_object();
    object.wire_bytes = b"not zstd at all".to_vec();
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::BadSignature { .. }),
        "got: {error}"
    );
}

#[test]
fn metadata_that_disagrees_with_the_payload_is_the_finding() {
    for (field, break_it) in [
        (
            "counter",
            (|object: &mut FetchedObject| object.metadata_counter += 1) as fn(&mut FetchedObject),
        ),
        ("schema_version", |object| {
            object.metadata_schema_version += 1
        }),
    ] {
        let mut object = stored_object();
        break_it(&mut object);
        let error = verified(&object).unwrap_err();
        assert!(
            matches!(&error, VerifyError::Disagrees { field: named, .. } if *named == field),
            "{field} must be reported as a disagreement, got: {error}"
        );
    }
}

/// An object under a key that is not what its payload would be keyed as is
/// misfiled: the rebuild would seed a counter the payload does not carry.
#[test]
fn an_object_filed_under_the_wrong_key_does_not_verify() {
    let mut object = stored_object();
    object.name.counter += 1;
    object.metadata_counter += 1;
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::Misfiled { .. }),
        "got: {error}"
    );
}

// Reported as its own finding, not as corruption: an auditor reading
// "malformed" would chase the object rather than the ceiling.
#[test]
fn a_payload_over_the_ceiling_does_not_verify() {
    let object = stored_object();
    let error = verify(&object, tight_limits(1), &mut ColdKey, test_now()).unwrap_err();
    assert!(
        matches!(error, VerifyError::OversizedPayload { max: 1, .. }),
        "got: {error}"
    );
}

/// The two failure kinds are counted apart, because the exit code an operator
/// reads has to distinguish a corpus with a bad object in it from a corpus
/// nobody could read.
#[test]
fn an_audit_counts_unreadable_and_failed_objects_apart() {
    let found = Audit {
        verified: 1,
        failures: vec![
            AuditFailure::Unreadable {
                key: stored_object().name.to_key(),
                reason: "the endpoint answered 404".to_string(),
            },
            AuditFailure::Failed(
                verified(&{
                    let mut object = stored_object();
                    *object.wire_bytes.last_mut().unwrap() ^= 0xff;
                    object
                })
                .unwrap_err(),
            ),
        ],
    };
    assert_eq!(found.unreadable(), 1);
    assert_eq!(found.failed(), 1);
}

/// Every failure names the object, because an audit reports over a bucket and
/// a finding without a key is not actionable.
#[test]
fn every_failure_names_the_object() {
    let key = stored_object().name.to_key();
    let mut tampered = stored_object();
    *tampered.wire_bytes.last_mut().unwrap() ^= 0xff;
    for error in [
        verified(&tampered).unwrap_err(),
        verify(&stored_object(), tight_limits(1), &mut ColdKey, test_now()).unwrap_err(),
    ] {
        assert!(error.to_string().contains(&key), "got: {error}");
    }
}

// Acceptance: the Calidus half of ADR 0003 reaches the archive too.
#[test]
fn an_object_signed_by_the_pools_calidus_key_verifies() {
    let hot = calidus_key();
    let envelope = envelope_of_pool(pool_of(&test_key()), 7);
    let object = object_of(&hot, &envelope);
    let directory = CannedDirectory::holding(envelope.pool_id, vec![registration("nonce-1-key-a")]);

    let verified = verify(
        &object,
        TEST_LIMITS,
        &mut calidus_authority(directory),
        test_now(),
    )
    .unwrap();

    assert_eq!(verified.counter, 7);
}

// A directory that cannot answer is reported apart from a finding about the
// object, which is what lets `audit` stop on it.
#[test]
fn an_object_no_directory_can_decide_on_is_not_a_finding() {
    let hot = calidus_key();
    let envelope = envelope_of_pool(pool_of(&test_key()), 7);
    let object = object_of(&hot, &envelope);
    let mut authority = ColdKeyOrCalidus::new(CalidusKeys::new(
        UnavailableDirectory {
            reason: "db-sync is down",
        },
        nonzero_u32(TEST_TTL_SECS),
    ));
    let error = verify(&object, TEST_LIMITS, &mut authority, test_now()).unwrap_err();
    assert!(matches!(error, VerifyError::Undecided(_)), "got: {error}");
}
