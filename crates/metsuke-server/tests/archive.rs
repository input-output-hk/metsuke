//! Object naming, both ways. The key is the only copy of a submission's
//! identity that survives losing the index (ADR 0005), so writing it and
//! reading it back must be one bijection, and a key that is not one must say
//! so rather than parse into a plausible wrong pool.

use metsuke_server::archive::{FilesystemArchive, List, ObjectName};
use metsuke_wire::envelope::{PoolId, SigningKey};
use proptest::prelude::*;
use time::{OffsetDateTime, UtcOffset};

mod support;
use support::{pool_of, test_key};

fn name_at(pool_id: PoolId, counter: u64, unix: i64) -> ObjectName {
    ObjectName {
        pool_id,
        counter,
        timestamp: OffsetDateTime::from_unix_timestamp(unix).unwrap(),
    }
}

#[test]
fn a_key_carries_the_pool_the_day_and_the_counter() {
    let pool = pool_of(&test_key());
    let name = name_at(pool, 42, 1_755_000_000);
    assert_eq!(
        name.to_key(),
        format!("v1/{pool}/2025-08-12/1755000000-42.json.zst")
    );
}

#[test]
fn a_key_parses_back_to_the_name_that_wrote_it() {
    let name = name_at(pool_of(&test_key()), 42, 1_755_000_000);
    assert_eq!(ObjectName::parse(&name.to_key()).unwrap(), name);
}

#[test]
fn a_timestamp_in_another_offset_is_keyed_by_its_utc_day() {
    // 22:00 UTC, which is the next day at +02:00.
    let instant = OffsetDateTime::from_unix_timestamp(1_755_036_000).unwrap();
    let local = ObjectName {
        pool_id: pool_of(&test_key()),
        counter: 1,
        timestamp: instant.to_offset(UtcOffset::from_hms(2, 0, 0).unwrap()),
    };
    assert!(
        local.to_key().contains("/2025-08-12/"),
        "keyed under {}",
        local.to_key()
    );
    assert_eq!(ObjectName::parse(&local.to_key()).unwrap(), local);
}

/// The date folder and the leading timestamp are what make a bucket listing
/// readable by hand: sorted by key, a pool's uploads come out in order.
#[test]
fn keys_sort_by_day_then_by_timestamp() {
    let pool = pool_of(&test_key());
    let mut keys = [
        name_at(pool, 3, 1_755_100_000).to_key(),
        name_at(pool, 1, 1_754_000_000).to_key(),
        name_at(pool, 2, 1_755_000_000).to_key(),
    ];
    keys.sort();
    assert_eq!(
        keys.map(|key| ObjectName::parse(&key).unwrap().counter),
        [1, 2, 3]
    );
}

#[test]
fn a_key_that_is_not_an_archive_object_is_refused() {
    for key in [
        "",
        "duck.jpg",
        // The right shape under the wrong schema version.
        "v2/pool1nope/2025-08-12/1755000000-1.json.zst",
        // A pool id that is not bech32.
        "v1/pool1nope/2025-08-12/1755000000-1.json.zst",
        // Uncompressed, so not a body this server ever wrote.
        "v1/pool1nope/2025-08-12/1755000000-1.json",
    ] {
        assert!(
            ObjectName::parse(key).is_err(),
            "{key:?} must not parse as an object name"
        );
    }
}

#[test]
fn a_malformed_key_names_itself_and_what_is_wrong() {
    let pool = pool_of(&test_key());
    let error = ObjectName::parse(&format!("v1/{pool}/2025-08-12/1755000000-x.json.zst"))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("counter") && error.contains(&pool.to_bech32()),
        "got: {error}"
    );
}

#[test]
fn a_key_whose_folder_contradicts_its_timestamp_is_refused() {
    let pool = pool_of(&test_key());
    let error = ObjectName::parse(&format!("v1/{pool}/2020-01-01/1755000000-1.json.zst"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("2020-01-01"), "got: {error}");
}

#[test]
fn the_filesystem_archive_lists_what_it_stored() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(dir.path());
    let pool = pool_of(&test_key());
    let written: Vec<String> = [1, 2]
        .map(|counter| name_at(pool, counter, 1_755_000_000 + counter as i64).to_key())
        .to_vec();
    for key in &written {
        let path = dir.path().join(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"body").unwrap();
    }
    let mut listed = archive.keys().unwrap();
    listed.sort();
    assert_eq!(listed, written);
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
    for counter in [1, 2, 3] {
        let path = dir
            .path()
            .join(name_at(pool, counter, 1_755_000_000 + counter as i64).to_key());
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
    /// depends on across every pool, counter, instant and sender offset.
    #[test]
    fn every_name_roundtrips_through_its_key(
        seed: [u8; 32],
        counter: u64,
        unix in 0i64..4_000_000_000,
        offset_hours in -12i8..15,
    ) {
        let name = ObjectName {
            pool_id: pool_of(&SigningKey::from_bytes(&seed)),
            counter,
            timestamp: OffsetDateTime::from_unix_timestamp(unix)
                .unwrap()
                .to_offset(UtcOffset::from_hms(offset_hours, 0, 0).unwrap()),
        };
        prop_assert_eq!(ObjectName::parse(&name.to_key()).unwrap(), name);
    }
}
