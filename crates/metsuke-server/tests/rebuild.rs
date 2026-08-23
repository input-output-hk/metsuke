//! Rebuilding the index (see `rebuild`). What it must reconstruct is the
//! replay counter state, because that is the only thing a lost index would let
//! an attacker replay past.

use metsuke_server::archive::{FilesystemArchive, ObjectName, Store};
use metsuke_server::counters::CounterStore;
use metsuke_server::rebuild::{EmptyArchive, RebuildError, SeededPool, rebuild};
use metsuke_wire::envelope::{PoolId, SigningKey};
use time::OffsetDateTime;

mod support;
use support::{
    FailingArchive, counter_store, envelope_for, other_key, pool_of, seal, stored_submission,
    test_key, test_now,
};

/// An archive holding one object per (pool, counter), stored through the same
/// path an accepted upload takes.
fn archive_of(dir: &std::path::Path, objects: &[(&SigningKey, u64)]) -> FilesystemArchive {
    let archive = FilesystemArchive::new(dir);
    for (key, counter) in objects {
        archive
            .store(&stored_submission(
                key,
                *counter,
                test_now() + time::Duration::seconds(*counter as i64),
                seal(key, &envelope_for(key, *counter)).1,
                b"body",
            ))
            .unwrap();
    }
    archive
}

/// Every rebuild here reads an archive it just wrote, so an empty listing is
/// the mistake `EmptyArchive::Refuse` is for.
fn rebuild_of(
    archive: &FilesystemArchive,
    counters: &mut CounterStore,
) -> Result<metsuke_server::rebuild::RebuiltIndex, RebuildError> {
    rebuild(archive, counters, EmptyArchive::Refuse)
}

fn last(counters: &CounterStore, pool: PoolId) -> Option<u64> {
    counters.last_counter(pool).unwrap()
}

#[test]
fn each_pools_counter_comes_back_at_its_highest_stored_object() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = (test_key(), other_key());
    let archive = archive_of(
        &dir.path().join("archive"),
        &[(&one, 1), (&one, 9), (&one, 4), (&two, 3)],
    );
    let mut counters = counter_store(dir.path());
    let summary = rebuild_of(&archive, &mut counters).unwrap();

    assert_eq!(summary.objects, 4);
    assert_eq!(last(&counters, pool_of(&one)), Some(9));
    assert_eq!(last(&counters, pool_of(&two)), Some(3));
    assert_eq!(summary.pools.len(), 2);
}

#[test]
fn a_rebuild_never_lowers_a_counter_it_already_has() {
    let dir = tempfile::tempdir().unwrap();
    let key = test_key();
    let archive = archive_of(&dir.path().join("archive"), &[(&key, 2)]);
    let mut counters = counter_store(dir.path());
    rebuild_of(&archive, &mut counters).unwrap();

    // A live server accepted a newer batch than the listing shows.
    let ahead = archive_of(&dir.path().join("ahead"), &[(&key, 20)]);
    rebuild_of(&ahead, &mut counters).unwrap();
    rebuild_of(&archive, &mut counters).unwrap();
    assert_eq!(last(&counters, pool_of(&key)), Some(20));
}

/// The ambiguity `EmptyArchive` exists for, refused by default.
#[test]
fn an_empty_archive_refuses_the_rebuild_rather_than_seeding_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("not-the-root-you-meant"));
    let mut counters = counter_store(dir.path());
    let error = rebuild_of(&archive, &mut counters).unwrap_err().to_string();
    assert!(error.contains("no objects"), "got: {error}");
}

/// The way past, for the server that has genuinely never accepted anything.
#[test]
fn an_empty_archive_indexes_nothing_when_the_operator_says_it_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    let mut counters = counter_store(dir.path());
    let summary = rebuild(&archive, &mut counters, EmptyArchive::Accept).unwrap();
    assert_eq!(summary.objects, 0);
    assert!(summary.pools.is_empty());
}

/// A key nothing can parse means the bucket holds an object this server did
/// not write. Skipping it silently would rebuild an index that is quietly
/// short of the corpus.
#[test]
fn an_unparseable_key_fails_the_rebuild_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archive");
    let archive = archive_of(&root, &[(&test_key(), 1)]);
    std::fs::write(root.join("v1/stray-object"), b"?").unwrap();
    let mut counters = counter_store(dir.path());
    let error = rebuild_of(&archive, &mut counters).unwrap_err().to_string();
    assert!(error.contains("stray-object"), "got: {error}");
}

/// A listing that fails is not an empty one: it says nothing about the corpus,
/// so the rebuild stops whatever the operator said about emptiness.
#[test]
fn an_unreadable_archive_fails_the_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut counters = counter_store(dir.path());
    let unlistable = FailingArchive {
        reason: "the bucket said no",
    };
    for empty in [EmptyArchive::Refuse, EmptyArchive::Accept] {
        let error = rebuild(&unlistable, &mut counters, empty)
            .unwrap_err()
            .to_string();
        assert!(error.contains("the bucket said no"), "got: {error}");
    }
}

/// `pools` is what the operator reads the rebuild off, so it names the object
/// the counter came from — the newest one, not the rebuild's own clock.
#[test]
fn the_summary_reports_each_pools_newest_object() {
    let dir = tempfile::tempdir().unwrap();
    let key = test_key();
    let archive = archive_of(&dir.path().join("archive"), &[(&key, 1), (&key, 5)]);
    let mut counters = counter_store(dir.path());
    let summary = rebuild_of(&archive, &mut counters).unwrap();
    let newest: OffsetDateTime = test_now() + time::Duration::seconds(5);
    assert_eq!(
        summary.pools,
        vec![SeededPool {
            newest: ObjectName {
                pool_id: pool_of(&key),
                counter: 5,
                timestamp: newest,
            },
            seeded: true,
        }]
    );
}

/// A pool the index already knows better than the listing did not get its
/// state from this run, and the summary must not read as though it did.
#[test]
fn a_pool_the_index_is_already_ahead_of_is_reported_as_such() {
    let dir = tempfile::tempdir().unwrap();
    let key = test_key();
    let ahead = archive_of(&dir.path().join("ahead"), &[(&key, 20)]);
    let behind = archive_of(&dir.path().join("behind"), &[(&key, 2)]);
    let mut counters = counter_store(dir.path());
    assert!(rebuild_of(&ahead, &mut counters).unwrap().pools[0].seeded);

    let summary = rebuild_of(&behind, &mut counters).unwrap();
    assert!(!summary.pools[0].seeded);
    assert_eq!(summary.pools[0].newest.counter, 2);
    assert_eq!(last(&counters, pool_of(&key)), Some(20));
}
