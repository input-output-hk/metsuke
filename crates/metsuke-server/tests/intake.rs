//! Ingest pipeline tests: one submission in, one archived object and an ACK
//! out, and each of the three checks — allowlist, key-belongs-to-pool,
//! signature — rejecting on its own with its own reason.

use metsuke_server::archive::{FilesystemArchive, List, ObjectName};
use metsuke_server::config::IngestConfig;
use metsuke_server::http::status_for;
use metsuke_server::intake::{IngestError, Intake, Rejection};
use metsuke_wire::envelope::{
    CONTAINER_MAGIC, ContainerError, Envelope, PoolId, SCHEMA_VERSION_LINES,
    SCHEMA_VERSION_SAMPLES, SigningKey,
};

mod support;
use support::{
    FailingArchive, envelope_for, lines_envelope_at, nonzero_u32, nonzero_u64, other_key,
    permissive_config, pool_of, seal, submission, test_agent_id, test_key, test_now, trace_line,
};

/// An intake wired to a temporary directory, ready to submit to. The
/// directory is returned because dropping it deletes the archive.
fn intake_with(config: IngestConfig) -> (Intake<FilesystemArchive>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    (Intake::new(config, archive), dir)
}

fn intake_for(pools: &[PoolId]) -> (Intake<FilesystemArchive>, tempfile::TempDir) {
    intake_with(permissive_config(pools))
}

fn rejection(error: IngestError) -> Rejection {
    match error {
        IngestError::Rejected(rejection) => rejection,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn submit(
    intake: &mut Intake<FilesystemArchive>,
    key: &SigningKey,
    envelope: &Envelope,
) -> Result<metsuke_wire::envelope::Ack, IngestError> {
    let (body, signature) = seal(key, envelope);
    intake.submit(
        &submission(key.verifying_key(), signature, &body),
        test_now(),
    )
}

/// Every key the archive holds, in key order.
fn stored_keys(intake: &Intake<FilesystemArchive>) -> Vec<String> {
    intake.archive().page("", "", nonzero_u32(10)).unwrap().keys
}

/// The one key the archive holds, for a test that has to name the object the
/// intake stamped: the id in it is the intake's, not the test's.
fn only_key(intake: &Intake<FilesystemArchive>) -> String {
    match stored_keys(intake).as_slice() {
        [key] => key.clone(),
        other => panic!("expected one object, got {other:?}"),
    }
}

/// Submit a container built byte by byte, which is how a client speaking a
/// schema this build does not would send one. `declared` is what the frame's
/// length field states, so a test can state a length the header does not have.
fn submit_container(
    intake: &mut Intake<FilesystemArchive>,
    key: &SigningKey,
    header: &[u8],
    declared: u32,
    data: &[u8],
) -> Result<metsuke_wire::envelope::Ack, IngestError> {
    use ed25519_dalek::Signer;
    let bytes = container(header, declared, data);
    let signature = key.sign(&bytes);
    intake.submit(
        &submission(key.verifying_key(), signature, &bytes),
        test_now(),
    )
}

/// A container built byte by byte around `data`, which is compressed here so
/// only the framing is the caller's to state.
fn container(header: &[u8], declared: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&declared.to_le_bytes());
    bytes.extend_from_slice(header);
    bytes.extend_from_slice(&zstd::encode_all(data, 0).unwrap());
    bytes
}

/// The same for a header this build could not build an `Envelope` for.
fn submit_header(
    intake: &mut Intake<FilesystemArchive>,
    key: &SigningKey,
    header: serde_json::Value,
    data: &[u8],
) -> Result<metsuke_wire::envelope::Ack, IngestError> {
    let header = header.to_string();
    submit_container(intake, key, header.as_bytes(), header.len() as u32, data)
}

/// A well-formed header at the named schema version, for the tests whose
/// subject is what the header says rather than what it carries.
fn header(key: &SigningKey, schema_version: u32) -> serde_json::Value {
    serde_json::json!({
        "schema_version": schema_version,
        "pool_id": pool_of(key).to_bech32(),
        "agent_id": test_agent_id().as_str(),
        "agent_version": "0.1.0",
        "counter": 1,
        "timestamp": "2025-08-12T12:00:00Z",
    })
}

// Acceptance: a valid batch is archived as the bytes that were signed, and
// the ACK carries the client version this server was built against
// (ADR 0005, ADR 0006).
#[test]
fn valid_submission_is_archived_raw_and_acked() {
    let key = test_key();
    let (mut intake, dir) = intake_for(&[pool_of(&key)]);
    let envelope = envelope_for(&key, 1);
    let (body, signature) = seal(&key, &envelope);

    let ack = intake
        .submit(
            &submission(key.verifying_key(), signature, &body),
            test_now(),
        )
        .unwrap();

    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);
    let stored = std::fs::read(dir.path().join("archive").join(only_key(&intake))).unwrap();
    assert_eq!(stored, body, "archived object must be the received bytes");
}

