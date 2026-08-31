//! The Key Roster as the server reads it: loaded loudly, re-read when the file
//! changes, and never emptied by a failed re-read (ADR 0011).

use std::path::Path;

use metsuke_server::roster::Roster;
use metsuke_wire::envelope::{PoolId, SubmissionKey};
use metsuke_wire::hex;

mod support;
use support::{pool_of, test_key, test_leios_key};

/// A roster file naming `keys` for `pool`, at a fixed chain position.
fn write_roster(path: &Path, pool: PoolId, keys: &[&SubmissionKey]) {
    let listed: Vec<String> = keys
        .iter()
        .map(|key| format!("\"{}\"", key.public_key_hex()))
        .collect();
    let text = format!(
        r#"{{"epoch": 42, "slot": 907200, "pools": {{"{pool}": [{}]}}}}"#,
        listed.join(", "),
        pool = hex::encode(pool.as_hash())
    );
    std::fs::write(path, text).expect("the roster writes");
}

fn public_key(key: &SubmissionKey) -> metsuke_wire::leios::LeiosPublicKey {
    metsuke_wire::leios::LeiosPublicKey::from_bytes(
        &hex::decode::<96>(&key.public_key_hex()).expect("96 bytes of hex"),
    )
    .expect("a signing key's own public half")
}

#[test]
fn a_roster_registers_the_key_it_lists_for_the_pool_it_lists_it_under() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    let key = test_leios_key(1);
    write_roster(&path, pool, &[&key]);

    let roster = Roster::load(&path).unwrap();

    assert!(roster.registers(pool, &public_key(&key)));
    assert_eq!(roster.position(), (42, 907200));
}

#[test]
fn a_key_the_roster_does_not_list_for_that_pool_is_not_registered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    write_roster(&path, pool, &[&test_leios_key(1)]);
    let roster = Roster::load(&path).unwrap();

    // The right key under the wrong pool, and the wrong key under the right
    // one. Neither is a registration.
    assert!(!roster.registers(pool, &public_key(&test_leios_key(2))));
    assert!(!roster.registers(
        pool_of(&support::other_key()),
        &public_key(&test_leios_key(1))
    ));
}

/// The writer lists the registered key and the announced one together, which is
/// what makes a rotation land before the epoch boundary (ADR 0011).
#[test]
fn a_pool_may_have_more_than_one_key_listed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    write_roster(&path, pool, &[&test_leios_key(1), &test_leios_key(2)]);

    let roster = Roster::load(&path).unwrap();

    assert!(roster.registers(pool, &public_key(&test_leios_key(1))));
    assert!(roster.registers(pool, &public_key(&test_leios_key(2))));
}

/// The rotation, as the server meets it: the file is rewritten under a running
/// server and the next submission is judged against what it now says.
#[test]
fn a_rewritten_roster_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    write_roster(&path, pool, &[&test_leios_key(1)]);
    let roster = Roster::load(&path).unwrap();
    assert!(roster.registers(pool, &public_key(&test_leios_key(1))));

    write_roster(&path, pool, &[&test_leios_key(2)]);

    assert!(roster.registers(pool, &public_key(&test_leios_key(2))));
    assert!(
        !roster.registers(pool, &public_key(&test_leios_key(1))),
        "the retired key is refused as soon as the roster stops listing it"
    );
}

/// A writer caught halfway through must not empty the roster: refusing every
/// pool is a worse answer than answering from the file that did parse.
#[test]
fn a_roster_that_stops_parsing_keeps_the_one_already_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    write_roster(&path, pool, &[&test_leios_key(1)]);
    let roster = Roster::load(&path).unwrap();

    std::fs::write(&path, "{\"epoch\": 43, \"slot\"").expect("half a file writes");

    assert!(roster.registers(pool, &public_key(&test_leios_key(1))));
    assert_eq!(
        roster.position(),
        (42, 907200),
        "still the roster that read"
    );
}

#[test]
fn a_roster_that_cannot_be_read_at_startup_is_a_startup_failure() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nothing.json");

    let error = Roster::load(&missing).unwrap_err().to_string();

    assert!(error.contains("nothing.json"), "got: {error}");
}

#[test]
fn a_roster_key_that_is_not_a_key_is_refused_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.json");
    let pool = pool_of(&test_key());
    let text = format!(
        r#"{{"epoch": 1, "slot": 2, "pools": {{"{pool}": ["{}"]}}}}"#,
        "ab".repeat(96),
        pool = hex::encode(pool.as_hash())
    );
    std::fs::write(&path, text).unwrap();

    let error = Roster::load(&path).unwrap_err().to_string();

    assert!(error.contains(&pool.to_bech32()), "got: {error}");
}
