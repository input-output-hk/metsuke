//! Tier-1 property tests for the wire contract: seal→open roundtrip, tamper
//! rejection (ticket metsuke-4zo.1) and the container the two frames make
//! (ticket metsuke-jfb.1).

use std::collections::BTreeMap;

use metsuke_wire::envelope::{
    self, AgentId, CONTAINER_MAGIC, Envelope, Failure, HEADER_OFFSET, Limits, Metric,
    PROVENANCE_KEY, Payload, PayloadLine, PoolId, Provenance, Reason, SCHEMA_VERSION_LINES,
    SCHEMA_VERSION_SCRAPES, Scrape, SigningKey, TraceLine,
};
use metsuke_wire::fixtures;
use proptest::prelude::*;
use serde_json::Number;
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

/// One metric, generated the way a body states them: an integer or a float
/// value, labels sometimes, a declared type sometimes.
fn arb_metric() -> impl Strategy<Value = Metric> {
    (
        "[a-zA-Z_][a-zA-Z0-9_]{0,40}",
        proptest::collection::btree_map("[a-z_]{1,8}", ".{0,16}", 0..4),
        prop_oneof![
            any::<u64>().prop_map(Number::from),
            any::<i64>().prop_map(Number::from),
            (-1.0e12f64..1.0e12)
                .prop_map(|value| Number::from_f64(value).expect("a finite float is a number")),
        ],
        proptest::option::of(prop_oneof![Just("gauge"), Just("counter"), Just("info")]),
    )
        .prop_map(|(name, labels, value, declared_type)| Metric {
            name,
            labels,
            value,
            declared_type: declared_type.map(str::to_string),
        })
}

