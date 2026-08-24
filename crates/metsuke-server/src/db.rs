//! The one way this server reads a chain: a query on a Postgres connection.
//! Why the wire protocol and not a `psql` subprocess: ADR 0009.

use std::path::Path;
use std::time::Duration;

use postgres::types::FromSql;
use postgres::{NoTls, Row};

/// What a caller binds a value as. Re-exported so a query site names one crate.
pub use postgres::types::ToSql;

/// A query's bound values, in the order the text numbers them.
pub type Parameters<'a> = [&'a (dyn ToSql + Sync)];

/// The db-sync a config names, opened per query. Connecting costs one round
/// trip on a unix socket and only happens on a cache miss, which buys a `&self`
/// caller with no pool, no lock and no reconnect state to get wrong.
pub struct Connection<'a> {
    pub socket_dir: &'a Path,
    pub dbname: &'a str,
    pub role: &'a str,
    pub query_timeout_secs: u64,
    /// `None` where the connection needs no password: a peer-authenticated
    /// socket does not.
    pub password_file: Option<&'a Path>,
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("cannot read the password file {path}: {source}")]
    NoPassword {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot reach {dbname} on {socket_dir}: {source}")]
    CannotConnect {
        dbname: String,
        socket_dir: String,
        #[source]
        source: postgres::Error,
    },
    #[error("{dbname} refused the query: {source}")]
    Refused {
        dbname: String,
        #[source]
        source: postgres::Error,
    },
    #[error("no {column} of the type the query names: {source}")]
    Column {
        column: String,
        #[source]
        source: postgres::Error,
    },
}

/// One column of one row. `Row::get` would panic on a schema that no longer
/// matches the query, which is a db-sync to fix rather than a thread to lose.
pub fn column<'a, T: FromSql<'a>>(row: &'a Row, name: &str) -> Result<T, DbError> {
    row.try_get(name).map_err(|source| DbError::Column {
        column: name.to_string(),
        source,
    })
}

impl Connection<'_> {
    /// The rows the query answered. Parameters are bound, so the text is fixed
    /// at compile time and no value it carries can reach the parser.
    pub fn query(&self, text: &str, parameters: &Parameters<'_>) -> Result<Vec<Row>, DbError> {
        let named = |path: &Path| path.display().to_string();
        let options = format!("-c statement_timeout={}s", self.query_timeout_secs);
        let mut config = postgres::Config::new();
        config
            .host_path(self.socket_dir)
            .dbname(self.dbname)
            .user(self.role)
            .options(&options)
            .connect_timeout(Duration::from_secs(self.query_timeout_secs));
        if let Some(path) = self.password_file {
            let password = std::fs::read_to_string(path).map_err(|source| DbError::NoPassword {
                path: named(path),
                source,
            })?;
            config.password(password.trim_end_matches(['\r', '\n']));
        }
        let mut client = config
            .connect(NoTls)
            .map_err(|source| DbError::CannotConnect {
                dbname: self.dbname.to_string(),
                socket_dir: named(self.socket_dir),
                source,
            })?;
        let rows = client
            .query(text, parameters)
            .map_err(|source| DbError::Refused {
                dbname: self.dbname.to_string(),
                source,
            })?;
        // Best-effort: the rows are already in hand, and a failed goodbye is
        // not an answer the caller should lose.
        let _ = client.close();
        Ok(rows)
    }
}
