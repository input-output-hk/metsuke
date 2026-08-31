use metsuke_wire::sqlite::{MigrateError, migrate};
use rusqlite::Connection;

const MIGRATIONS: [&str; 2] = [
    "CREATE TABLE first (id INTEGER PRIMARY KEY)",
    "CREATE TABLE second (id INTEGER PRIMARY KEY)",
];

fn user_version(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn applies_every_migration_to_a_fresh_database() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn, &MIGRATIONS).unwrap();
    assert_eq!(user_version(&conn), 2);
    conn.execute_batch("SELECT id FROM first; SELECT id FROM second")
        .unwrap();
}

#[test]
fn applies_only_the_missing_suffix() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn, &MIGRATIONS[..1]).unwrap();
    migrate(&conn, &MIGRATIONS).unwrap();
    assert_eq!(user_version(&conn), 2);
}

#[test]
fn is_a_no_op_when_every_migration_has_run() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn, &MIGRATIONS).unwrap();
    // A second run would fail on `CREATE TABLE` if it replayed anything.
    migrate(&conn, &MIGRATIONS).unwrap();
    assert_eq!(user_version(&conn), 2);
}

// A migration is all or nothing, version included. Applying the statements
// and then claiming the version was two steps, so a process that died between
// them left a database carrying the new schema while still counting the old
// one: the next start replays the migration, the CREATE TABLE fails against
// the table it already made, and the binary never opens that file again.
#[test]
fn a_migration_that_fails_leaves_the_database_as_it_was() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn, &MIGRATIONS[..1]).unwrap();
    // Makes a table, then fails, exactly as a half-applied submission would.
    let broken = [
        MIGRATIONS[0],
        "CREATE TABLE second (id INTEGER PRIMARY KEY);
         INSERT INTO a_table_that_is_not_there (id) VALUES (1);",
    ];
    assert!(migrate(&conn, &broken).is_err());

    assert_eq!(user_version(&conn), 1, "the version must not have moved");
    assert!(
        conn.execute_batch("SELECT id FROM second").is_err(),
        "and the table the failed submission made must be gone"
    );
    // So the real migration still applies, which is the point.
    migrate(&conn, &MIGRATIONS).unwrap();
    assert_eq!(user_version(&conn), 2);
    conn.execute_batch("SELECT id FROM second").unwrap();
}

#[test]
fn refuses_a_database_from_a_newer_build() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn, &MIGRATIONS).unwrap();
    let error = migrate(&conn, &MIGRATIONS[..1]).unwrap_err();
    assert!(
        matches!(error, MigrateError::FromTheFuture { found: 2, known: 1 }),
        "{error}"
    );
}
