//! Header decode: the ADR-0001 headers are the only thing standing between
//! an arbitrary internet request and the intake, so every malformed shape
//! must name what is wrong rather than reach `submit`.

use metsuke_server::http::SubmissionHeaders;
use metsuke_wire::envelope::{HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY};
use tiny_http::Header;

mod support;
use metsuke_wire::hex;
use support::{pool_of, seal, test_key};

fn header(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes()).unwrap()
}

/// The three headers a well-formed upload carries.
fn valid_headers() -> Vec<Header> {
    let key = test_key();
    let (_, signature) = seal(&key, &support::envelope_for(&key, 1));
    vec![
        header(HEADER_POOL_ID, &pool_of(&key).to_bech32()),
        header(HEADER_VKEY, &hex::encode(key.verifying_key().as_bytes())),
        header(HEADER_SIGNATURE, &hex::encode(&signature.to_bytes())),
    ]
}

/// Replace one header's value, keeping the other two well formed.
fn with(field: &'static str, value: &str) -> Vec<Header> {
    let mut headers: Vec<Header> = valid_headers()
        .into_iter()
        .filter(|existing| !existing.field.equiv(field))
        .collect();
    headers.push(header(field, value));
    headers
}

fn without(field: &'static str) -> Vec<Header> {
    valid_headers()
        .into_iter()
        .filter(|existing| !existing.field.equiv(field))
        .collect()
}

#[test]
fn valid_headers_decode_to_the_claimed_identity() {
    let key = test_key();
    let decoded = SubmissionHeaders::decode(&valid_headers()).unwrap();
    assert_eq!(decoded.pool_id, pool_of(&key));
    assert_eq!(decoded.vkey, key.verifying_key());
    let (wire_bytes, signature) = seal(&key, &support::envelope_for(&key, 1));
    assert_eq!(decoded.signature, signature);
    // Decoded well enough to verify with: the whole point of the layer.
    metsuke_wire::envelope::open(&decoded.vkey, &wire_bytes, &decoded.signature, 1 << 20).unwrap();
}

#[test]
fn a_missing_header_names_it() {
    for field in [HEADER_POOL_ID, HEADER_VKEY, HEADER_SIGNATURE] {
        let error = SubmissionHeaders::decode(&without(field)).unwrap_err();
        assert!(
            error.to_string().contains(field),
            "{field} missing must be named, got: {error}"
        );
    }
}

#[test]
fn header_names_are_matched_case_insensitively() {
    let key = test_key();
    let headers = vec![
        header(&HEADER_POOL_ID.to_uppercase(), &pool_of(&key).to_bech32()),
        header(
            &HEADER_VKEY.to_uppercase(),
            &hex::encode(key.verifying_key().as_bytes()),
        ),
        header(
            &HEADER_SIGNATURE.to_uppercase(),
            &hex::encode(&seal(&key, &support::envelope_for(&key, 1)).1.to_bytes()),
        ),
    ];
    assert_eq!(
        SubmissionHeaders::decode(&headers).unwrap().pool_id,
        pool_of(&key)
    );
}

#[test]
fn a_key_of_the_wrong_length_is_refused() {
    let error = SubmissionHeaders::decode(&with(HEADER_VKEY, &"ab".repeat(31))).unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains(HEADER_VKEY) && text.contains("31"),
        "got: {text}"
    );
}

#[test]
fn a_signature_of_the_wrong_length_is_refused() {
    let error = SubmissionHeaders::decode(&with(HEADER_SIGNATURE, &"ab".repeat(65))).unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains(HEADER_SIGNATURE) && text.contains("65"),
        "got: {text}"
    );
}

#[test]
fn a_non_hex_key_is_refused() {
    for value in ["zz".repeat(32), "abc".to_string()] {
        let error = SubmissionHeaders::decode(&with(HEADER_VKEY, &value)).unwrap_err();
        assert!(
            error.to_string().contains(HEADER_VKEY),
            "{value:?} must be refused naming the header, got: {error}"
        );
    }
}

#[test]
fn uppercase_hex_decodes() {
    let key = test_key();
    let decoded = SubmissionHeaders::decode(&with(
        HEADER_VKEY,
        &hex::encode(key.verifying_key().as_bytes()).to_uppercase(),
    ))
    .unwrap();
    assert_eq!(decoded.vkey, key.verifying_key());
}

#[test]
fn a_pool_id_that_is_not_bech32_is_refused() {
    let error = SubmissionHeaders::decode(&with(HEADER_POOL_ID, "pool1nope")).unwrap_err();
    assert!(error.to_string().contains(HEADER_POOL_ID), "got: {error}");
}

/// Thirty-two bytes of hex whose y coordinate is on no curve point: the one
/// malformed key shape that survives length and hex checks.
#[test]
fn a_key_that_is_not_a_curve_point_is_refused() {
    let not_a_point = format!("02{}", "00".repeat(31));
    let error = SubmissionHeaders::decode(&with(HEADER_VKEY, &not_a_point)).unwrap_err();
    assert!(error.to_string().contains(HEADER_VKEY), "got: {error}");
}
