//! Rebuilding the index (see `rebuild`). What it must reconstruct is the
//! submission rows the developer listing serves, because the bucket is the only
//! place they can come back from.

use metsuke_server::archive::{FilesystemArchive, Kind, List, ObjectName, Store};
use metsuke_server::index::Index;
use metsuke_server::rebuild::{EmptyArchive, RebuildError, rebuild};
use metsuke_wire::envelope::SigningKey;

mod support;
use support::{
    FailingArchive, envelope_for, index_store, object_name, other_key, seal, stored_submission,
    test_key, test_now,
};

/// An archive holding one object per (pool, step), stored through the same path
/// an accepted upload takes.
fn archive_of(dir: &std::path::Path, objects: &[(&SigningKey, i64)]) -> FilesystemArchive {
    let archive = FilesystemArchive::new(dir);
    for (key, step) in objects {
        let received = test_now() + time::Duration::seconds(*step);
        archive
            .store(&stored_submission(
                key,
                object_name(key, received, Kind::Metrics),
                seal(key, &envelope_for(key, 1)).1,
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
    index: &mut Index,
) -> Result<metsuke_server::rebuild::RebuiltIndex, RebuildError> {
    rebuild(archive, index, EmptyArchive::Refuse)
}

/// The listing needs every object: a rebuild that recorded some of them would
/// leave a developer pull blind to the rest of the corpus.
#[test]
fn every_listed_object_becomes_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = (test_key(), other_key());
    let archive = archive_of(
        &dir.path().join("archive"),
        &[(&one, 1), (&one, 9), (&two, 3)],
    );
    let mut index = index_store(dir.path());

    let summary = rebuild_of(&archive, &mut index).unwrap();

    assert_eq!(summary.objects, 3);
    let listed: Vec<String> = index
        .submissions("", "", support::nonzero_u32(100))
        .unwrap()
        .objects
        .iter()
        .map(ObjectName::to_key)
        .collect();
    let mut expected: Vec<String> = archive.keys().unwrap();
    expected.sort();
    assert_eq!(listed, expected);
}

/// The bucket is the source of truth, so rebuilding over an index that already
/// holds the rows changes nothing.
#[test]
fn rebuilding_twice_leaves_the_same_rows() {
    let dir = tempfile::tempdir().unwrap();
    let archive = archive_of(&dir.path().join("archive"), &[(&test_key(), 1)]);
    let mut index = index_store(dir.path());
    rebuild_of(&archive, &mut index).unwrap();

    rebuild_of(&archive, &mut index).unwrap();

    assert_eq!(
        index
            .submissions("", "", support::nonzero_u32(100))
            .unwrap()
            .objects
            .len(),
        1
    );
}

/// The ambiguity `EmptyArchive` exists for, refused by default.
#[test]
fn an_empty_archive_refuses_the_rebuild_rather_than_indexing_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("not-the-root-you-meant"));
    let mut index = index_store(dir.path());
    let error = rebuild_of(&archive, &mut index).unwrap_err().to_string();
    assert!(error.contains("no objects"), "got: {error}");
}

/// The way past, for the server that has genuinely never accepted anything.
#[test]
fn an_empty_archive_indexes_nothing_when_the_operator_says_it_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    let mut index = index_store(dir.path());
    let summary = rebuild(&archive, &mut index, EmptyArchive::Accept).unwrap();
    assert_eq!(summary.objects, 0);
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
    let mut index = index_store(dir.path());
    let error = rebuild_of(&archive, &mut index).unwrap_err().to_string();
    assert!(error.contains("stray-object"), "got: {error}");
}

/// A listing that fails is not an empty one: it says nothing about the corpus,
/// so the rebuild stops whatever the operator said about emptiness.
#[test]
fn an_unreadable_archive_fails_the_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut index = index_store(dir.path());
    let unlistable = FailingArchive {
        reason: "the bucket said no",
    };
    for empty in [EmptyArchive::Refuse, EmptyArchive::Accept] {
        let error = rebuild(&unlistable, &mut index, empty)
            .unwrap_err()
            .to_string();
        assert!(error.contains("the bucket said no"), "got: {error}");
    }
}
