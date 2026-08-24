//! The one way this server reads a chain: a query handed to `psql`. Why a
//! subprocess and not a Postgres client: ADR 0008 Consequences.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Whether the answer names its own columns. The applications query reads its
/// columns back by name; the registrations query prints one and no header.
#[derive(Debug, Clone, Copy)]
pub enum Rows {
    WithHeader,
    TuplesOnly,
}

/// A query against the database a config names.
pub struct Query<'a> {
    pub psql_path: &'a Path,
    pub socket_dir: &'a Path,
    pub dbname: &'a str,
    pub role: &'a str,
    pub query_timeout_secs: u64,
    /// A `.pgpass` reaching psql as `PGPASSFILE`, where the connection needs
    /// one: a peer-authenticated socket does not.
    pub password_file: Option<&'a Path>,
    /// Bound as psql variables rather than spliced into `text`, so every query
    /// stays fixed at compile time. Interpolated only into a script psql reads,
    /// which is why `text` goes in on stdin — recorded under
    /// tests/fixtures/psql.
    pub variables: &'a [(&'a str, String)],
    pub rows: Rows,
    pub text: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum PsqlError {
    #[error("cannot run {psql}: {source}")]
    CannotRun {
        psql: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{psql} exited {status}: {stderr}")]
    Failed {
        psql: String,
        status: String,
        stderr: String,
    },
    #[error("{psql} printed something that is not UTF-8: {source}")]
    NotUtf8 {
        psql: String,
        #[source]
        source: std::string::FromUtf8Error,
    },
}

impl Query<'_> {
    /// What psql printed. Nothing is inherited: PGPASSWORD, PGSERVICE and
    /// PGDATABASE would each reach a database the config does not name.
    /// PGPASSFILE is a path, so no password enters the environment
    /// (metsuke-4zo.50).
    pub fn run(&self) -> Result<String, PsqlError> {
        let psql = || self.psql_path.display().to_string();
        let mut command = Command::new(self.psql_path);
        command
            .env_clear()
            .env(
                "PGOPTIONS",
                format!("-c statement_timeout={}s", self.query_timeout_secs),
            )
            .args(["--csv", "--no-psqlrc", "--quiet"])
            .args([OsStr::new("--host"), self.socket_dir.as_os_str()])
            .args(["--dbname", self.dbname])
            .args(["--username", self.role]);
        if let Some(password_file) = self.password_file {
            command.env("PGPASSFILE", password_file);
        }
        if matches!(self.rows, Rows::TuplesOnly) {
            command.arg("--tuples-only");
        }
        for (name, value) in self.variables {
            command.args(["--set", &format!("{name}={value}")]);
        }
        let mut running = command
            .args(["--file", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| PsqlError::CannotRun {
                psql: psql(),
                source,
            })?;
        // Dropped at the end of the statement, which is the EOF psql waits for.
        let unwritten = running
            .stdin
            .take()
            .expect("a piped stdin")
            .write_all(self.text.as_bytes())
            .err();
        let printed = running
            .wait_with_output()
            .map_err(|source| PsqlError::CannotRun {
                psql: psql(),
                source,
            })?;
        // What psql said about its own failure, ahead of the broken pipe that
        // failure leaves behind.
        if !printed.status.success() {
            return Err(PsqlError::Failed {
                psql: psql(),
                status: printed.status.to_string(),
                stderr: String::from_utf8_lossy(&printed.stderr).trim().to_string(),
            });
        }
        if let Some(source) = unwritten {
            return Err(PsqlError::CannotRun {
                psql: psql(),
                source,
            });
        }
        String::from_utf8(printed.stdout).map_err(|source| PsqlError::NotUtf8 {
            psql: psql(),
            source,
        })
    }
}
