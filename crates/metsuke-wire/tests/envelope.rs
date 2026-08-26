//! Tier-1 property tests for the wire contract: seal→open roundtrip and
//! tamper rejection (ticket metsuke-4zo.1).

use metsuke_wire::envelope::{
    self, Envelope, Payload, PoolId, SCHEMA_VERSION_LINES, SCHEMA_VERSION_SAMPLES, Sample,
    SigningKey,
};
use proptest::prelude::*;
use time::OffsetDateTime;

// Large enough for any generated envelope; the real limit is server config.
const TEST_DECOMPRESS_LIMIT: u64 = 64 * 1024 * 1024;

fn arb_timestamp() -> impl Strategy<Value = OffsetDateTime> {
    // 1970..2100, with sub-second precision RFC 3339 must preserve.
    (0i64..4_102_444_800, 0u32..1_000_000_000).prop_map(|(secs, nanos)| {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
            + time::Duration::nanoseconds(nanos as i64)
    })
}

fn arb_sample() -> impl Strategy<Value = Sample> {
    (
        arb_timestamp(),
        any::<Option<u64>>(),
        any::<Option<u64>>(),
        any::<Option<u64>>(),
        any::<Option<u64>>(),
        proptest::option::of(-1.0e12f64..1.0e12),
        any::<Option<String>>(),
        any::<Option<String>>(),
        any::<Option<i64>>(),
    )
        .prop_map(
            |(
                sampled_at,
                block_height,
                slot,
                slot_in_epoch,
                epoch,
                sync_progress,
                node_version,
                node_revision,
                clock_offset_ms,
            )| Sample {
                sampled_at,
                block_height,
                slot,
                slot_in_epoch,
                epoch,
                sync_progress,
                node_version,
                node_revision,
                clock_offset_ms,
            },
        )
}

fn arb_pool_id() -> impl Strategy<Value = PoolId> {
    any::<[u8; 32]>()
        .prop_map(|seed| PoolId::from_cold_key(&SigningKey::from_bytes(&seed).verifying_key()))
}

fn arb_envelope() -> impl Strategy<Value = Envelope> {
    (
        arb_pool_id(),
        any::<String>(),
        any::<u64>(),
        arb_timestamp(),
        proptest::collection::vec(arb_sample(), 0..8),
    )
        .prop_map(|(pool_id, agent_version, counter, timestamp, samples)| {
            Envelope::new(
                pool_id,
                agent_version,
                counter,
                timestamp,
                Payload::Samples { samples },
            )
        })
}

fn arb_lines_envelope() -> impl Strategy<Value = Envelope> {
    (
        arb_pool_id(),
        any::<String>(),
        any::<u64>(),
        arb_timestamp(),
        // No newline: that is the framing byte, and `seal` refuses a line
        // holding one (`a_line_holding_a_newline_is_refused`).
        proptest::collection::vec("[^\n]*", 0..8),
    )
        .prop_map(|(pool_id, agent_version, counter, timestamp, lines)| {
            Envelope::new(
                pool_id,
                agent_version,
                counter,
                timestamp,
                Payload::Lines { lines },
            )
        })
}

proptest! {
    #[test]
    fn seal_open_roundtrip(env in arb_envelope(), seed in any::<[u8; 32]>(), level in 0i32..=5) {
        let key = SigningKey::from_bytes(&seed);
        let (bytes, sig) = envelope::seal(&key, &env, level).unwrap();
        let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap();
        prop_assert_eq!(env, opened);
    }

    #[test]
    fn tampered_byte_rejects(
        env in arb_envelope(),
        seed in any::<[u8; 32]>(),
        index in any::<prop::sample::Index>(),
        mask in 1u8..=255,
    ) {
        let key = SigningKey::from_bytes(&seed);
        let (mut bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
        let i = index.index(bytes.len());
        bytes[i] ^= mask;
        let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap_err();
        prop_assert!(matches!(err, envelope::OpenError::Signature(_)));
    }

    // The v2 payload rides the same seal/open pair: the lines come back in
    // order and byte for byte, however many of them there are.
    #[test]
    fn lines_seal_open_roundtrip(
        env in arb_lines_envelope(),
        seed in any::<[u8; 32]>(),
        level in 0i32..=5,
    ) {
        let key = SigningKey::from_bytes(&seed);
        let (bytes, sig) = envelope::seal(&key, &env, level).unwrap();
        let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap();
        prop_assert_eq!(env, opened);
    }

    #[test]
    fn pool_id_bech32_roundtrip(pool_id in arb_pool_id()) {
        prop_assert_eq!(PoolId::from_bech32(&pool_id.to_bech32()).unwrap(), pool_id);
    }
}

// A syntactically valid, correctly signed body whose pool_id is not bech32
// must reject at open, not surface later in verification lookups.
#[test]
fn open_rejects_malformed_pool_id() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let json = serde_json::json!({
        "schema_version": SCHEMA_VERSION_SAMPLES,
        "pool_id": "pool1notvalidbech32",
        "agent_version": "0.1.0",
        "counter": 1,
        "timestamp": "1970-01-01T00:00:00Z",
        "samples": [],
    })
    .to_string();
    let bytes = zstd::encode_all(json.as_bytes(), 0).unwrap();
    use ed25519_dalek::Signer;
    let sig = key.sign(&bytes);
    let err =
        envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap_err();
    assert!(matches!(err, envelope::OpenError::Json(_)));
}