// The bucket is the only account of what was accepted (ADR 0005), and the key
// is what says whose the object is.
#[test]
fn an_accepted_submission_is_filed_under_what_it_carries() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);

    submit(&mut intake, &key, &envelope_for(&key, 4)).unwrap();

    let name = ObjectName::parse(&only_key(&intake)).unwrap();
    assert_eq!(name.pool_id, pool_of(&key));
    assert_eq!(name.agent_id, test_agent_id());
    assert_eq!(name.kind, metsuke_server::archive::Kind::Metrics);
}

// The object key is what makes the bucket sync-able by one cursor: the day the
// server received it, then an id that orders within the day, then who sent it.
#[test]
fn the_object_key_is_time_major_and_names_the_sender() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);

    submit(&mut intake, &key, &envelope_for(&key, 1)).unwrap();

    let stored = only_key(&intake);
    let expected_tail = format!(
        "-{pool}-{agent}-metrics.jsonl.zst",
        pool = pool_of(&key),
        agent = test_agent_id(),
    );
    assert!(
        stored.starts_with("v1/2025-08-12/") && stored.ends_with(&expected_tail),
        "got: {stored}"
    );
}

// The motivating bug: two machines reporting for one pool. Both land, and the
// agent id in the key is what tells the two objects apart.
#[test]
fn two_agents_of_one_pool_both_land() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let first = envelope_for(&key, 1);
    let mut second = envelope_for(&key, 1);
    second.agent_id = metsuke_wire::envelope::AgentId::parse("other-relay").unwrap();

    submit(&mut intake, &key, &first).unwrap();
    submit(&mut intake, &key, &second).unwrap();

    let mut agents: Vec<String> = stored_keys(&intake)
        .iter()
        .map(|key| ObjectName::parse(key).unwrap().agent_id.to_string())
        .collect();
    agents.sort();
    assert_eq!(agents, vec!["other-relay", "test-relay"]);
}

// A pool nobody onboarded is refused before any cryptography runs.
#[test]
fn unknown_pool_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&other_key())]);
    let error = submit(&mut intake, &key, &envelope_for(&key, 1)).unwrap_err();
    assert!(
        matches!(rejection(error), Rejection::UnknownPool { .. }),
        "expected the allowlist to reject"
    );
}

// The key is what says which pool an upload is for, so a key that
// is nobody's cold key speaks for nobody: the pool it hashes to is not on the
// allowlist, and the refusal names that pool rather than the one the batch
// claims.
#[test]
fn a_key_that_is_not_the_pools_cold_key_speaks_for_nobody() {
    let pool = pool_of(&test_key());
    let impostor = other_key();
    let (mut intake, _dir) = intake_for(&[pool]);
    // The batch's own header still names the allowlisted pool: the server does
    // not read it, so the claim buys the impostor nothing.
    let envelope = envelope_for(&impostor, 1);
    let (body, signature) = seal(&impostor, &envelope);

    let error = intake
        .submit(
            &submission(impostor.verifying_key(), signature, &body),
            test_now(),
        )
        .unwrap_err();

    match rejection(error) {
        Rejection::UnknownPool { pool_id } => {
            assert_eq!(pool_id, pool_of(&impostor), "the key's pool, not the claim");
        }
        other => panic!("expected the derived pool to miss the allowlist, got {other:?}"),
    }
}

