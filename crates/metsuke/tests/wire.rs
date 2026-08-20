//! Tier-1 property tests for the wire contract: seal→open roundtrip and
//! tamper rejection (ticket metsuke-4zo.1).

use metsuke::envelope::{self, Envelope, PoolId, SCHEMA_VERSION, Sample, SigningKey};
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
        .prop_map(
            |(pool_id, agent_version, counter, timestamp, samples)| Envelope {
                schema_version: SCHEMA_VERSION,
                pool_id,
                agent_version,
                counter,
                timestamp,
                samples,
            },
        )
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
        "schema_version": SCHEMA_VERSION,
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
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: PoolId::from_cold_key(&SigningKey::from_bytes(&[7u8; 32]).verifying_key()),
        agent_version: "0.1.0".into(),
        counter: 1,
        timestamp: OffsetDateTime::UNIX_EPOCH,
        samples: vec![],
    };
    let (bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, 8).unwrap_err();
    assert!(matches!(
        err,
        envelope::OpenError::TooLarge {
            max_decompressed_bytes: 8
        }
    ));
}

#[test]
fn seal_rejects_non_finite_sync_progress() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: PoolId::from_cold_key(&SigningKey::from_bytes(&[7u8; 32]).verifying_key()),
        agent_version: "0.1.0".into(),
        counter: 1,
        timestamp: OffsetDateTime::UNIX_EPOCH,
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
    };
    let err = envelope::seal(&key, &env, 0).unwrap_err();
    assert!(matches!(
        err,
        envelope::SealError::NonFiniteSyncProgress { index: 0, .. }
    ));
}