fn arb_scrape() -> impl Strategy<Value = Scrape> {
    // The two a scrape can be, never both: metrics, or the reason there are
    // none.
    let outcome = prop_oneof![
        proptest::collection::vec(arb_metric(), 0..8).prop_map(|metrics| (None, metrics)),
        (
            prop_oneof![
                Just(Reason::Unreachable),
                Just(Reason::Refused),
                Just(Reason::TooLarge),
                Just(Reason::Unreadable),
            ],
            ".{0,32}",
        )
            .prop_map(|(reason, detail)| (Some(Failure { reason, detail }), Vec::new())),
    ];
    (arb_timestamp(), any::<Option<i64>>(), outcome).prop_map(
        |(scraped_at, clock_offset_ms, (failure, metrics))| Scrape {
            scraped_at,
            clock_offset_ms,
            failure,
            metrics,
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

/// Through `parse`, so what the generator emits is what the strict reader
/// accepts and the two cannot drift apart.
fn arb_agent_id() -> impl Strategy<Value = AgentId> {
    "[a-z0-9]{1,8}(-[a-z0-9]{1,8}){0,2}"
        .prop_map(|slug| AgentId::parse(&slug).expect("the generator emits slugs"))
}

/// An object of string fields under keys too short to be `PROVENANCE_KEY`,
/// which a `TraceLine` never holds:
/// `a_line_declaring_the_reserved_key_is_refused` covers that case on its own.
fn arb_trace_line() -> impl Strategy<Value = TraceLine> {
    proptest::collection::hash_map("[a-z]{1,6}", "(?s).{0,16}", 0..4).prop_map(|fields| {
        let object: serde_json::Map<String, serde_json::Value> = fields
            .into_iter()
            .map(|(key, value)| (key, serde_json::Value::String(value)))
            .collect();
        TraceLine::parse(&serde_json::Value::Object(object).to_string())
            .expect("an object naming no reserved key")
    })
}

fn arb_envelope() -> impl Strategy<Value = Envelope> {
    (
        arb_pool_id(),
        arb_agent_id(),
        arb_agent_version(),
        any::<u64>(),
        arb_timestamp(),
        proptest::collection::vec(arb_scrape(), 0..8),
    )
        .prop_map(
            |(pool_id, agent_id, agent_version, counter, timestamp, scrapes)| {
                envelope_of(
                    Provenance { pool_id, agent_id },
                    agent_version,
                    counter,
                    timestamp,
                    |stamp| scrapes_payload(&scrapes, stamp),
                )
            },
        )
}

fn arb_lines_envelope() -> impl Strategy<Value = Envelope> {
    (
        arb_pool_id(),
        arb_agent_id(),
        arb_agent_version(),
        any::<u64>(),
        arb_timestamp(),
        proptest::collection::vec(arb_trace_line(), 0..8),
    )
        .prop_map(
            |(pool_id, agent_id, agent_version, counter, timestamp, lines)| {
                envelope_of(
                    Provenance { pool_id, agent_id },
                    agent_version,
                    counter,
                    timestamp,
                    |stamp| lines_payload(&lines, stamp),
                )
            },
        )
}

/// The three properties that seal and open a whole envelope per case. Each one
/// costs a zstd compression and an Ed25519 sign-and-verify, which is most of
/// this file's runtime; the rest of the block below runs at proptest's default.
/// The regression files replay past failures whatever this says, so a shrunk
/// case count does not drop a case that has already caught something.
const SEALING: u32 = 64;

proptest! {
    #![proptest_config(ProptestConfig { cases: SEALING, ..ProptestConfig::default() })]

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
    // order and field for field, however many of them there are.
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
}

proptest! {
    // The id an agent reports under survives being made from any name at all,
    // and comes back out of the strict reader unchanged.
    #[test]
    fn a_slugified_name_is_an_agent_id(name in "(?s).{0,32}") {
        let Ok(slug) = AgentId::slugify(&name) else {
            prop_assert!(!name.chars().any(|c| c.is_ascii_alphanumeric()));
            return Ok(());
        };
        prop_assert_eq!(AgentId::parse(slug.as_str()).unwrap(), slug);
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

    // metsuke-jfb.9: what a row is charged is what the payload spends on it.
    // The agent budgets a batch by summing `wire_bytes` and the server bounds
    // the bytes `payload_lines` produced, so a framing change that moves one
    // without the other puts the two limits out of step.
    #[test]
    fn a_payload_costs_what_its_lines_charge(
        scrapes in proptest::collection::vec(arb_scrape(), 0..8),
        lines in proptest::collection::vec(arb_trace_line(), 0..8),
    ) {
        let stamp = test_stamp();
        let scrape_lines: Vec<PayloadLine> = scrapes
            .iter()
            .map(|scrape| PayloadLine::scrape(scrape, &stamp).unwrap())
            .collect();
        let trace_lines: Vec<PayloadLine> = lines
            .iter()
            .map(|line| PayloadLine::trace_line(line, &stamp).unwrap())
            .collect();
        for lines in [scrape_lines, trace_lines] {
            let charged: u64 = lines.iter().map(PayloadLine::wire_bytes).sum();
            let envelope = Envelope::new(
                stamp.clone(),
                "0.1.0".into(),
                1,
                OffsetDateTime::UNIX_EPOCH,
                Payload::scrapes(lines),
            );
            prop_assert_eq!(envelope::payload_lines(&envelope).len() as u64, charged);
        }
    }

    // Both payload shapes make the same line; ADR 0010 says why that matters.
    #[test]
    fn every_payload_line_carries_the_batch_s_provenance(
        scrapes in arb_envelope(),
        lines in arb_lines_envelope(),
    ) {
        for envelope in [scrapes, lines] {
            let stated = serde_json::to_value(&envelope.provenance).unwrap();
            let body = String::from_utf8(envelope::payload_lines(&envelope)).unwrap();
            for line in body.lines() {
                let line: serde_json::Value = serde_json::from_str(line).unwrap();
                prop_assert_eq!(&line[PROVENANCE_KEY], &stated);
            }
        }
    }
}

// The stamp is what an agent knows before it spools a line, which is the pool
// and the machine and nothing else: the batch's counter and timestamp are drawn
// when it is sealed, so they stay in the header.
#[test]
fn a_stamped_line_names_the_pool_and_the_agent() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let body = envelope::payload_lines(&one_scrape_envelope(&key));
    let line: serde_json::Value =
        serde_json::from_slice(body.strip_suffix(b"\n").unwrap()).unwrap();
    let stamp = line[PROVENANCE_KEY].as_object().unwrap();

    assert_eq!(stamp.keys().collect::<Vec<_>>(), ["agent_id", "pool_id"]);
    // Beside its own fields, not instead of them.
    assert!(line["scraped_at"].is_string());
}

// A line already using metsuke's one reserved key (ADR 0010) is not a line this
// can stamp.
#[test]
fn a_line_declaring_the_reserved_key_is_refused() {
    let line = format!(r#"{{"ns":"Consensus.LeiosKernel","{PROVENANCE_KEY}":"mine"}}"#);
    assert!(matches!(
        TraceLine::parse(&line),
        Err(envelope::TraceLineError::ReservedKey)
    ));
}

#[test]
fn a_line_that_is_not_one_whole_object_is_refused() {
    for line in [
        "not json",
        "[1,2]",
        r#""a string""#,
        r#"{"ns":"Consensus.LeiosKernel""#,
        r#"{"a":1}{"b":2}"#,
    ] {
        assert!(
            matches!(
                TraceLine::parse(line),
                Err(envelope::TraceLineError::NotAnObject(_))
            ),
            "{line} is not one whole JSON object"
        );
    }
}

// `TraceLine::to_line` re-renders a parsed object without a fallible path,
// which holds only because the parse refuses what JSON cannot write back.
#[test]
fn a_number_json_cannot_write_back_is_refused_at_the_parse() {
    assert!(matches!(
        TraceLine::parse(r#"{"slot":1e999}"#),
        Err(envelope::TraceLineError::NotAnObject(_))
    ));
}

#[test]
fn open_refuses_a_line_stamped_with_another_batch() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let pool_id = PoolId::from_cold_key(&key.verifying_key());
    let elsewhere = Provenance {
        pool_id,
        agent_id: AgentId::parse("other-relay").unwrap(),
    };
    let line = serde_json::json!({"ns": "Consensus.LeiosKernel", PROVENANCE_KEY: elsewhere});
    let (bytes, sig) = sealed_header(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES,
            "pool_id": pool_id.to_bech32(),
            "agent_id": "relay-1",
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
        }),
        format!("{line}\n").as_bytes(),
    );
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
    assert!(
        matches!(err, envelope::OpenError::LineProvenance { index: 0 }),
        "expected the offending line to be named, got: {err}"
    );
}

// A line with no provenance at all is refused by the field it does not have,
// and by its place in the payload (`OpenError::LineStamp`).
#[test]
fn open_refuses_an_unstamped_line() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let pool_id = PoolId::from_cold_key(&key.verifying_key());
    let stamp = Provenance {
        pool_id,
        agent_id: AgentId::parse("relay-1").unwrap(),
    };
    let stamped = serde_json::json!({"ns": "Consensus.LeiosKernel", PROVENANCE_KEY: stamp});
    let (bytes, sig) = sealed_header(
        &key,
        serde_json::json!({
            "schema_version": SCHEMA_VERSION_LINES,
            "pool_id": pool_id.to_bech32(),
            "agent_id": "relay-1",
            "agent_version": "0.1.0",
            "counter": 1,
            "timestamp": "1970-01-01T00:00:00Z",
        }),
        format!("{stamped}\n{{\"ns\":\"Consensus.LeiosKernel\"}}\n").as_bytes(),
    );
    let err = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap_err();
    assert!(
        matches!(err, envelope::OpenError::LineStamp { index: 1, .. }),
        "expected the offending line to be named, got: {err}"
    );
    assert!(
        err.to_string().contains(PROVENANCE_KEY),
        "the refusal must name what is missing, got: {err}"
    );
}

// Who sent a submission, and under which schema, without a key and without a
// decompressor: seek past the magic and the length, parse what follows.
#[test]
fn the_header_reads_back_without_a_decompressor() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = empty_scrapes_envelope(&key);
    let (bytes, _) = envelope::seal(&key, &env, 0).unwrap();

    let frames = envelope::split(&bytes, TEST_LIMITS.max_header_bytes).unwrap();
    let header: serde_json::Value = serde_json::from_slice(frames.header).unwrap();

    assert_eq!(header["schema_version"], SCHEMA_VERSION_SCRAPES);
    assert_eq!(header["pool_id"], env.provenance.pool_id.to_bech32());
    assert_eq!(header["counter"], env.counter);
}

