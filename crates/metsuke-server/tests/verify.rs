//! Re-verifying a stored object. Each test builds the object the archive would
//! hand back and breaks exactly one thing about it, so what the verification
//! rests on is visible one failure at a time.

use metsuke_server::archive::{FetchedObject, Kind};
use metsuke_server::verify::{Audit, AuditFailure, VerifyError, verify};
use metsuke_wire::envelope::{Envelope, Header, SigningKey};

mod support;
use support::{
    MAX_HEADER_BYTES, envelope_for, object_name, other_key, pool_of, seal, test_agent_id, test_key,
    test_now,
};

/// The object the archive holds for `envelope`, signed by `signer` and filed
/// under `signer`'s pool.
fn object_of(signer: &SigningKey, envelope: &Envelope) -> FetchedObject {
    let (wire_bytes, signature) = seal(signer, envelope);
    FetchedObject {
        name: object_name(signer, test_now(), Kind::Metrics),
        vkey: signer.verifying_key(),
        signature,
        wire_bytes,
    }
}

fn stored_object() -> FetchedObject {
    let key = test_key();
    object_of(&key, &envelope_for(&key, 7))
}

fn verified(object: &FetchedObject) -> Result<Header, VerifyError> {
    verify(object, MAX_HEADER_BYTES)
}

#[test]
fn an_object_as_stored_verifies_to_the_header_it_holds() {
    let header = verified(&stored_object()).unwrap();
    assert_eq!(header.counter, 7);
    assert_eq!(header.pool_id, pool_of(&test_key()));
    assert_eq!(header.agent_id, test_agent_id());
    assert_eq!(header.timestamp, test_now());
}

/// The check that makes the archive an authenticated record rather than a pile
/// of blobs: the pool it is filed under is the pool the signing key derives to
/// (ADR 0003), so an object another key signed is misfiled wherever it sits.
#[test]
fn an_object_signed_by_another_key_does_not_verify() {
    let stranger = other_key();
    let mut object = object_of(&stranger, &envelope_for(&stranger, 7));
    // Filed under the pool it claims to be, signed by someone else.
    object.name.pool_id = pool_of(&test_key());
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::Misfiled { .. }),
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

/// Verification order: bytes nobody signed are refused before anything reads a
/// header out of them.
#[test]
fn bytes_that_were_never_signed_fail_before_the_header_is_read() {
    let mut object = stored_object();
    object.wire_bytes = b"not a container at all".to_vec();
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::BadSignature { .. }),
        "got: {error}"
    );
}

/// An object whose key says one agent and whose header says another is
/// misfiled: the key is `store`'s output, so re-deriving it is what catches a
/// batch that was filed under something other than what it carries.
#[test]
fn an_object_filed_under_the_wrong_agent_does_not_verify() {
    let mut object = stored_object();
    object.name.agent_id = metsuke_wire::envelope::AgentId::parse("other-relay").unwrap();
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::Misfiled { .. }),
        "got: {error}"
    );
}

/// The same for the kind: a metrics batch filed as logs is not the object its
/// key names.
#[test]
fn an_object_filed_under_the_wrong_kind_does_not_verify() {
    let mut object = stored_object();
    object.name.kind = Kind::Logs;
    let error = verified(&object).unwrap_err();
    assert!(
        matches!(error, VerifyError::Misfiled { .. }),
        "got: {error}"
    );
}

/// A header over the bound is its own finding, not corruption: an auditor
/// reading "does not verify" would chase the object rather than the bound.
#[test]
fn a_header_over_the_bound_does_not_verify() {
    let object = stored_object();
    let error = verify(&object, 1).unwrap_err();
    assert!(
        matches!(error, VerifyError::UnreadableHeader { .. }),
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
    let object = stored_object();
    let key = object.name.to_key();
    let mut tampered = stored_object();
    tampered.name = object.name.clone();
    *tampered.wire_bytes.last_mut().unwrap() ^= 0xff;
    for error in [
        verified(&tampered).unwrap_err(),
        verify(&object, 1).unwrap_err(),
    ] {
        assert!(error.to_string().contains(&key), "got: {error}");
    }
}
