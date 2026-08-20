//! Signing-key resolution tests (ticket metsuke-4zo.5): TextEnvelope file
//! in, `SigningKey` out; flag beats config path; absent everywhere is a
//! loud startup failure.

use metsuke::keys;

/// A cardano-cli TextEnvelope for the all-sevens Ed25519 seed: cborHex is
/// CBOR bytes(32) — the `5820` major-type prefix — followed by the seed.
fn seven_seed_envelope() -> String {
    format!(
        r#"{{
            "type": "StakePoolSigningKey_ed25519",
            "description": "Stake Pool Operator Signing Key",
            "cborHex": "5820{}"
        }}"#,
        "07".repeat(32)
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
    let expected = metsuke::envelope::SigningKey::from_bytes(&[1u8; 32]);
    assert_eq!(key.verifying_key(), expected.verifying_key());
}

#[test]
fn config_path_used_when_no_flag() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_envelope(&dir, "config.skey", 2);
    let key = keys::resolve_signing_key(None, Some(&config)).unwrap();
    let expected = metsuke::envelope::SigningKey::from_bytes(&[2u8; 32]);
    assert_eq!(key.verifying_key(), expected.verifying_key());
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

#[test]
fn text_envelope_file_loads_the_seed_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.skey");
    std::fs::write(&path, seven_seed_envelope()).unwrap();
    let key = keys::load_signing_key(&path).unwrap();
    let expected = metsuke::envelope::SigningKey::from_bytes(&[7u8; 32]);
    assert_eq!(key.verifying_key(), expected.verifying_key());
}
