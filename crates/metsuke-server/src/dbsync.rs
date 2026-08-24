//! The `Directory` behind the Calidus half: label-867 rows read out of the
//! Leios db-sync by running the shipped query, and the security parameter that
//! says which of them are deep enough to count (ADR 0008).

use std::num::NonZeroU32;
use std::path::Path;

use metsuke_wire::envelope::PoolId;
use metsuke_wire::hex;

use crate::calidus::{Directory, DirectoryError};
use crate::config::CalidusConfig;
use crate::db::{Connection, column};

/// The one query the server asks a chain. Its own file so the recorder runs the
/// same text the server does: scripts/record-calidus-fixtures.sh.
const QUERY: &str = include_str!("registrations.sql");

/// How the query names a pool: db-sync renders metadata byte strings into its
/// `json` column as the `0x`-prefixed hex the scope holds.
fn scope_of(pool_id: PoolId) -> String {
    format!("0x{}", hex::encode(pool_id.as_hash()))
}

/// The chain, over the db-sync the config names.
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
        // The depth is bound as `bigint` because that is what `block_no` is;
        // k is a `u32`, so the widening cannot lose it.
        let k = i64::from(self.security_parameter.get());
        Connection {
            socket_dir: self.config.socket_dir.as_path(),
            dbname: &self.config.dbname,
            role: &self.config.role,
            query_timeout_secs: self.config.query_timeout_secs.get(),
            password_file: Some(self.config.password_file.as_path()),
        }
        .query(QUERY, &[&scope_of(pool_id), &k])
        .and_then(|rows| {
            rows.iter()
                .map(|row| column(row, "registration"))
                .collect::<Result<Vec<Vec<u8>>, _>>()
        })
        .map_err(|error| DirectoryError::Unavailable {
            pool_id,
            reason: error.to_string(),
        })
    }
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