// The same read as the fields a caller acts on, which is what an ingest path
// files an object by (`envelope::read_header`).
#[test]
fn read_header_answers_the_batch_s_own_account_of_itself() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let env = empty_scrapes_envelope(&key);
    let (bytes, _) = envelope::seal(&key, &env, 0).unwrap();

    let header = envelope::read_header(&bytes, TEST_LIMITS.max_header_bytes).unwrap();

    assert_eq!(header.schema_version, SCHEMA_VERSION_SCRAPES);
    assert_eq!(header.provenance, env.provenance);
    assert_eq!(header.agent_version, env.agent_version);
    assert_eq!(header.counter, env.counter);
    assert_eq!(header.timestamp, env.timestamp);
}

// The header's JSON is the wire, so the Rust shape behind it is free to move
// only as long as these bytes do not: `Provenance` became one flattened field
// of `Header`, and this is what says the keys stayed where they were.
#[test]
fn the_header_renders_the_same_bytes_as_it_always_has() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let header = envelope::header_json(&empty_scrapes_envelope(&key)).unwrap();
    assert_eq!(
        String::from_utf8(header).unwrap(),
        r#"{"schema_version":1,"pool_id":"pool13vscgf9dwn0jt56u965wp99ychz6avktk3pyrye326f3xctz4nm","agent_id":"relay-1","agent_version":"0.1.0","counter":1,"timestamp":"1970-01-01T00:00:00Z"}"#
    );
}

