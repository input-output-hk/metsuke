//! Object naming, both ways. The key is the only copy of a submission's
//! identity that survives losing the index (ADR 0005), so writing it and
//! reading it back must be one bijection, and a key that is not one must say
//! so rather than parse into a plausible wrong pool.

use metsuke_server::archive::{FilesystemArchive, Kind, List, ObjectName, Store};
use metsuke_wire::envelope::{AgentId, PoolId, SigningKey};
use proptest::prelude::*;
use time::{OffsetDateTime, UtcOffset};
use uuid::{NoContext, Timestamp, Uuid};

mod support;
use support::{
    envelope_for, object_name, pool_of, read_object, seal, stored_submission, test_agent_id,
    test_key, test_now,
};

/// A name whose id was stamped at `unix`, so a test can name the day a key
/// lands in without owning the id's random half.
fn name_at(pool_id: PoolId, unix: i64) -> ObjectName {
    ObjectName {
        id: Uuid::new_v7(Timestamp::from_unix(NoContext, unix as u64, 0)),
        pool_id,
        agent_id: test_agent_id(),
        kind: Kind::Metrics,
    }
}

#[test]
fn a_key_carries_the_day_the_id_the_pool_the_agent_and_the_kind() {
    let pool = pool_of(&test_key());
    let name = name_at(pool, 1_755_000_000);
    assert_eq!(
        name.to_key(),
        format!(
            "v1/2025-08-12/{id}-{pool}-{agent}-metrics.jsonl.zst",
            id = name.id,
            agent = test_agent_id(),
        )
    );
}

#[test]
fn a_key_parses_back_to_the_name_that_wrote_it() {
    let name = name_at(pool_of(&test_key()), 1_755_000_000);
    assert_eq!(ObjectName::parse(&name.to_key()).unwrap(), name);
}

/// The receipt instant carries an offset in it; the folder does not. A key
/// naming a local day would sort two pools' uploads of the same instant into
/// different folders.
#[test]
fn a_receipt_in_another_offset_is_keyed_by_its_utc_day() {
    // 22:00 UTC, which is the next day at +02:00.
    let instant = OffsetDateTime::from_unix_timestamp(1_755_036_000)
        .unwrap()
        .to_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
    let name = object_name(&test_key(), instant, Kind::Metrics);
    assert!(
        name.to_key().starts_with("v1/2025-08-12/"),
        "keyed under {}",
        name.to_key()
    );
}

/// One start-after cursor is the whole delta-sync protocol, and that rests on
/// keys sorting by receipt: the day folder orders the corpus and the UUIDv7
/// orders within the day.
#[test]
fn keys_sort_by_receipt_order() {
    let pool = pool_of(&test_key());
    let stamps = [1_754_000_000, 1_755_000_000, 1_755_100_000];
    let names = stamps.map(|unix| name_at(pool, unix));
    let mut keys = names.clone().map(|name| name.to_key());
    keys.sort();
    assert_eq!(keys.to_vec(), names.map(|name| name.to_key()).to_vec());
}

#[test]
fn a_key_that_is_not_an_archive_object_is_refused() {
    let pool = pool_of(&test_key());
    let id = name_at(pool, 1_755_000_000).id;
    for key in [
        String::new(),
        "duck.jpg".to_string(),
        // The right shape under the wrong schema version.
        format!("v2/2025-08-12/{id}-{pool}-test-relay-metrics.jsonl.zst"),
        // A pool id that is not bech32.
        format!("v1/2025-08-12/{id}-pool1nope-test-relay-metrics.jsonl.zst"),
        // An id that is not a uuid.
        format!(
            "v1/2025-08-12/{}-{pool}-test-relay-metrics.jsonl.zst",
            "x".repeat(36)
        ),
        // A payload kind nothing files.
        format!("v1/2025-08-12/{id}-{pool}-test-relay-guesses.jsonl.zst"),
        // An agent id no `slugify` would emit.
        format!("v1/2025-08-12/{id}-{pool}-Relay_1-metrics.jsonl.zst"),
        // Uncompressed, so not a body this server ever wrote.
        format!("v1/2025-08-12/{id}-{pool}-test-relay-metrics.jsonl"),
        // The pool segment missing entirely.
        format!("v1/2025-08-12/{id}-test-relay-metrics.jsonl.zst"),
    ] {
        assert!(
            ObjectName::parse(&key).is_err(),
            "{key:?} must not parse as an object name"
        );
    }
}

#[test]
fn a_malformed_key_names_itself_and_what_is_wrong() {
    let pool = pool_of(&test_key());
    let id = name_at(pool, 1_755_000_000).id;
    let error = ObjectName::parse(&format!(
        "v1/2025-08-12/{id}-{pool}-test-relay-guesses.jsonl.zst"
    ))
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("guesses") && error.contains(&pool.to_bech32()),
        "got: {error}"
    );
}

#[test]
fn a_key_whose_folder_contradicts_its_id_is_refused() {
    let pool = pool_of(&test_key());
    let id = name_at(pool, 1_755_000_000).id;
    let error = ObjectName::parse(&format!(
        "v1/2020-01-01/{id}-{pool}-test-relay-metrics.jsonl.zst"
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("2020-01-01"), "got: {error}");
}

/// A foreign writer or a migration can leave a key whose uuid is not a v7, and
/// only a v7 carries the day the folder has to agree with. It has to come back
/// as an error, because the process aborts if it reaches the day lookup.
#[test]
fn a_key_whose_id_is_not_a_uuid_v7_is_refused() {
    let pool = pool_of(&test_key());
    let id = Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef);
    assert_eq!(id.get_version_num(), 4);

    let error = ObjectName::parse(&format!(
        "v1/2020-01-01/{id}-{pool}-test-relay-metrics.jsonl.zst"
    ))
    .unwrap_err()
    .to_string();

    assert!(
        error.contains("v7") && error.contains(&id.to_string()),
        "got: {error}"
    );
}