// A body altered in flight no longer verifies under the presented key.
#[test]
fn tampered_body_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let envelope = envelope_for(&key, 1);
    let (mut body, signature) = seal(&key, &envelope);
    *body.last_mut().unwrap() ^= 0xff;

    let error = intake
        .submit(
            &submission(key.verifying_key(), signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert_eq!(status_for(&error), 403);
    assert!(
        matches!(rejection(error), Rejection::BadSignature),
        "expected the signature check to reject"
    );
}

// Acceptance: the server never decompresses. A data frame that is not zstd at
// all is archived unread — the signature says the pool sent these bytes, and
// what is inside them is the consumer's problem, not this server's.
#[test]
fn a_data_frame_that_is_not_zstd_is_archived_unread() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let header = header(&key, SCHEMA_VERSION_SAMPLES).to_string();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(b"not a zstd frame");
    use ed25519_dalek::Signer;
    let signature = key.sign(&bytes);

    intake
        .submit(
            &submission(key.verifying_key(), signature, &bytes),
            test_now(),
        )
        .unwrap();
}

// The body as sent is capped before anything touches it.
#[test]
fn oversized_body_is_rejected() {
    let key = test_key();
    let config = IngestConfig {
        max_body_bytes: nonzero_u64(16),
        ..permissive_config(&[pool_of(&key)])
    };
    let (mut intake, _dir) = intake_with(config);
    let error = submit(&mut intake, &key, &envelope_for(&key, 1)).unwrap_err();
    assert!(
        matches!(rejection(error), Rejection::OversizedBody { .. }),
        "expected the body size cap to reject"
    );
}

// The per-pool budget is configuration: one upload per window here, so the
// second is refused and the first is not.
#[test]
fn second_upload_in_the_window_is_rate_limited() {
    let key = test_key();
    let config = IngestConfig {
        rate_limit_uploads: nonzero_u32(1),
        ..permissive_config(&[pool_of(&key)])
    };
    let (mut intake, _dir) = intake_with(config);
    submit(&mut intake, &key, &envelope_for(&key, 1)).unwrap();
    let error = submit(&mut intake, &key, &envelope_for(&key, 2)).unwrap_err();
    assert!(
        matches!(rejection(error), Rejection::RateLimited { .. }),
        "expected the rate limit to reject"
    );
}

// The shared budget is the other half: a pool inside its own limit is still
// refused once every pool together has filled the window, and the refusal says
// so rather than blaming the pool.
#[test]
fn the_shared_budget_refuses_a_pool_inside_its_own_limit() {
    let first = test_key();
    let second = other_key();
    let config = IngestConfig {
        rate_limit_uploads_total: nonzero_u32(1),
        ..permissive_config(&[pool_of(&first), pool_of(&second)])
    };
    let (mut intake, _dir) = intake_with(config);
    submit(&mut intake, &first, &envelope_for(&first, 1)).unwrap();

    let error = submit(&mut intake, &second, &envelope_for(&second, 1)).unwrap_err();

    assert_eq!(status_for(&error), 429);
    assert!(
        matches!(rejection(error), Rejection::ServerBusy { max: 1, .. }),
        "expected the shared window to reject"
    );
}

// The container check is first: a body that is not a submission is refused
// before the allowlist, the limiter or any cryptography — so a pool that is
// not allowlisted still hears about the framing rather than the allowlist.
#[test]
fn a_body_that_is_not_a_container_is_refused_before_the_allowlist() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[]);
    let bytes = zstd::encode_all(&b"{}\n"[..], 0).unwrap();
    use ed25519_dalek::Signer;
    let signature = key.sign(&bytes);

    let error = intake
        .submit(
            &submission(key.verifying_key(), signature, &bytes),
            test_now(),
        )
        .unwrap_err();

    assert_eq!(status_for(&error), 400);
    assert!(
        matches!(
            rejection(error),
            Rejection::NotASubmission(ContainerError::NotAContainer)
        ),
        "expected the container check to run before the allowlist"
    );
}