// A header frame that is not a header is its own refusal, told apart from a
// body that is not a container at all.
#[test]
fn read_header_refuses_a_frame_that_is_not_a_header() {
    let not_a_header = container(b"{}", 2, &[]);
    assert!(matches!(
        envelope::read_header(&not_a_header, 4096),
        Err(envelope::HeaderError::Json(_))
    ));
    assert!(matches!(
        envelope::read_header(b"not a container", 4096),
        Err(envelope::HeaderError::Container(
            envelope::ContainerError::NotAContainer
        ))
    ));
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
            "schema_version": SCHEMA_VERSION_SCRAPES,
            "pool_id": "pool1notvalidbech32",
            "agent_id": "relay-1",
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
            "agent_id": "relay-1",
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
            "agent_id": "relay-1",
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
    let env = one_scrape_envelope(&key);
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
    let env = empty_scrapes_envelope(&key);
    let (bytes, sig) = envelope::seal(&key, &env, 0).unwrap();
    let limits = Limits {
        max_decompressed_bytes: 64 * 1024 * 1024 * 1024,
        ..TEST_LIMITS
    };
    let opened = envelope::open(&key.verifying_key(), &bytes, &sig, limits).unwrap();
    assert_eq!(opened, env);
}

// What `Metric::value` being a JSON number buys, at the one value that shows
// it: 2^53 + 1 is the smallest integer an f64 cannot hold, so a value that went
// through a float comes back one short.
#[test]
fn a_counter_past_an_f64_s_exact_range_keeps_its_digits_through_seal_and_open() {
    const ALLOCATED: u64 = (1u64 << 53) + 1;
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut counter = scrape();
    counter.metrics[0] = Metric {
        name: "rts_gc_bytes_allocated".to_string(),
        labels: BTreeMap::new(),
        value: ALLOCATED.into(),
        declared_type: Some("counter".to_string()),
    };
    let envelope = test_envelope(&key, |stamp| scrapes_payload(&[counter], stamp));
    let (bytes, sig) = envelope::seal(&key, &envelope, 0).unwrap();

    let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap();
    let value = &opened.scrapes().unwrap()[0].metrics[0].value;
    assert_eq!(value.as_u64(), Some(ALLOCATED));
    assert_eq!(value.to_string(), ALLOCATED.to_string());
}