/// Two objects stamped in the same millisecond are still two objects: nothing
/// but the id makes a key unique.
#[test]
fn two_names_stamped_at_one_instant_are_distinct() {
    let now = test_now();
    let first = object_name(&test_key(), now, Kind::Metrics);
    let second = object_name(&test_key(), now, Kind::Metrics);
    assert_ne!(first.to_key(), second.to_key());
}

#[test]
fn the_filesystem_archive_lists_what_it_stored() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(dir.path());
    let pool = pool_of(&test_key());
    let mut written: Vec<String> = [1, 2]
        .map(|step| name_at(pool, 1_755_000_000 + step).to_key())
        .to_vec();
    for key in &written {
        let path = dir.path().join(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"body").unwrap();
    }
    written.sort();
    let mut listed = archive.keys().unwrap();
    listed.sort();
    assert_eq!(listed, written);
}

/// What the download route answers with, and why it must be unchanged:
/// `archive::Bytes`.
#[test]
fn an_object_reads_back_as_the_bytes_that_were_stored() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(dir.path());
    let key = test_key();
    let body = b"the compressed, signed body";
    let stored = stored_submission(
        object_name(&key, test_now(), Kind::Metrics),
        seal(&key, &envelope_for(&key, 3)).1,
        body,
    );
    archive.store(&stored).unwrap();

    assert_eq!(read_object(&archive, &stored.object_key()).unwrap(), body);
}

/// A key the archive does not hold is an error naming it, not an empty body a
/// developer would take for an empty submission.
#[test]
fn bytes_for_a_key_the_archive_does_not_hold_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(dir.path());
    let missing = name_at(pool_of(&test_key()), 1_755_000_000).to_key();

    let error = read_object(&archive, &missing).unwrap_err().to_string();

    assert!(error.contains(&missing), "got: {error}");
}

/// The guard `Bytes for FilesystemArchive` states: a key is parsed before it
/// is joined to the root. `v1/../secret` has the three segments a key has and
/// would read a file outside the archive.
#[test]
fn bytes_for_a_key_that_climbs_out_of_the_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archive");
    // The schema folder a live archive always has: path resolution walks every
    // component, so `..` only climbs out of a directory that exists.
    std::fs::create_dir_all(root.join(metsuke_server::archive::KEY_PREFIX)).unwrap();
    std::fs::write(dir.path().join("secret"), b"not the archive's").unwrap();
    let archive = FilesystemArchive::new(&root);

    let error = read_object(&archive, "v1/../secret")
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("is not a v1 archive object key"),
        "the guard must be what refused it, got: {error}"
    );
}

/// The half of `List::for_each_key` its callers rely on: a visitor that fails
/// stops the walk, and its error survives rather than becoming an
/// `ArchiveError`.
#[test]
fn a_visitor_that_fails_stops_the_listing_with_its_own_error() {
    #[derive(Debug, thiserror::Error)]
    enum Stopped {
        #[error(transparent)]
        Archive(#[from] metsuke_server::archive::ArchiveError),
        #[error("the second key is one too many")]
        Enough,
    }
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(dir.path());
    let pool = pool_of(&test_key());
    for step in [1, 2, 3] {
        let path = dir
            .path()
            .join(name_at(pool, 1_755_000_000 + step).to_key());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"body").unwrap();
    }
    let mut visited = 0;
    let stopped = archive.for_each_key(|_| {
        visited += 1;
        match visited {
            1 => Ok(()),
            _ => Err(Stopped::Enough),
        }
    });
    assert!(matches!(stopped, Err(Stopped::Enough)), "got: {stopped:?}");
    assert_eq!(visited, 2, "the walk must stop at the refusal");
}

/// Listing a root that was never written to is an empty archive, not a
/// failure: the rebuild must be runnable before the first upload.
#[test]
fn listing_an_archive_that_does_not_exist_yet_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("not-created"));
    assert!(archive.keys().unwrap().is_empty());
}

#[test]
fn listing_a_root_that_is_not_a_directory_fails_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archive-is-a-file");
    std::fs::write(&root, b"not a directory").unwrap();
    let error = FilesystemArchive::new(&root)
        .keys()
        .unwrap_err()
        .to_string();
    assert!(error.contains("archive-is-a-file"), "got: {error}");
}

proptest! {
    /// Any name the server can hold roundtrips: the property the rebuild
    /// depends on across every pool, agent, instant and kind.
    #[test]
    fn every_name_roundtrips_through_its_key(
        seed: [u8; 32],
        agent in "[a-z0-9]{1,12}",
        unix in 0u64..4_000_000_000,
        logs: bool,
    ) {
        let name = ObjectName {
            id: Uuid::new_v7(Timestamp::from_unix(NoContext, unix, 0)),
            pool_id: pool_of(&SigningKey::from_bytes(&seed)),
            agent_id: AgentId::parse(&agent).unwrap(),
            kind: if logs { Kind::Logs } else { Kind::Metrics },
        };
        prop_assert_eq!(ObjectName::parse(&name.to_key()).unwrap(), name);
    }
}
