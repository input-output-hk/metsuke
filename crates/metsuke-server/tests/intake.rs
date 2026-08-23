//! Ingest pipeline tests (ticket metsuke-4zo.6): one submission in, one
//! archived object and an ACK out, and every check in the ADR-0002 chain
//! rejecting on its own with its own reason.

use metsuke_server::archive::FilesystemArchive;
use metsuke_server::authority::{ColdKey, ColdKeyOrCalidus};
use metsuke_server::calidus::CalidusKeys;
use metsuke_server::config::IngestConfig;
use metsuke_server::counters::CounterStore;
use metsuke_server::intake::{IngestError, Intake, Rejection};
use metsuke_wire::envelope::{Envelope, PoolId, SCHEMA_VERSION, SigningKey};
use time::OffsetDateTime;

mod support;
use support::{
    CannedDirectory, FailingArchive, UnavailableDirectory, calidus_authority, calidus_key,
    counter_store, envelope_for, nonzero_u32, nonzero_u64, other_key, permissive_config, pool_of,
    registration, rotated_calidus_key, seal, stored_submission, submission, test_key, test_now,
};

/// An intake wired to a temporary directory and database, ready to submit
/// to. The directory is returned because dropping it deletes the archive.
fn intake_with(config: IngestConfig) -> (Intake<FilesystemArchive, ColdKey>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let counters = counter_store(dir.path());
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    (Intake::new(config, counters, archive, ColdKey), dir)
}

fn intake_for(pools: &[PoolId]) -> (Intake<FilesystemArchive, ColdKey>, tempfile::TempDir) {
    intake_with(permissive_config(pools))
}

fn rejection(error: IngestError) -> Rejection {
    match error {
        IngestError::Rejected(rejection) => rejection,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn submit(
    intake: &mut Intake<FilesystemArchive, ColdKey>,
    key: &SigningKey,
    envelope: &Envelope,
) -> Result<metsuke_wire::envelope::Ack, IngestError> {
    let (body, signature) = seal(key, envelope);
    intake.submit(
        &submission(key.verifying_key(), envelope.pool_id, signature, &body),
        test_now(),
    )
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
            &submission(key.verifying_key(), envelope.pool_id, signature, &body),
            test_now(),
        )
        .unwrap();

    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);
    let key_path = stored_submission(&key, envelope.counter, envelope.timestamp, signature, &body)
        .object_key();
    let stored = std::fs::read(dir.path().join("archive").join(&key_path)).unwrap();
    assert_eq!(stored, body, "archived object must be the received bytes");
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

// A key that does not hash to the pool id may not speak for it (ADR 0003).
#[test]
fn key_that_is_not_the_pools_cold_key_is_rejected() {
    let key = test_key();
    let impostor = other_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let mut envelope = envelope_for(&impostor, 1);
    envelope.pool_id = pool_of(&key);
    let (body, signature) = seal(&impostor, &envelope);

    let error = intake
        .submit(
            &submission(impostor.verifying_key(), pool_of(&key), signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::UnauthorizedKey { .. }),
        "expected the cold-key check to reject"
    );
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
            &submission(key.verifying_key(), envelope.pool_id, signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::BadSignature),
        "expected the signature check to reject"
    );
}

// Acceptance: nothing reads decompressed bytes before the signature passes.
// A zstd bomb far over the ceiling, signed by nobody, must come back as a
// signature failure — the decompressor never saw it.
#[test]
fn unsigned_bomb_is_rejected_before_it_is_decompressed() {
    let key = test_key();
    let config = IngestConfig {
        max_decompressed_bytes: nonzero_u64(1024),
        ..permissive_config(&[pool_of(&key)])
    };
    let (mut intake, _dir) = intake_with(config);
    let bomb = zstd::encode_all(vec![0u8; 64 * 1024 * 1024].as_slice(), 0).unwrap();
    let (_, signature) = seal(&key, &envelope_for(&key, 1));

    let error = intake
        .submit(
            &submission(key.verifying_key(), pool_of(&key), signature, &bomb),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::BadSignature),
        "the bomb must fail on its signature, not on its size"
    );
}

// Acceptance: decompression stops at the configured ceiling. The payload is
// authentic, so it reaches the decompressor — which reads no further than
// the limit before refusing.
#[test]
fn payload_over_the_decompression_ceiling_is_rejected() {
    let key = test_key();
    let config = IngestConfig {
        max_decompressed_bytes: nonzero_u64(512),
        ..permissive_config(&[pool_of(&key)])
    };
    let (mut intake, _dir) = intake_with(config);
    let mut envelope = envelope_for(&key, 1);
    envelope.samples = std::iter::repeat_n(envelope.samples[0].clone(), 1000).collect();

    let error = submit(&mut intake, &key, &envelope).unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::OversizedPayload { max: 512 }),
        "expected the decompression ceiling to reject"
    );
}

// The compressed body is capped before anything touches it.
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

