//! Schema migration, shared by the agent spool and the server's index
//! because both are one-file SQLite databases opened by a binary that may be
//! older than the file.

use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database is at schema version {found}, this build knows {known}")]
    FromTheFuture { found: u32, known: u32 },
}

/// One migration this call ran, and the rows it inserted, updated or deleted.
/// A migration that discards rows is data the caller's operator is losing, so
/// the count leaves this function rather than being counted again by a caller
/// that would have to know the migration's SQL to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// `user_version` after the migration ran: its 1-based place in
    /// `migrations`.
    pub version: u32,
    pub rows: u64,
}

/// Apply the migrations the database has not seen. `migrations` holds one
/// entry per released schema version and `user_version` counts how many have
/// run, so an old database gets exactly the missing suffix, and a database
/// written by a newer build is refused rather than half-understood.
///
/// One migration is one transaction, covering both its statements and the
/// version it claims. `user_version` lives in the database header and rolls
/// back with everything else, so a migration that is interrupted leaves a
/// database that migrates again rather than one that replays a half-applied
/// schema and refuses to open from then on. A migration must therefore not
/// open a transaction of its own.
pub fn migrate(conn: &Connection, migrations: &[&str]) -> Result<Vec<Applied>, MigrateError> {
    let applied: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if applied as usize > migrations.len() {
        return Err(MigrateError::FromTheFuture {
            found: applied,
            known: migrations.len() as u32,
        });
    }
    let mut ran = Vec::new();
    for (version, migration) in migrations.iter().enumerate().skip(applied as usize) {
        // `Connection::changes` reports the last statement of the batch alone,
        // and a migration is as many statements as it needs.
        let before = conn.total_changes();
        let transaction = conn.unchecked_transaction()?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", version as u32 + 1)?;
        transaction.commit()?;
        ran.push(Applied {
            version: version as u32 + 1,
            rows: conn.total_changes() - before,
        });
    }
    Ok(ran)
}
