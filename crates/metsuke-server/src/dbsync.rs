//! The `Directory` behind the Calidus half: label-867 rows read out of the
//! Leios db-sync by running the shipped query through `psql`, and the security
//! parameter that says which of them are deep enough to count (ADR 0008).

use std::num::NonZeroU32;
use std::path::Path;

use metsuke_wire::envelope::PoolId;
use metsuke_wire::hex::{self, HexError};

use crate::calidus::{Directory, DirectoryError};
use crate::config::CalidusConfig;
use crate::psql::{Query, Rows};

/// The one query the server asks a chain. Its own file so the recorder runs the
/// same text the server does: scripts/record-calidus-fixtures.sh.
const QUERY: &str = include_str!("registrations.sql");

/// How the query names a pool: db-sync renders metadata byte strings as the
/// `0x`-prefixed hex the scope holds.
fn scope_of(pool_id: PoolId) -> String {
    format!("0x{}", hex::encode(pool_id.as_hash()))
}

/// The chain, over a `psql` the config names.
pub struct DbSync {
    config: CalidusConfig,
    security_parameter: NonZeroU32,
}

impl DbSync {
    pub fn new(config: CalidusConfig, security_parameter: NonZeroU32) -> Self {
        DbSync {
            config,
            security_parameter,
        }
    }
}

impl Directory for DbSync {
    fn registrations(&self, pool_id: PoolId) -> Result<Vec<Vec<u8>>, DirectoryError> {
        let unavailable = |reason: String| DirectoryError::Unavailable { pool_id, reason };
        let csv = Query {
            psql_path: self.config.psql_path.as_path(),
            socket_dir: self.config.socket_dir.as_path(),
            dbname: &self.config.dbname,
            role: &self.config.role,
            query_timeout_secs: self.config.query_timeout_secs.get(),
            password_file: Some(self.config.password_file.as_path()),
            variables: &[
                ("scope", scope_of(pool_id)),
                ("k", self.security_parameter.to_string()),
            ],
            rows: Rows::TuplesOnly,
            text: QUERY,
        }
        .run()
        .map_err(|error| unavailable(error.to_string()))?;
        read_registrations(&csv)
            .map_err(|source| unavailable(format!("psql printed a row that is not hex: {source}")))
    }
}

/// One hex blob per line, as `psql --csv --tuples-only` prints a single column.
///
/// A line that is not hex is a fault in the query or in psql, not a stranger's
/// transaction: the column is `encode(tm.bytes, 'hex')`, produced server-side,
/// so it is hex whatever went on chain.
fn read_registrations(csv: &str) -> Result<Vec<Vec<u8>>, HexError> {
    csv.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(hex::decode_bytes)
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    #[error("cannot read the Shelley genesis {path}: {source}")]
    Unreadable {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the Shelley genesis {path} does not parse: {source}")]
    NotJson {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("the Shelley genesis {path} has no securityParam")]
    NoSecurityParameter { path: String },
    #[error("the Shelley genesis {path} has securityParam {found}, which is no block count")]
    NotABlockCount { path: String, found: String },
}

/// The chain's k, which fixes how deep a registration must be before it grants
/// anything. Why the genesis file is the only source: ADR 0008 Consequences.
///
/// Nonzero, because a k of zero counts a registration in the tip block and
/// there is then no depth to wait out at all.
pub fn security_parameter(path: &Path) -> Result<NonZeroU32, GenesisError> {
    let named = || path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| GenesisError::Unreadable {
        path: named(),
        source,
    })?;
    let genesis: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| GenesisError::NotJson {
            path: named(),
            source,
        })?;
    let found = genesis
        .get("securityParam")
        .ok_or_else(|| GenesisError::NoSecurityParameter { path: named() })?;
    found
        .as_u64()
        .and_then(|k| u32::try_from(k).ok())
        .and_then(NonZeroU32::new)
        .ok_or_else(|| GenesisError::NotABlockCount {
            path: named(),
            found: found.to_string(),
        })
}