// Acceptance: a replayed batch is refused, and so is any counter that fails
// to advance (ADR 0002).
#[test]
fn replayed_counter_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    submit(&mut intake, &key, &envelope_for(&key, 7)).unwrap();

    let replay = rejection(submit(&mut intake, &key, &envelope_for(&key, 7)).unwrap_err());
    assert!(
        matches!(replay, Rejection::ReplayedCounter { found: 7, last: 7 }),
        "expected the replay check to reject, got {replay}"
    );
    let stale = rejection(submit(&mut intake, &key, &envelope_for(&key, 3)).unwrap_err());
    assert!(
        matches!(stale, Rejection::ReplayedCounter { found: 3, last: 7 }),
        "expected the replay check to reject, got {stale}"
    );
    submit(&mut intake, &key, &envelope_for(&key, 8)).unwrap();
}

// Counter state is per pool: one pool's traffic never blocks another's.
#[test]
fn counters_are_tracked_per_pool() {
    let first = test_key();
    let second = other_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&first), pool_of(&second)]);
    submit(&mut intake, &first, &envelope_for(&first, 42)).unwrap();
    submit(&mut intake, &second, &envelope_for(&second, 1)).unwrap();
}

// The signed envelope's own claim about which pool it is for must match the
// header the allowlist and rate limit were checked against.
#[test]
fn envelope_for_another_pool_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let mut envelope = envelope_for(&key, 1);
    envelope.pool_id = pool_of(&other_key());
    let (body, signature) = seal(&key, &envelope);

    let error = intake
        .submit(
            &submission(key.verifying_key(), pool_of(&key), signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::PoolIdMismatch { .. }),
        "expected the pool id cross-check to reject"
    );
}

// A future schema (ADR 0001 reserves v2 for the log-based payload) is not
// silently parsed as v1.
#[test]
fn unsupported_schema_version_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    let mut envelope = envelope_for(&key, 1);
    envelope.schema_version = SCHEMA_VERSION + 1;

    let error = submit(&mut intake, &key, &envelope).unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::UnsupportedSchema { .. }),
        "expected the schema check to reject"
    );
}

// The timestamp backstop: too far from server time in either direction.
#[test]
fn timestamp_outside_the_window_is_rejected() {
    let key = test_key();
    let (mut intake, _dir) = intake_for(&[pool_of(&key)]);
    for offset in [-3600, 3600] {
        let mut envelope = envelope_for(&key, 1);
        envelope.timestamp = test_now() + time::Duration::seconds(offset);
        let error = submit(&mut intake, &key, &envelope).unwrap_err();
        assert!(
            matches!(rejection(error), Rejection::TimestampOutOfWindow { .. }),
            "expected the timestamp window to reject an offset of {offset}s"
        );
    }
}

// A store that cannot store is the server's failure, not the client's: it
// must not come back as a rejection the operator would chase (ADR 0004 —
// no ACK, so the client keeps the samples spooled). And counter state must
// not have advanced, because the chain did not pass (ADR 0002).
#[test]
fn archive_failure_is_not_a_rejection_and_leaves_the_counter_alone() {
    let key = test_key();
    let dir = tempfile::tempdir().unwrap();
    let counters_path = dir.path().join("counters.sqlite");
    let counters = CounterStore::open(&counters_path).unwrap();
    let mut intake = Intake::new(
        permissive_config(&[pool_of(&key)]),
        counters,
        FailingArchive {
            reason: "archive is down",
        },
        ColdKey,
    );
    let envelope = envelope_for(&key, 1);
    let (body, signature) = seal(&key, &envelope);

    let error = intake
        .submit(
            &submission(key.verifying_key(), envelope.pool_id, signature, &body),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(error, IngestError::Archive(_)),
        "expected an availability error, got {error:?}"
    );
    let recorded = CounterStore::open(&counters_path)
        .unwrap()
        .last_counter(pool_of(&key))
        .unwrap();
    assert_eq!(recorded, None, "an unstored batch must not spend a counter");
}

// The object key is what makes the bucket browsable by pool and day
// (ADR 0005).
#[test]
fn object_key_groups_by_pool_and_day() {
    let key = test_key();
    let timestamp = OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap();
    let stored = stored_submission(
        &key,
        12,
        timestamp,
        seal(&key, &envelope_for(&key, 12)).1,
        &[],
    );
    assert_eq!(
        stored.object_key(),
        format!("v1/{}/2025-08-12/1755000000-12.json.zst", pool_of(&key))
    );
}

/// An intake whose authority can resolve Calidus keys, plus the directory it
/// resolves them from (ADR 0003).
fn calidus_intake(
    pools: &[PoolId],
    directory: CannedDirectory,
) -> (
    Intake<FilesystemArchive, ColdKeyOrCalidus<CannedDirectory>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let counters = counter_store(dir.path());
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    let intake = Intake::new(
        permissive_config(pools),
        counters,
        archive,
        calidus_authority(directory),
    );
    (intake, dir)
}

/// What a pool's Calidus-signed upload looks like: the envelope is the pool's,
/// the signature is the hot key's.
fn calidus_submit(
    intake: &mut Intake<FilesystemArchive, ColdKeyOrCalidus<CannedDirectory>>,
    signer: &SigningKey,
    counter: u64,
) -> Result<metsuke_wire::envelope::Ack, IngestError> {
    let envelope = envelope_for(&test_key(), counter);
    let (body, signature) = seal(signer, &envelope);
    intake.submit(
        &submission(signer.verifying_key(), envelope.pool_id, signature, &body),
        test_now(),
    )
}