// What a consumer gets back out of a batch: the fields, not the bytes. The
// only place a payload struct reads a payload line.
#[test]
fn a_consumer_reads_a_batch_s_scrapes_back_as_scrapes() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let (bytes, sig) = envelope::seal(&key, &one_scrape_envelope(&key), 0).unwrap();
    let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap();

    assert_eq!(opened.scrapes().unwrap(), [scrape()]);
}

// Asking a batch for the shape it does not carry names both versions rather
// than reporting the fields the other schema happens not to have.
#[test]
fn reading_a_scrape_batch_as_trace_lines_names_both_schemas() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let err = one_scrape_envelope(&key).trace_lines().unwrap_err();
    assert!(
        matches!(
            err,
            envelope::ReadError::PayloadIsNot {
                asked: SCHEMA_VERSION_LINES,
                found: SCHEMA_VERSION_SCRAPES
            }
        ),
        "expected both versions to be named, got: {err}"
    );
}

// The newline is what terminates a line, and a `TraceLine` is a JSON object
// whose writer escapes every one it holds: the framing byte cannot appear
// inside a line, so nothing has to check for it.
#[test]
fn a_line_holding_a_newline_seals_as_one_line() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let envelope = test_envelope(&key, |stamp| {
        lines_payload(
            &[TraceLine::parse("{\"msg\":\"first\\nsecond\"}").unwrap()],
            stamp,
        )
    });
    let (bytes, sig) = envelope::seal(&key, &envelope, 0).unwrap();
    let opened = envelope::open(&key.verifying_key(), &bytes, &sig, TEST_LIMITS).unwrap();
    assert_eq!(opened, envelope);
}

#[test]
fn slugify_folds_a_hostname_into_an_agent_id() {
    for (name, slug) in [
        ("relay-1", "relay-1"),
        ("Relay_1", "relay-1"),
        ("bp.example.org", "bp-example-org"),
        ("  spaced  out  ", "spaced-out"),
        ("Ω-node", "node"),
    ] {
        assert_eq!(AgentId::slugify(name).unwrap().as_str(), slug);
    }
}

// The one name that leaves nothing to report under. Refusing it is the whole
// exception to slugifying rather than rejecting.
#[test]
fn slugify_refuses_a_name_with_nothing_alphanumeric_in_it() {
    let err = AgentId::slugify("___").unwrap_err();
    assert!(
        err.to_string().contains("___"),
        "the refusal must name what it was given, got: {err}"
    );
}

// The wire reader takes only what `slugify` emits, so an id that arrives is an
// id something made rather than a string that travelled.
#[test]
fn parse_refuses_what_slugify_would_never_emit() {
    for found in ["", "Relay-1", "relay_1", "-relay", "relay-", "a--b"] {
        assert!(
            matches!(
                AgentId::parse(found),
                Err(envelope::AgentIdError::NotASlug { .. })
            ),
            "{found:?} is not a slug"
        );
    }
}

