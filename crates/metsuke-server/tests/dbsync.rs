//! What the Calidus half asks a db-sync, and the genesis value that decides
//! which of its answers count (ADR 0008).
//!
//! The query itself is not exercised here: with bound parameters and a `bytea`
//! column there is no text to parse, and the only double that could answer the
//! wire protocol is a Postgres (ADR 0009). What the shipped SQL returns on a
//! real chain is the recorder's to prove: scripts/record-calidus-fixtures.sh.

use metsuke_server::calidus::{Directory, DirectoryError};
use metsuke_server::dbsync::{DbSync, GenesisError, security_parameter};

mod support;
use support::{
    TEST_SECURITY_PARAMETER, calidus_config, nonzero_u32, registered_pool, write_password,
};

// A db-sync that cannot be reached has not said the pool registered nothing,
// and treating it as such would strip a pool of its upload rights.
#[test]
fn a_database_that_cannot_be_reached_is_unavailable_rather_than_an_empty_answer() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = dir.path().join("shelley-genesis.json");
    std::fs::write(
        &genesis,
        format!("{{\"securityParam\": {TEST_SECURITY_PARAMETER}}}"),
    )
    .unwrap();
    write_password(dir.path());
    // An empty directory: no postgres has ever put a socket there.
    let directory = DbSync::new(
        calidus_config(dir.path(), &genesis),
        nonzero_u32(TEST_SECURITY_PARAMETER),
    );

    let DirectoryError::Unavailable { reason, .. } =
        directory.registrations(registered_pool()).unwrap_err();

    assert!(reason.contains("cexplorer"), "got: {reason}");
}

// k is a genesis parameter and nothing else answers it, so the file must be
// read rather than guessed at.
#[test]
fn the_security_parameter_comes_from_the_shelley_genesis() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = dir.path().join("shelley-genesis.json");
    std::fs::write(&genesis, "{\"securityParam\": 108, \"epochLength\": 21600}").unwrap();

    assert_eq!(security_parameter(&genesis).unwrap(), nonzero_u32(108));
}

#[test]
fn a_genesis_that_names_no_security_parameter_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = dir.path().join("shelley-genesis.json");
    std::fs::write(&genesis, "{\"epochLength\": 21600}").unwrap();

    assert!(matches!(
        security_parameter(&genesis),
        Err(GenesisError::NoSecurityParameter { .. })
    ));
    assert!(matches!(
        security_parameter(&dir.path().join("absent.json")),
        Err(GenesisError::Unreadable { .. })
    ));
}

// A securityParam that is there but is no block count is a different genesis to
// fix than one that names none, and neither may read as the other. Zero is one
// of them: it would count a registration in the tip block, leaving no depth to
// wait out.
#[test]
fn a_security_parameter_that_is_no_block_count_names_what_it_found() {
    let dir = tempfile::tempdir().unwrap();
    let written = |json: &str| {
        let genesis = dir.path().join(format!("{}.json", json.len()));
        std::fs::write(&genesis, format!("{{\"securityParam\": {json}}}")).unwrap();
        security_parameter(&genesis)
    };

    for json in ["\"432\"", "-1", "4294967296", "0"] {
        let error = written(json).unwrap_err();
        assert!(
            matches!(&error, GenesisError::NotABlockCount { found, .. } if found == json),
            "got: {error}"
        );
    }
}