#[test]
fn pool_id_rejects_wrong_prefix() {
    let hrp = bech32::Hrp::parse("addr").unwrap();
    let addr = bech32::encode::<bech32::Bech32>(hrp, &[7u8; 28]).unwrap();
    assert!(matches!(
        PoolId::from_bech32(&addr),
        Err(envelope::PoolIdError::WrongHrp { .. })
    ));
}

#[test]
fn pool_id_rejects_wrong_length() {
    let hrp = bech32::Hrp::parse("pool").unwrap();
    let short = bech32::encode::<bech32::Bech32>(hrp, &[7u8; 20]).unwrap();
    assert!(matches!(
        PoolId::from_bech32(&short),
        Err(envelope::PoolIdError::WrongLength { found: 20 })
    ));
}

#[test]
fn open_rejects_oversized_payload() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = empty_samples_envelope(&key);
    let (bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, 8).unwrap_err();
    assert!(matches!(
        err,
        envelope::OpenError::TooLarge {
            max_decompressed_bytes: 8
        }
    ));
}

// The ceiling is a cap, not a reservation: a small payload under a huge
// limit must cost what the payload costs, not what the limit allows.
#[test]
fn open_under_a_huge_limit_costs_only_the_payload() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = empty_samples_envelope(&key);
    let (bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
    let opened =
        envelope::open(&key.verifying_key(), &bytes, &sig, 64 * 1024 * 1024 * 1024).unwrap();
    assert_eq!(opened, env);
}

#[test]
fn seal_rejects_non_finite_sync_progress() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        Payload::Samples {
            samples: vec![Sample {
                sampled_at: OffsetDateTime::UNIX_EPOCH,
                block_height: None,
                slot: None,
                slot_in_epoch: None,
                epoch: None,
                sync_progress: Some(f64::NAN),
                node_version: None,
                node_revision: None,
                clock_offset_ms: None,
            }],
        },
    );
    let err = envelope::seal(&key, &env, 0).unwrap_err();
    assert!(matches!(
        err,
        envelope::SealError::NonFiniteSyncProgress { index: 0, .. }
    ));
}

/// A hand-built body signed with `key`, as a client that speaks a different
/// schema than this build would send one.
fn sealed_json(key: &SigningKey, json: serde_json::Value) -> (Vec<u8>, envelope::Signature) {
    sealed_body(key, &json.to_string())
}

/// The same for a body that is not one JSON object, which is the only way to
/// state a framing no `Envelope` can be built into.
fn sealed_body(key: &SigningKey, body: &str) -> (Vec<u8>, envelope::Signature) {
    use ed25519_dalek::Signer;
    let bytes = zstd::encode_all(body.as_bytes(), 0).unwrap();
    let signature = key.sign(&bytes);
    (bytes, signature)
}

// The declared version and the body's own shape are two statements about one
// envelope; when they disagree, neither is what the sender meant.
#[test]
fn open_rejects_a_version_that_contradicts_the_body() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = sealed_json(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES,
            "pool_id": PoolId::from_cold_key(&key.verifying_key()).to_bech32(),
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
            "samples": [],
        }),
    );
    let err =
        envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap_err();
    assert!(
        matches!(
            err,
            envelope::OpenError::BodyContradictsVersion {
                declared: SCHEMA_VERSION_LINES,
                ..
            }
        ),
        "expected the cross-check to name the declared version, got: {err}"
    );
}