// The declared length is checked against the bound before any of it is read,
// so a body claiming a gigabyte header costs the bound rather than the claim.
#[test]
fn a_header_over_the_bound_is_refused() {
    let key = test_key();
    let config = IngestConfig {
        max_header_bytes: nonzero_u64(64),
        ..permissive_config(&[pool_of(&key)])
    };
    let (mut intake, _dir) = intake_with(config);

    let error = submit_container(&mut intake, &key, b"", 1 << 30, b"").unwrap_err();

    assert!(
        matches!(
            rejection(error),
            Rejection::NotASubmission(ContainerError::OversizedHeader {
                declared: 1_073_741_824,
                max: 64
            })
        ),
        "expected the header bound to name both numbers"
    );
}

// A header frame that is not a header is refused after the signature: these
// bytes are the pool's, and there is still no name to file them under.
#[test]
fn a_header_frame_that_does_not_read_is_refused() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);

    let error = submit_container(&mut intake, &key, b"{}", 2, b"").unwrap_err();

    assert_eq!(status_for(&error), 400);
    assert!(
        matches!(rejection(error), Rejection::UnreadableHeader(_)),
        "expected the header read to reject"
    );
}

// A schema this build has no name for cannot be filed, and the refusal names
// the version rather than reading as a malformed header.
#[test]
fn a_schema_version_with_no_object_name_is_refused() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let unknown = SCHEMA_VERSION_LINES + 1;

    let error = submit_header(&mut intake, &key, header(&key, unknown), b"").unwrap_err();

    assert_eq!(status_for(&error), 400);
    assert!(
        matches!(
            rejection(error),
            Rejection::UnnameableKind { schema_version } if schema_version == unknown
        ),
        "expected the kind to name the version"
    );
}

// The trace-line schema goes through the same chain and is filed as logs: the
// developers' data arrives by the path the samples already take (ADR 0005).
#[test]
fn a_trace_line_upload_is_accepted_and_filed_as_logs() {
    let key = test_key();
    let (mut intake, dir) = intake_for(&[pool_of(&key)]);
    let lines = vec![trace_line(
        r#"{"at":"2026-08-25T18:19:41.024688522Z","ns":"x","sev":"Info"}"#,
    )];
    let envelope = lines_envelope_at(&key, 1, test_now(), lines);
    assert_eq!(envelope.schema_version(), SCHEMA_VERSION_LINES);
    let (body, signature) = seal(&key, &envelope);

    let ack = intake
        .submit(
            &submission(key.verifying_key(), signature, &body),
            test_now(),
        )
        .unwrap();

    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);
    let stored_key = only_key(&intake);
    assert!(stored_key.ends_with("-logs.jsonl.zst"), "got: {stored_key}");
    let stored = std::fs::read(dir.path().join("archive").join(&stored_key)).unwrap();
    assert_eq!(stored, body, "archived object must be the received bytes");
}

// A store that cannot store is the server's failure, not the client's: it
// must not come back as a rejection the operator would chase (ADR 0004 —
// no ACK, so the client keeps the samples spooled).
#[test]
fn archive_failure_is_not_a_rejection() {
    let key = test_key();
    let mut intake = Intake::new(
        permissive_config(&[pool_of(&key)]),
        FailingArchive {
            reason: "archive is down",
        },
    );
    let envelope = envelope_for(&key, 1);
    let (body, signature) = seal(&key, &envelope);

    let error = intake
        .submit(
            &submission(key.verifying_key(), signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(error, IngestError::Archive(_)),
        "expected an availability error, got {error:?}"
    );
    assert_eq!(status_for(&error), 503);
}

// A refused submission archives nothing.
#[test]
fn a_rejected_submission_stores_no_object() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&other_key())]);

    submit(&mut intake, &key, &envelope_for(&key, 1)).unwrap_err();

    let keys = stored_keys(&intake);
    assert!(keys.is_empty(), "got: {keys:?}");
}