/// The bytes this build's sealing path produced for each payload shape,
/// lowercase hex on one line. Provenance: tests/fixtures/recordings/README.md.
const RECORDINGS: [&str; 2] = [
    include_str!("fixtures/recordings/submission-scrapes.hex"),
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
//
// Resealed from the fields rather than from what `open` returned: `open` takes
// each line as the text it received and `seal` concatenates them, so resealing
// an opened recording reproduces whatever the recording said. Going back
// through the payload structs and the stamp is what puts this build's own
// rendering in the comparison.
#[test]
fn this_build_seals_the_recorded_submissions() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    for hex in RECORDINGS {
        let (_, opened) = recorded(hex);
        let (resealed, _) = envelope::seal(&key, &restamped(&opened), 0).unwrap();
        assert_eq!(metsuke_wire::hex::encode(&resealed), hex.trim());
    }
}

/// The same batch with every line rendered again from its own fields.
fn restamped(envelope: &Envelope) -> Envelope {
    let stamp = &envelope.provenance;
    let payload = match envelope.schema_version() {
        SCHEMA_VERSION_SCRAPES => scrapes_payload(&envelope.scrapes().unwrap(), stamp),
        SCHEMA_VERSION_LINES => lines_payload(&envelope.trace_lines().unwrap(), stamp),
        found => panic!("a recording this build opened declared v{found}"),
    };
    Envelope::new(
        stamp.clone(),
        envelope.agent_version.clone(),
        envelope.counter,
        envelope.timestamp,
        payload,
    )
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
        assert_eq!(output.stdout, envelope::payload_lines(&opened));
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

/// An envelope stamped with its own header, as the agent's spool stamps a row.
/// `open` checks every line against the header, so lines stamped with anything
/// else make a batch that can only fail to open — which is
/// `open_refuses_a_line_stamped_with_another_batch`, not a builder's job.
fn envelope_of(
    provenance: Provenance,
    agent_version: String,
    counter: u64,
    timestamp: OffsetDateTime,
    payload: impl FnOnce(&Provenance) -> Payload,
) -> Envelope {
    let payload = payload(&provenance);
    Envelope::new(provenance, agent_version, counter, timestamp, payload)
}

fn scrapes_payload(scrapes: &[Scrape], stamp: &Provenance) -> Payload {
    Payload::scrapes(
        scrapes
            .iter()
            .map(|scrape| PayloadLine::scrape(scrape, stamp).expect("a generated scrape stamps"))
            .collect(),
    )
}

fn lines_payload(lines: &[TraceLine], stamp: &Provenance) -> Payload {
    Payload::trace_lines(
        lines
            .iter()
            .map(|line| PayloadLine::trace_line(line, stamp).expect("a parsed line stamps"))
            .collect(),
    )
}

/// The fixed identity the non-property tests report under.
fn test_stamp() -> Provenance {
    Provenance {
        pool_id: PoolId::from_cold_key(&SigningKey::from_bytes(&[7u8; 32]).verifying_key()),
        agent_id: AgentId::parse("relay-1").unwrap(),
    }
}

fn test_envelope(key: &SigningKey, payload: impl FnOnce(&Provenance) -> Payload) -> Envelope {
    envelope_of(
        Provenance {
            pool_id: PoolId::from_cold_key(&key.verifying_key()),
            agent_id: AgentId::parse("relay-1").unwrap(),
        },
        "0.1.0".into(),
        1,
        OffsetDateTime::UNIX_EPOCH,
        payload,
    )
}

/// The smallest envelope, for the tests that care about the container rather
/// than what it holds.
fn empty_scrapes_envelope(key: &SigningKey) -> Envelope {
    test_envelope(key, |_| Payload::scrapes(vec![]))
}

/// One with a payload, for the tests that need the data frame to inflate to
/// something.
fn one_scrape_envelope(key: &SigningKey) -> Envelope {
    test_envelope(key, |stamp| scrapes_payload(&[scrape()], stamp))
}

/// One scrape carrying one metric, so the tests that only need a payload line
/// have the smallest one a real scrape produces.
fn scrape() -> Scrape {
    fixtures::block_number_scrape(OffsetDateTime::UNIX_EPOCH, 5)
}
