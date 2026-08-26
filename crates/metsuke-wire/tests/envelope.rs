//! Tier-1 property tests for the wire contract: seal→open roundtrip, tamper
//! rejection (ticket metsuke-4zo.1) and the container the two frames make
//! (ticket metsuke-jfb.1).

use metsuke_wire::envelope::{
    self, CONTAINER_MAGIC, Envelope, HEADER_OFFSET, Limits, Payload, PoolId, SCHEMA_VERSION_LINES,
    SCHEMA_VERSION_SAMPLES, Sample, SigningKey,
};
use proptest::prelude::*;
use time::OffsetDateTime;

// Wide enough for any generated envelope; the real limits are server config.
const TEST_LIMITS: Limits = Limits {
    max_header_bytes: 4096,
    max_decompressed_bytes: 64 * 1024 * 1024,
};

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

/// Any string, bounded in length alone: the header travels uncompressed under
/// the byte limit `TEST_LIMITS` states, and every other character — quote,
/// control, newline, non-ASCII — is JSON escaping this has to cover.
fn arb_agent_version() -> impl Strategy<Value = String> {
    "(?s).{0,32}"
}

fn arb_envelope() -> impl Strategy<Value = Envelope> {
    (
        arb_pool_id(),
        arb_agent_version(),
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
        arb_agent_version(),
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
        let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap();
        prop_assert_eq!(env, opened);
    }

    // A signature over the whole byte sequence, so a flipped bit in the
    // uncompressed header frame fails the same way one in the data frame does.
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
        let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
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
        let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap();
        prop_assert_eq!(env, opened);
    }

    // The first frame is a skippable frame whatever the envelope holds, and
    // its declared length is the header that follows it.
    #[test]
    fn every_submission_begins_with_a_skippable_frame(
        env in arb_lines_envelope(),
        seed in any::<[u8; 32]>(),
    ) {
        let key = SigningKey::from_bytes(&seed);
        let (bytes, _) = envelope::seal(&key, &env, 0).unwrap();
        let header = envelope::header_json(&env).unwrap();
        prop_assert_eq!(&bytes[..4], CONTAINER_MAGIC.to_le_bytes());
        prop_assert_eq!(&bytes[4..HEADER_OFFSET], (header.len() as u32).to_le_bytes());
        prop_assert_eq!(&bytes[HEADER_OFFSET..HEADER_OFFSET + header.len()], header.as_slice());
    }

    #[test]
    fn pool_id_bech32_roundtrip(pool_id in arb_pool_id()) {
        prop_assert_eq!(PoolId::from_bech32(&pool_id.to_bech32()).unwrap(), pool_id);
    }
}

// Who sent a submission, and under which schema, without a key and without a
// decompressor: seek past the magic and the length, parse what follows.
#[test]
fn the_header_reads_back_without_a_decompressor() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = empty_samples_envelope(&key);
    let (bytes, _) = envelope::seal(&key, &env, 0).unwrap();

    let frames = envelope::split(&bytes, TEST_LIMITS.max_header_bytes).unwrap();
    let header: serde_json::Value = serde_json::from_slice(frames.header).unwrap();

    assert_eq!(header["schema_version"], SCHEMA_VERSION_SAMPLES);
    assert_eq!(header["pool_id"], env.pool_id.to_bech32());
    assert_eq!(header["counter"], env.counter);
}

#[test]
fn a_body_without_the_magic_is_not_a_container() {
    let plain = zstd::encode_all(&b"{}\n"[..], 0).unwrap();
    assert!(matches!(
        envelope::split(&plain, 4096),
        Err(envelope::ContainerError::NotAContainer)
    ));
}

// Shorter than the prefix it would have to begin with, so there is nothing to
// read a length out of.
#[test]
fn a_body_shorter_than_the_frame_prefix_is_not_a_container() {
    assert!(matches!(
        envelope::split(&CONTAINER_MAGIC.to_le_bytes(), 4096),
        Err(envelope::ContainerError::NotAContainer)
    ));
}

// The declared length is refused before any of it is read, so a body claiming
// a gigabyte header costs the bound rather than the claim.
#[test]
fn a_header_over_the_bound_is_refused() {
    let container = container(&[], 1 << 30, &[]);
    assert!(matches!(
        envelope::split(&container, 4096),
        Err(envelope::ContainerError::OversizedHeader {
            declared: 1_073_741_824,
            max: 4096
        })
    ));
}

#[test]
fn a_header_longer_than_the_body_is_refused() {
    let container = container(b"{}", 64, &[]);
    assert!(matches!(
        envelope::split(&container, 4096),
        Err(envelope::ContainerError::ShortHeader {
            declared: 64,
            found: 2
        })
    ));
}

#[test]
fn open_refuses_a_body_that_is_not_a_container() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let plain = zstd::encode_all(&b"{}\n"[..], 0).unwrap();
    use ed25519_dalek::Signer;
    let err =
        envelope::open(&key.verifying_key(), &plain, &key.sign(&plain), TEST_LIMITS).unwrap_err();
    assert!(matches!(
        err,
        envelope::OpenError::Container(envelope::ContainerError::NotAContainer)
    ));
}

// A syntactically valid, correctly signed submission whose pool_id is not
// bech32 must reject at open, not surface later in verification lookups.
#[test]
fn open_rejects_malformed_pool_id() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = sealed_header(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_SAMPLES,
            "pool_id": "pool1notvalidbech32",
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
        }),
        b"",
    );
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
    assert!(matches!(err, envelope::OpenError::Json(_)));
}

