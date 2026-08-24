//! What the Calidus half asks a db-sync, and the genesis value that decides
//! which of its answers count (ADR 0008).

use metsuke_server::calidus::{Directory, DirectoryError};
use metsuke_server::dbsync::{DbSync, GenesisError, security_parameter};

mod support;
use support::{
    TEST_SECURITY_PARAMETER, calidus_config, fake_psql, nonzero_u32, pool_of, psql_answers,
    psql_fails, query_csv, recorded_argv, recorded_script, registered_pool, registration, test_key,
};

/// A `DbSync` over a `psql` that replays `csv`.
fn dbsync(dir: &std::path::Path, csv: &str) -> DbSync {
    psql_answers(dir, csv);
    let genesis = dir.join("shelley-genesis.json");
    std::fs::write(
        &genesis,
        format!("{{\"securityParam\": {TEST_SECURITY_PARAMETER}}}"),
    )
    .unwrap();
    DbSync::new(
        calidus_config(&fake_psql(), dir, &genesis),
        nonzero_u32(TEST_SECURITY_PARAMETER),
    )
}

// Acceptance: the answer a real db-sync gave to the shipped query is the blob
// the verifier reads. Recorded by scripts/record-calidus-fixtures.sh.
#[test]
fn the_recorded_query_answer_is_the_registration_that_was_submitted() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dbsync(dir.path(), &query_csv());

    let rows = directory.registrations(registered_pool()).unwrap();

    assert_eq!(rows, vec![registration("on-chain-nonce-1-key-a")]);
}

/// What the query is run with: the connection the config names, the pool as a
/// bound variable, and k from the genesis rather than from the query text.
#[test]
fn psql_is_run_against_the_configured_database_with_the_pool_and_k_bound() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dbsync(dir.path(), &query_csv());
    directory.registrations(registered_pool()).unwrap();

    // One argument per line, as the double writes them.
    let argv = std::fs::read_to_string(recorded_argv(dir.path())).unwrap();
    let after = |flag: &str| {
        let mut lines = argv.lines().skip_while(|line| *line != flag);
        lines.next();
        lines.next().unwrap_or_default().to_string()
    };
    assert_eq!(after("--dbname"), "cexplorer");
    assert_eq!(after("--username"), "metsuke_ro");
    assert_eq!(after("--host"), dir.path().display().to_string());
    assert!(argv.lines().any(|line| line == "--csv"), "got: {argv}");
    assert!(
        argv.lines().any(|line| line
            == format!(
                "scope=0x{}",
                metsuke_wire::hex::encode(registered_pool().as_hash())
            )),
        "got: {argv}"
    );
    assert!(
        argv.lines()
            .any(|line| line == format!("k={TEST_SECURITY_PARAMETER}")),
        "got: {argv}"
    );
    assert_eq!(
        argv.lines().next(),
        Some("PGOPTIONS=-c statement_timeout=11s")
    );
    assert_eq!(
        argv.lines().nth(1),
        Some(format!("PGPASSFILE={}", dir.path().join("pgpass").display()).as_str()),
        "the password reaches psql by path, never as a value"
    );
}

// The shipped query has to reach psql as a script it reads, which is what
// binds the variables it names: tests/fixtures/psql.
#[test]
fn the_shipped_query_arrives_as_a_script_psql_reads_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dbsync(dir.path(), &query_csv());
    directory.registrations(registered_pool()).unwrap();

    let argv = std::fs::read_to_string(recorded_argv(dir.path())).unwrap();
    assert!(argv.lines().any(|line| line == "--file"), "got: {argv}");
    assert!(!argv.lines().any(|line| line == "--command"), "got: {argv}");
    assert_eq!(
        std::fs::read_to_string(recorded_script(dir.path())).unwrap(),
        include_str!("../src/registrations.sql"),
        "psql must be piped the query the server ships, variables unspliced"
    );
}

// A pool the query answers nothing for registered nothing, which is a real
// answer and not a failure.
#[test]
fn a_pool_with_no_rows_resolves_to_no_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dbsync(dir.path(), "");

    assert!(
        directory
            .registrations(pool_of(&test_key()))
            .unwrap()
            .is_empty()
    );
}

// A database that refused the query has not said the pool registered nothing,
// and treating it as such would strip a pool of its upload rights.
#[test]
fn a_psql_that_fails_is_unavailable_rather_than_an_empty_answer() {
    let dir = tempfile::tempdir().unwrap();
    psql_fails(dir.path(), "FATAL: role does not exist");
    let genesis = dir.path().join("shelley-genesis.json");
    std::fs::write(&genesis, "{\"securityParam\": 1}").unwrap();
    let directory = DbSync::new(
        calidus_config(&fake_psql(), dir.path(), &genesis),
        nonzero_u32(1),
    );

    let DirectoryError::Unavailable { reason, .. } =
        directory.registrations(registered_pool()).unwrap_err();

    assert!(reason.contains("role does not exist"), "got: {reason}");
}

// The column is `encode(tm.bytes, 'hex')`, so a row that is not hex is psql or
// the query misbehaving, not a stranger's transaction: dropping it would report
// one registration fewer than the pool has.
#[test]
fn a_row_that_is_not_hex_is_unavailable_rather_than_a_dropped_row() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dbsync(dir.path(), "zz");

    let DirectoryError::Unavailable { reason, .. } =
        directory.registrations(registered_pool()).unwrap_err();

    assert!(reason.contains("not hex"), "got: {reason}");
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