// The other half of the same cross-check: v1 keeps its samples in the header
// line and writes nothing after it, so anything after it is not a v1 body.
#[test]
fn open_rejects_lines_appended_to_a_v1_header() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let header = serde_json::json!({
        "schema_version": SCHEMA_VERSION_SAMPLES,
        "pool_id": PoolId::from_cold_key(&key.verifying_key()).to_bech32(),
        "agent_version": "0.1.0",
        "counter": 1,
        "timestamp": "1970-01-01T00:00:00Z",
        "samples": [],
    });
    let (bytes, sig) = sealed_body(&key, &format!("{header}\nsmuggled"));
    let err =
        envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap_err();
    assert!(
        matches!(
            err,
            envelope::OpenError::BodyContradictsVersion {
                declared: SCHEMA_VERSION_SAMPLES,
                ..
            }
        ),
        "expected the cross-check to reject the appended line, got: {err}"
    );
}

// A version this build has never heard of is named as such, not reported as a
// payload that matched no shape.
#[test]
fn open_names_an_unsupported_schema_version() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = sealed_json(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES + 1,
            "pool_id": PoolId::from_cold_key(&key.verifying_key()).to_bech32(),
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
            "spans": [],
        }),
    );
    let err =
        envelope::open(&key.verifying_key(), &bytes, &sig, TEST_DECOMPRESS_LIMIT).unwrap_err();
    assert!(
        matches!(err, envelope::OpenError::UnsupportedSchemaVersion { found } if found == SCHEMA_VERSION_LINES + 1),
        "expected the version to be named, got: {err}"
    );
}

/// The bytes a v1-only build sealed, and what it put in them. Provenance:
/// tests/fixtures/recordings/README.md.
const RECORDED_V1: &str = include_str!("fixtures/recordings/v1-envelope.hex");

// ADR 0005 says an archived object stays independently verifiable, and every
// deployed agent still ships v1. Both hold only if this build opens bytes it
// did not produce.
#[test]
fn a_recorded_v1_envelope_still_opens() {
    use ed25519_dalek::Signer;
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let bytes = metsuke_wire::hex::decode_bytes(RECORDED_V1.trim()).unwrap();
    let signature = key.sign(&bytes);
    let opened = envelope::open(
        &key.verifying_key(),
        &bytes,
        &signature,
        TEST_DECOMPRESS_LIMIT,
    )
    .unwrap();

    assert_eq!(opened.schema_version(), SCHEMA_VERSION_SAMPLES);
    assert_eq!(opened.counter, 42);
    let Payload::Samples { samples } = opened.payload() else {
        panic!("a v1 recording carries samples, got {:?}", opened.payload());
    };
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].block_height, Some(12_318_442));
    assert_eq!(samples[0].node_version.as_deref(), Some("11.0.1"));
    assert_eq!(samples[0].clock_offset_ms, Some(-3));
}

// Opening it is half the freeze; this is the other half. Every deployed agent
// still seals v1, so a framing change that only stays readable would split the
// archive into bytes two builds produced for one envelope. Re-sealing what the
// recording holds restates none of its values: the recorder owns them.
#[test]
fn this_build_seals_a_v1_envelope_to_the_recorded_bytes() {
    use ed25519_dalek::Signer;
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let recorded = metsuke_wire::hex::decode_bytes(RECORDED_V1.trim()).unwrap();
    let opened = envelope::open(
        &key.verifying_key(),
        &recorded,
        &key.sign(&recorded),
        TEST_DECOMPRESS_LIMIT,
    )
    .unwrap();

    // Level 0, as scripts/record-v1-envelope.sh sealed it.
    let (resealed, _) = envelope::seal(&key, &opened, 0).unwrap();

    assert_eq!(metsuke_wire::hex::encode(&resealed), RECORDED_V1.trim());
}

// A newline inside a line is the one thing the framing cannot carry: it would
// open as two lines under a signature that verifies, so the caller hears about
// it rather than the archive holding a line the node never wrote.
#[test]
fn a_line_holding_a_newline_is_refused() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let envelope = Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        Payload::Lines {
            lines: vec!["first".into(), "second\nthird".into()],
        },
    );
    let err = envelope::seal(&key, &envelope, 0).unwrap_err();
    assert!(
        matches!(err, envelope::SealError::LineHoldsNewline { index: 1 }),
        "expected the offending line to be named, got: {err}"
    );
}

/// The smallest envelope of each shape, for the tests that care about the
/// wrapper rather than what it holds.
fn empty_samples_envelope(key: &SigningKey) -> Envelope {
    Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        Payload::Samples { samples: vec![] },
    )
}