// A version this build has never heard of is named as such, not reported as a
// payload that matched no shape.
#[test]
fn open_names_an_unsupported_schema_version() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = sealed_header(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES + 1,
            "pool_id": PoolId::from_cold_key(&key.verifying_key()).to_bech32(),
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
        }),
        b"",
    );
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
    assert!(
        matches!(err, envelope::OpenError::UnsupportedSchemaVersion { found } if found == SCHEMA_VERSION_LINES + 1),
        "expected the version to be named, got: {err}"
    );
}

// Every line ends with its newline, so trailing bytes without one are a line
// that was cut off rather than a line the sender wrote.
#[test]
fn open_rejects_an_unterminated_last_line() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = sealed_header(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES,
            "pool_id": PoolId::from_cold_key(&key.verifying_key()).to_bech32(),
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
        }),
        b"whole\ncut off",
    );
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
    assert!(matches!(err, envelope::OpenError::UnterminatedLine));
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
    let env = one_sample_envelope(&key);
    let (bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
    let limits = Limits {
        max_decompressed_bytes: 8,
        ..TEST_LIMITS
    };
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, limits).unwrap_err();
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
    let limits = Limits {
        max_decompressed_bytes: 64 * 1024 * 1024 * 1024,
        ..TEST_LIMITS
    };
    let opened = envelope::open(&key.verifying_key(), &bytes, &sig, limits).unwrap();
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
                sync_progress: Some(f64::NAN),
                ..sample()
            }],
        },
    );
    let err = envelope::seal(&key, &env, 0).unwrap_err();
    assert!(matches!(
        err,
        envelope::SealError::NonFiniteSyncProgress { index: 0, .. }
    ));
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

/// The bytes this build's sealing path produced for each payload shape,
/// lowercase hex on one line. Provenance: tests/fixtures/recordings/README.md.
const RECORDINGS: [&str; 2] = [
    include_str!("fixtures/recordings/submission-samples.hex"),
    include_str!("fixtures/recordings/submission-lines.hex"),
];

/// One recording, opened. The signature is not recorded — Ed25519 is
/// deterministic, so re-signing the same bytes with the same key reproduces it.
fn recorded(hex: &str) -> (Vec<u8>, Envelope) {
    use ed25519_dalek::Signer;
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let bytes = metsuke_wire::hex::decode_bytes(hex.trim()).unwrap();
    let opened =
        envelope::open(&key.verifying_key(), &bytes, &key.sign(&bytes), TEST_LIMITS).unwrap();
    (bytes, opened)
}

// A framing change that still round-trips through this build's own `open`
// would go unnoticed; the recordings are what make it a failing test. Opening
// them restates none of their values — the recorder owns those.
#[test]
fn this_build_seals_the_recorded_submissions() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    for hex in RECORDINGS {
        let (_, opened) = recorded(hex);
        // Level 0, as scripts/record-submission.sh sealed it.
        let (resealed, _) = envelope::seal(&key, &opened, 0).unwrap();
        assert_eq!(metsuke_wire::hex::encode(&resealed), hex.trim());
    }
}

// The point of the container: a tool that has never heard of metsuke skips the
// header frame and gets the payload. Asserted against `zstd` itself, because
// the claim is about what conforming decompressors do, not about this crate.
#[test]
fn zstd_decompresses_the_recordings_to_exactly_their_payloads() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    for hex in RECORDINGS {
        let (bytes, opened) = recorded(hex);
        let mut zstd = Command::new("zstd")
            .args(["-d", "--stdout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("zstd is on PATH for the test suite");
        zstd.stdin
            .take()
            .expect("stdin was piped")
            .write_all(&bytes)
            .unwrap();
        let output = zstd.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "zstd -d refused a recording: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, envelope::payload_lines(&opened).unwrap());
    }
}

/// A container built byte by byte, for the framings `seal` cannot produce.
/// `declared` is what the length field states, which is the point of the
/// helper: it is not required to be `header.len()`.
fn container(header: &[u8], declared: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&declared.to_le_bytes());
    bytes.extend_from_slice(header);
    bytes.extend_from_slice(data);
    bytes
}

/// A well-framed, correctly signed container around a hand-written header and
/// payload — how a client speaking a schema this build does not would send one.
fn sealed_header(
    key: &SigningKey,
    header: serde_json::Value,
    payload: &[u8],
) -> (Vec<u8>, envelope::Signature) {
    use ed25519_dalek::Signer;
    let header = header.to_string();
    let bytes = container(
        header.as_bytes(),
        header.len() as u32,
        &zstd::encode_all(payload, 0).unwrap(),
    );
    let signature = key.sign(&bytes);
    (bytes, signature)
}

/// The smallest envelope, for the tests that care about the container rather
/// than what it holds.
fn empty_samples_envelope(key: &SigningKey) -> Envelope {
    Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        Payload::Samples { samples: vec![] },
    )
}

/// One with a payload, for the tests that need the data frame to inflate to
/// something.
fn one_sample_envelope(key: &SigningKey) -> Envelope {
    Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        Payload::Samples {
            samples: vec![sample()],
        },
    )
}

fn sample() -> Sample {
    Sample {
        sampled_at: OffsetDateTime::UNIX_EPOCH,
        block_height: None,
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}
