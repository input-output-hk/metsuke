//! Index tests (metsuke-4zo.10): what the developer listing reads, and that a
//! row nothing can parse fails the page rather than shortening it.

use metsuke_server::archive::{Kind, ObjectName};
use metsuke_server::index::Index;
use metsuke_wire::envelope::{AgentId, PoolId};

mod support;
use support::{index_store, nonzero_u32};
use uuid::{NoContext, Timestamp, Uuid};

/// An object of `pool` received `step` seconds past the epoch, so its key
/// sorts by `step`.
fn object(pool: PoolId, step: u64) -> ObjectName {
    ObjectName {
        id: Uuid::new_v7(Timestamp::from_unix(NoContext, step, 0)),
        pool_id: pool,
        agent_id: AgentId::parse("test-relay").unwrap(),
        kind: Kind::Metrics,
    }
}

/// The listing filters and the bound the developer endpoint passes.
fn listed(index: &Index, prefix: &str, after: &str) -> Vec<String> {
    index
        .submissions(prefix, after, nonzero_u32(100))
        .unwrap()
        .objects
        .iter()
        .map(ObjectName::to_key)
        .collect()
}

fn pool(index: u8) -> PoolId {
    let key = metsuke_wire::envelope::SigningKey::from_bytes(&[index; 32]);
    PoolId::from_cold_key(&key.verifying_key())
}

/// The listing is what a developer pull reads instead of scanning the bucket
/// (ADR 0005), so a stored object has to reach it.
#[test]
fn a_recorded_object_is_listed_back() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let name = object(pool(1), 7);

    index.record(&name).unwrap();

    assert_eq!(listed(&index, "", ""), vec![name.to_key()]);
}

/// A developer syncing the archive asks by key prefix, which the time-major
/// key makes a day filter.
#[test]
fn a_prefix_lists_only_the_keys_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let epoch_day = object(pool(1), 1);
    let later_day = object(pool(1), 1_755_000_000);
    index.record(&epoch_day).unwrap();
    index.record(&later_day).unwrap();

    assert_eq!(
        listed(&index, "v1/2025-08-12/", ""),
        vec![later_day.to_key()]
    );
}

/// `after` is the page cursor: handing back the last key of a page must
/// answer the next one, or a developer paging the archive loops.
#[test]
fn after_resumes_at_the_key_that_follows_it() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let names: Vec<ObjectName> = (1..=3).map(|step| object(pool(1), step)).collect();
    for name in &names {
        index.record(name).unwrap();
    }

    assert_eq!(
        listed(&index, "", &names[1].to_key()),
        vec![names[2].to_key()],
        "the cursor key itself is behind the page it names"
    );
}

/// A page cut off by the bound says so. A developer reading a short page as
/// the whole archive would miss every object past it.
#[test]
fn a_listing_over_the_limit_is_reported_as_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let names: Vec<ObjectName> = (1..=3).map(|step| object(pool(1), step)).collect();
    for name in &names {
        index.record(name).unwrap();
    }

    let page = index.submissions("", "", nonzero_u32(2)).unwrap();
    assert!(page.truncated);
    assert_eq!(
        page.objects
            .iter()
            .map(ObjectName::to_key)
            .collect::<Vec<_>>(),
        vec![names[0].to_key(), names[1].to_key()],
        "a truncated page carries the bound's worth, not one more"
    );

    let whole = index.submissions("", "", nonzero_u32(3)).unwrap();
    assert!(!whole.truncated, "an exact fit is not truncated");
    assert_eq!(whole.objects.len(), 3);
}

/// A row the listing cannot parse fails it rather than shortening the page
/// (`IndexError::ObjectName`).
#[test]
fn a_key_the_listing_cannot_parse_fails_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite");
    let index = Index::open(&path).unwrap();
    index.record(&object(pool(1), 1)).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO submissions (object_key) VALUES ('v1/not-a-key')",
            [],
        )
        .unwrap();

    let Err(error) = index.submissions("", "", nonzero_u32(100)) else {
        panic!("an unparseable key must not be dropped from the page");
    };

    assert!(
        error.to_string().contains("not-a-key"),
        "the error must name the key it could not read, got {error}"
    );
}

/// `rebuild-index` walks the whole bucket, so it re-records keys the index
/// already holds. That is a no-op, not a failure.
#[test]
fn recording_the_same_object_twice_leaves_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_store(dir.path());
    let name = object(pool(1), 1);

    index.record(&name).unwrap();
    index.record(&name).unwrap();

    assert_eq!(listed(&index, "", ""), vec![name.to_key()]);
}

// A database written by a newer build is refused: running it half-understood
// would silently drop whatever the newer schema records.
#[test]
fn a_database_from_a_newer_build_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite");
    Index::open(&path).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99u32)
        .unwrap();

    let Err(error) = Index::open(&path) else {
        panic!("opening a newer schema must fail");
    };

    assert!(
        error.to_string().contains("99"),
        "the error must name the version found, got {error}"
    );
}
