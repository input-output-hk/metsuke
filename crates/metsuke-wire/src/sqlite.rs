//! Schema migration, shared by the agent spool and the server's counter
//! store because both are one-file SQLite databases opened by a binary that
//! may be older than the file.

use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database is at schema version {found}, this build knows {known}")]
    FromTheFuture { found: u32, known: u32 },
}

/// Apply the migrations the database has not seen. `migrations` holds one
/// entry per released schema version and `user_version` counts how many have
/// run, so an old database gets exactly the missing suffix — and a database
/// written by a newer build is refused rather than half-understood.
pub fn migrate(conn: &Connection, migrations: &[&str]) -> Result<(), MigrateError> {
    let applied: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if applied as usize > migrations.len() {
        return Err(MigrateError::FromTheFuture {
            found: applied,
            known: migrations.len() as u32,
        });
    }
    for (version, migration) in migrations.iter().enumerate().skip(applied as usize) {
        conn.execute_batch(migration)?;
        conn.pragma_update(None, "user_version", version as u32 + 1)?;
    }
    Ok(())
}