// Acceptance: the second key path of ADR 0003.
#[test]
fn upload_signed_with_the_pools_registered_calidus_key_is_accepted() {
    let pool = pool_of(&test_key());
    let directory = CannedDirectory::holding(pool, vec![registration(&calidus_key(), 1)]);
    let (mut intake, _dir) = calidus_intake(&[pool], directory.clone());

    calidus_submit(&mut intake, &calidus_key(), 1).unwrap();

    assert_eq!(directory.lookups(), 1);
}

// Acceptance: a key nobody registered is refused. The attempt that resolved
// the pool must not also refresh it — a fetch one line old cannot come back
// different, and spending the budget there is what would keep a real rotation
// in the same window from converging.
#[test]
fn a_key_the_pool_never_registered_is_rejected() {
    let pool = pool_of(&test_key());
    let directory = CannedDirectory::holding(pool, vec![registration(&calidus_key(), 1)]);
    let (mut intake, _dir) = calidus_intake(&[pool], directory.clone());

    let error = calidus_submit(&mut intake, &other_key(), 1).unwrap_err();

    assert!(
        matches!(rejection(error), Rejection::UnauthorizedKey { .. }),
        "an unregistered key must not speak for the pool"
    );
    assert_eq!(directory.lookups(), 1);
}

// The pool's rotation still converges after a stranger's attempt, because that
// attempt left the budget alone. The pool rotates to a key the cache has never
// held, so only a refresh can accept it.
#[test]
fn a_strangers_attempt_does_not_spend_the_pools_refresh() {
    let pool = pool_of(&test_key());
    let directory = CannedDirectory::holding(pool, vec![registration(&calidus_key(), 1)]);
    let (mut intake, _dir) = calidus_intake(&[pool], directory.clone());
    calidus_submit(&mut intake, &other_key(), 1).unwrap_err();

    directory.rotate(pool, vec![registration(&rotated_calidus_key(), 2)]);
    calidus_submit(&mut intake, &rotated_calidus_key(), 2).unwrap();

    assert_eq!(directory.lookups(), 2, "the stranger cost the first lookup");
}

// A refresh the budget refused decided nothing: the server declined to look
// again, which is not the same answer as "this key is not the pool's" and must
// not reach the client as its fault.
#[test]
fn an_upload_whose_refresh_is_throttled_is_not_a_rejection() {
    let pool = pool_of(&test_key());
    let directory = CannedDirectory::holding(pool, vec![registration(&calidus_key(), 1)]);
    let (mut intake, _dir) = calidus_intake(&[pool], directory.clone());
    // The first attempt fills the cache, the second spends the one refresh.
    calidus_submit(&mut intake, &other_key(), 1).unwrap_err();
    calidus_submit(&mut intake, &other_key(), 2).unwrap_err();

    let error = calidus_submit(&mut intake, &other_key(), 3).unwrap_err();

    assert!(
        matches!(error, IngestError::Undecided(_)),
        "expected an availability error, got {error:?}"
    );
    assert_eq!(directory.lookups(), 2, "a throttled refresh asks nothing");
}

// Acceptance: rotation converges without a TTL. The cache predates the new
// registration, and the upload it cannot explain is what fetches it.
#[test]
fn a_rotated_calidus_key_is_accepted_after_one_refresh() {
    let pool = pool_of(&test_key());
    let directory = CannedDirectory::holding(pool, vec![registration(&calidus_key(), 1)]);
    let (mut intake, _dir) = calidus_intake(&[pool], directory.clone());
    calidus_submit(&mut intake, &calidus_key(), 1).unwrap();

    directory.rotate(pool, vec![registration(&other_key(), 2)]);
    calidus_submit(&mut intake, &other_key(), 2).unwrap();

    assert_eq!(directory.lookups(), 2, "one refresh, not one per upload");
}

// A directory that cannot answer decided nothing, so the upload is worth
// retrying and must not come back as the client's fault (ADR 0004).
#[test]
fn a_directory_that_cannot_answer_is_not_a_rejection() {
    let pool = pool_of(&test_key());
    let dir = tempfile::tempdir().unwrap();
    let counters = counter_store(dir.path());
    let mut intake = Intake::new(
        permissive_config(&[pool]),
        counters,
        FilesystemArchive::new(&dir.path().join("archive")),
        ColdKeyOrCalidus::new(CalidusKeys::new(
            UnavailableDirectory {
                reason: "db-sync is down",
            },
            nonzero_u32(1),
            nonzero_u64(3600),
        )),
    );
    let envelope = envelope_for(&test_key(), 1);
    let (body, signature) = seal(&calidus_key(), &envelope);

    let error = intake
        .submit(
            &submission(
                calidus_key().verifying_key(),
                envelope.pool_id,
                signature,
                &body,
            ),
            test_now(),
        )
        .unwrap_err();

    assert!(
        matches!(error, IngestError::Undecided(_)),
        "expected an availability error, got {error:?}"
    );
}
