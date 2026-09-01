//! The Leios key's signing, and what keeps a submission's signature out of the
//! scheme the node votes with (ADR 0011).

use metsuke_wire::envelope::{
    Attestation, HEADER_SIGNATURE, HEADER_VKEY, SigningKey, SubmissionKey,
};
use metsuke_wire::hex;
use metsuke_wire::leios::{DST, LeiosPublicKey, LeiosSignature, LeiosSigningKey};

const BODY: &[u8] = b"the sealed bytes, as sent";

fn leios_key(seed: u8) -> LeiosSigningKey {
    LeiosSigningKey::from_bytes(&[seed; 32]).expect("a fixed seed is a scalar")
}

#[test]
fn a_signature_stands_under_the_key_that_made_it() {
    let key = leios_key(1);

    let signature = key.sign(BODY);

    assert!(signature.verifies(BODY, &key.public_key()));
}

#[test]
fn a_signature_does_not_stand_under_another_key() {
    let signature = leios_key(1).sign(BODY);

    assert!(!signature.verifies(BODY, &leios_key(2).public_key()));
}

#[test]
fn a_signature_does_not_stand_over_other_bytes() {
    let key = leios_key(1);

    let signature = key.sign(BODY);

    assert!(!signature.verifies(b"one byte more", &key.public_key()));
}

/// The whole reason `DST` is ours: the same key signing the same bytes in the
/// consensus ciphersuite produces a signature this build does not accept, and
/// the one it does make is not a signature there. A vote and a submission
/// cannot be each other however the bytes line up.
#[test]
fn a_signature_in_the_consensus_ciphersuite_does_not_stand_here() {
    const CONSENSUS_DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_";
    assert_ne!(DST, CONSENSUS_DST, "our tag is not the consensus one");
    let secret = blst::min_sig::SecretKey::from_bytes(&[1u8; 32]).unwrap();

    let elsewhere = secret.sign(BODY, CONSENSUS_DST, &[]);

    let here = leios_key(1);
    let signature = LeiosSignature::from_bytes(&elsewhere.to_bytes()).expect("a well-formed point");
    assert!(!signature.verifies(BODY, &here.public_key()));
    assert_ne!(here.sign(BODY).to_bytes(), elsewhere.to_bytes());
}

/// Holding a `LeiosPublicKey` means the point was checked, so a roster entry
/// nobody could verify against is refused where it is read rather than where
/// it is used.
#[test]
fn a_verification_key_off_the_curve_is_refused() {
    let mut bytes = leios_key(1).public_key().to_bytes();
    bytes[40] ^= 0xff;

    let refused = LeiosPublicKey::from_bytes(&bytes);

    assert!(refused.is_err(), "a mangled encoding is not a key");
}

#[test]
fn the_infinity_verification_key_is_refused() {
    let mut infinity = [0u8; 96];
    infinity[0] = 0xc0;

    let refused = LeiosPublicKey::from_bytes(&infinity);

    assert!(refused.is_err(), "infinity is not a key");
}

#[test]
fn the_infinity_signature_is_refused() {
    let mut infinity = [0u8; 48];
    infinity[0] = 0xc0;

    let refused = LeiosSignature::from_bytes(&infinity);

    assert!(refused.is_err(), "infinity is not a signature");
}

/// Which scheme signed is the length of the pair, so the two decode apart with
/// no header saying which (`Attestation::decode`).
#[test]
fn the_pair_decodes_as_the_scheme_its_lengths_name() {
    let leios = SubmissionKey::LeiosKey(leios_key(1)).attest(BODY);
    let cold = SubmissionKey::ColdKey(SigningKey::from_bytes(&[7u8; 32])).attest(BODY);

    for attestation in [leios.clone(), cold.clone()] {
        let [(vkey_header, vkey), (signature_header, signature)] = attestation.headers();
        assert_eq!(
            (vkey_header, signature_header),
            (HEADER_VKEY, HEADER_SIGNATURE)
        );

        let decoded = Attestation::decode(Some(&vkey), Some(&signature)).unwrap();

        assert_eq!(decoded, attestation);
        assert!(decoded.verifies(BODY));
    }
    assert_eq!(hex::decode::<96>(&leios.headers()[0].1).unwrap().len(), 96);
    assert_eq!(hex::decode::<32>(&cold.headers()[0].1).unwrap().len(), 32);
}

/// A cold key names its pool; a Leios key names none, and the absence is a
/// value the server has to answer with a roster rather than a default.
#[test]
fn only_a_cold_key_attributes_a_pool() {
    let leios = SubmissionKey::LeiosKey(leios_key(1));
    let cold = SubmissionKey::ColdKey(SigningKey::from_bytes(&[7u8; 32]));

    assert!(leios.attributes().is_none());
    assert!(cold.attributes().is_some());
    assert_eq!(cold.attest(BODY).attributes(), cold.attributes());
    assert!(leios.attest(BODY).attributes().is_none());
}

/// A signing key that renders itself into a log line is a signing key in the
/// log, and both schemes go through the same rendering.
#[test]
fn a_signing_key_debugs_as_its_public_half() {
    let key = SubmissionKey::LeiosKey(leios_key(1));

    let shown = format!("{key:?}");

    assert!(shown.contains(&key.public_key_hex()), "got: {shown}");
    assert!(
        !shown.contains(&hex::encode(&[1u8; 32])),
        "the secret is in the rendering: {shown}"
    );
}
