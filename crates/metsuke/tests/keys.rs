//! Signing-key resolution tests (ticket metsuke-4zo.5): TextEnvelope file
//! in, `SigningKey` out; flag beats config path; absent everywhere is a
//! loud startup failure.

use metsuke::keys;

mod support;
use support::test_key;

/// A cardano-cli TextEnvelope for the suite's seed: cborHex is CBOR bytes(32),
/// the `5820` major-type prefix, followed by the seed.
fn test_key_envelope() -> String {
    let seed = metsuke_wire::hex::encode(&test_key().to_bytes());
    format!(
        r#"{{
            "type": "StakePoolSigningKey_ed25519",
            "description": "Stake Pool Operator Signing Key",
            "cborHex": "5820{seed}"
        }}"#
    )
}

/// The same seed under the type cardano-cli writes for a `node key-gen-BLS`
/// key. The prefix and the 32 bytes are the cold key file's, so `type` is the
/// only difference between the two.
fn leios_key_envelope() -> String {
    let seed = metsuke_wire::hex::encode(&test_key().to_bytes());
    format!(
        r#"{{
            "type": "BlsSigningKey_bls12-381-BLS-Signature-Mininimal-Signature-Size",
            "description": "BLS12-381 signing key",
            "cborHex": "5820{seed}"
        }}"#
    )
}

/// A TextEnvelope whose seed is `byte` repeated 32 times, written to `name`
/// under `dir`.
fn write_envelope(dir: &tempfile::TempDir, name: &str, byte: u8) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let envelope = format!(
        r#"{{"type": "StakePoolSigningKey_ed25519", "description": "", "cborHex": "5820{}"}}"#,
        format!("{byte:02x}").repeat(32)
    );
    std::fs::write(&path, envelope).unwrap();
    path
}

// systemd LoadCredential hands the key via the flag; it must beat the
// config path when both are present.
#[test]
fn flag_path_beats_config_path() {
    let dir = tempfile::tempdir().unwrap();
    let flag = write_envelope(&dir, "flag.skey", 1);
    let config = write_envelope(&dir, "config.skey", 2);
    let key = keys::resolve_signing_key(Some(&flag), Some(&config)).unwrap();
    let expected = metsuke_wire::envelope::SigningKey::from_bytes(&[1u8; 32]);
    assert_eq!(
        key.public_key_hex(),
        metsuke_wire::hex::encode(expected.verifying_key().as_bytes())
    );
}

#[test]
fn config_path_used_when_no_flag() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_envelope(&dir, "config.skey", 2);
    let key = keys::resolve_signing_key(None, Some(&config)).unwrap();
    let expected = metsuke_wire::envelope::SigningKey::from_bytes(&[2u8; 32]);
    assert_eq!(
        key.public_key_hex(),
        metsuke_wire::hex::encode(expected.verifying_key().as_bytes())
    );
}

// Acceptance: missing key at both flag and config → startup fails loudly,
// naming both places the operator can provide one.
#[test]
fn missing_at_flag_and_config_fails_loudly() {
    let err = keys::resolve_signing_key(None, None).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("--signing-key") && message.contains("signing_key"),
        "error must name both sources, got: {message}"
    );
}

/// A TextEnvelope carrying `cbor_hex` verbatim.
fn write_cbor_hex(dir: &tempfile::TempDir, name: &str, cbor_hex: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let envelope = format!(
        r#"{{"type": "StakePoolSigningKey_ed25519", "description": "", "cborHex": "{cbor_hex}"}}"#
    );
    std::fs::write(&path, envelope).unwrap();
    path
}

#[test]
fn cbor_hex_without_the_seed_prefix_names_the_prefix_it_wanted() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_cbor_hex(
        &dir,
        "wrong-prefix.skey",
        &format!("5840{}", "07".repeat(32)),
    );
    let message = keys::load_signing_key(&path).unwrap_err().to_string();
    assert!(message.contains("5820"), "got: {message}");
}

// The seed's own length and digits are what `hex::decode` checks, so the
// failure must arrive with its detail rather than as a bare refusal.
#[test]
fn a_seed_of_the_wrong_length_reports_the_byte_count_it_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_cbor_hex(&dir, "short.skey", &format!("5820{}", "07".repeat(31)));
    let message = keys::load_signing_key(&path).unwrap_err().to_string();
    assert!(
        message.contains("31 bytes") && message.contains("expected 32"),
        "got: {message}"
    );
}

#[test]
fn a_seed_that_is_not_hex_reports_that() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_cbor_hex(&dir, "not-hex.skey", &format!("5820{}", "zz".repeat(32)));
    let message = keys::load_signing_key(&path).unwrap_err().to_string();
    assert!(message.contains("hex digits"), "got: {message}");
}

#[test]
fn text_envelope_file_loads_the_seed_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.skey");
    std::fs::write(&path, test_key_envelope()).unwrap();
    let key = keys::load_signing_key(&path).unwrap();
    assert_eq!(
        key.public_key_hex(),
        metsuke_wire::hex::encode(test_key().verifying_key().as_bytes())
    );
}

/// The two key files are the same shape: a `5820` CBOR prefix over 32 bytes.
/// Only the envelope's `type` tells them apart, so it is what the loader reads
/// and what the wrong file is refused by (ADR 0011).
#[test]
fn a_bls_envelope_loads_as_a_leios_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bls.skey");
    std::fs::write(&path, leios_key_envelope()).unwrap();

    let key = keys::load_signing_key(&path).unwrap();

    assert!(
        key.attributes().is_none(),
        "a Leios key names no pool: {key:?}"
    );
    assert_eq!(key.public_key_hex().len(), 192, "96 bytes of hex");
}

#[test]
fn a_key_whose_type_names_neither_scheme_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vrf.skey");
    std::fs::write(
        &path,
        format!(
            r#"{{"type": "VrfSigningKey_PraosVRF", "description": "", "cborHex": "5820{}"}}"#,
            "07".repeat(32)
        ),
    )
    .unwrap();

    let message = keys::load_signing_key(&path).unwrap_err().to_string();

    assert!(
        message.contains("VrfSigningKey_PraosVRF") && message.contains("BlsSigningKey"),
        "got: {message}"
    );
}

/// A cold key's seed under a Leios key's type loads as a Leios key, and one
/// that speaks for nobody. What stops it is the roster, not the file, so the
/// two must not be confusable by their bytes alone.
#[test]
fn the_seed_alone_does_not_say_which_scheme_a_file_is() {
    let dir = tempfile::tempdir().unwrap();
    let as_cold = dir.path().join("cold.skey");
    let as_leios = dir.path().join("bls.skey");
    std::fs::write(&as_cold, test_key_envelope()).unwrap();
    std::fs::write(&as_leios, leios_key_envelope()).unwrap();

    let cold = keys::load_signing_key(&as_cold).unwrap();
    let leios = keys::load_signing_key(&as_leios).unwrap();

    assert_ne!(cold.public_key_hex(), leios.public_key_hex());
    assert_ne!(
        cold.attest(b"body").key_bytes(),
        leios.attest(b"body").key_bytes(),
        "the same seed, and two keys"
    );
}
