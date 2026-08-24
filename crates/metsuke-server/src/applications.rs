//! The gate a pool passes to reach the allowlist: the code in its application
//! must be the code its current pool registration carries. What that pair
//! proves is CONTEXT.md, under **Application Code**.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use metsuke_wire::envelope::{PoolId, PoolIdError};
use serde::{Deserialize, Serialize};

use crate::config::ApplicationsConfig;

/// Where a pool registration carries its application code: CIP-20 fixes the
/// label, the key inside it is the rewards program's own.
const METADATA_LABEL: u64 = 674;
const METADATA_KEY: &str = "musashinet_incentives_application_code";

/// The code an operator puts in both halves of the gate. Constrained to an
/// identifier alphabet so it is a bare TOML string wherever it is emitted, and
/// trimmed on the way in because a form export carries the whitespace around
/// what was pasted into it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ApplicationCode(String);

#[derive(Debug, thiserror::Error)]
pub enum ApplicationCodeError {
    #[error("an application code cannot be empty")]
    Empty,
    #[error("{found:?} is not an application code: letters, digits, '-', '_' and '.' only")]
    NotAnIdentifier { found: String },
}

impl ApplicationCode {
    pub fn parse(text: &str) -> Result<Self, ApplicationCodeError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ApplicationCodeError::Empty);
        }
        let identifier = |character: char| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        };
        if !trimmed.chars().all(identifier) {
            return Err(ApplicationCodeError::NotAnIdentifier {
                found: trimmed.to_string(),
            });
        }
        Ok(ApplicationCode(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApplicationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ApplicationCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        ApplicationCode::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// One code per pool, which is what both halves of the gate are.
pub type Codes = BTreeMap<PoolId, ApplicationCode>;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("cannot run {psql}: {source}")]
    CannotRun {
        psql: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{psql} exited {status}: {stderr}")]
    QueryFailed {
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
    #[error("cannot read what {psql} printed: {source}")]
    Unreadable {
        psql: String,
        #[source]
        source: ApplicationsCsvError,
    },
}

/// Every live pool's application code, from its current registration alone: an
/// earlier update's code is one the operator has already replaced, and counting
/// it would hold a corrected typo against them forever.
///
/// `DISTINCT` because one transaction may carry several registration
/// certificates for a pool: db-sync writes a `pool_update` row per certificate,
/// all sharing the `registered_tx_id` this joins on, so the same code comes back
/// once per certificate.
const QUERY: &str = "\
SELECT DISTINCT ph.view AS pool_id,
       tm.json ->> '{key}' AS application_code
FROM pool_hash ph
JOIN pool_update pu ON pu.hash_id = ph.id
JOIN tx_metadata tm ON tm.tx_id = pu.registered_tx_id
WHERE tm.key = {label}
  AND tm.json ? '{key}'
  AND pu.registered_tx_id = (
        SELECT MAX(registered_tx_id) FROM pool_update WHERE hash_id = ph.id)
  AND NOT EXISTS (
        SELECT 1
        FROM pool_retire pr
        WHERE pr.hash_id = ph.id
          AND pr.announced_tx_id > pu.registered_tx_id)";

/// The chain half, read by running the shipped query through `psql`. Why not a
/// Postgres client: ADR 0008 Consequences.
pub struct Psql<'a> {
    config: &'a ApplicationsConfig,
}

impl<'a> Psql<'a> {
    pub fn new(config: &'a ApplicationsConfig) -> Self {
        Psql { config }
    }

    pub fn registered_codes(&self) -> Result<Registered, ChainError> {
        let psql = self.config.psql_path.as_path().display().to_string();
        // Nothing is inherited: PGPASSWORD, PGSERVICE and PGDATABASE would each
        // reach a database the config does not name. There is no password to
        // pass instead — the connection is a peer-authenticated unix socket,
        // which is a deployment choice, not something the protocol fixes.
        let printed = Command::new(self.config.psql_path.as_path())
            .env_clear()
            .env(
                "PGOPTIONS",
                format!(
                    "-c statement_timeout={}s",
                    self.config.query_timeout_secs.get()
                ),
            )
            .args(["--csv", "--no-psqlrc", "--quiet"])
            .args([
                "--host".as_ref(),
                self.config.socket_dir.as_path().as_os_str(),
            ])
            .args(["--dbname", &self.config.dbname])
            .args(["--username", &self.config.role])
            .args(["--command", &self.query()])
            .output()
            .map_err(|source| ChainError::CannotRun {
                psql: psql.clone(),
                source,
            })?;
        if !printed.status.success() {
            return Err(ChainError::QueryFailed {
                psql,
                status: printed.status.to_string(),
                stderr: String::from_utf8_lossy(&printed.stderr).trim().to_string(),
            });
        }
        let csv = String::from_utf8(printed.stdout).map_err(|source| ChainError::NotUtf8 {
            psql: psql.clone(),
            source,
        })?;
        read_registered(&csv).map_err(|source| ChainError::Unreadable { psql, source })
    }

    fn query(&self) -> String {
        QUERY
            .replace("{key}", METADATA_KEY)
            .replace("{label}", &METADATA_LABEL.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationsCsvError {
    #[error("cannot read the rows: {0}")]
    Csv(#[from] csv::Error),
    #[error("no {POOL_ID_COLUMN:?} and {CODE_COLUMN:?} columns: found {found}")]
    MissingColumns { found: String },
    #[error("row {row}: {source}")]
    NotAPoolId {
        row: usize,
        #[source]
        source: PoolIdError,
    },
    #[error("row {row}: {source}")]
    NotACode {
        row: usize,
        #[source]
        source: ApplicationCodeError,
    },
    #[error("row {row}: pool {pool_id} already has code {first}")]
    PoolTwice {
        row: usize,
        pool_id: PoolId,
        first: ApplicationCode,
    },
}

pub const POOL_ID_COLUMN: &str = "pool_id";
pub const CODE_COLUMN: &str = "application_code";

/// The applications half. Every row must read, because this file is the
/// program's own record: a row that does not is a mistake to fix at the source,
/// and dropping it would read as a pool that never applied.
pub fn read_codes(text: &str) -> Result<Codes, ApplicationsCsvError> {
    let (mut reader, columns) = open(text)?;
    let mut codes = Codes::new();
    for (row, record) in numbered(&mut reader) {
        let (pool_id, code) = columns.parse(&record?, row)?;
        if let Some(first) = codes.insert(pool_id, code) {
            return Err(ApplicationsCsvError::PoolTwice {
                row,
                pool_id,
                first,
            });
        }
    }
    Ok(codes)
}

/// The registered half: the codes it named, and what it said that named none.
/// The two are counted apart because one is rows and the other is pools.
#[derive(Debug, PartialEq, Eq)]
pub struct Registered {
    pub codes: Codes,
    /// Rows no pool and code could be read from.
    pub unreadable: usize,
    /// Pools the answer gave more than one code for, so `codes` holds neither.
    pub contradicted: BTreeSet<PoolId>,
}

/// The registered half. Nothing here is the program's to fix: any pool operator
/// on the chain can register the label under any value, so a row that does not
/// read is dropped rather than refused — it matches no application either way,
/// and failing on it would let a stranger's transaction stop every onboarding.
///
/// A missing column is still fatal: the query names its own, so their absence
/// is this file disagreeing with itself.
pub fn read_registered(text: &str) -> Result<Registered, ApplicationsCsvError> {
    let (mut reader, columns) = open(text)?;
    let mut codes = Codes::new();
    let mut unreadable = 0;
    // Dropped after the pass, not during it: the row that contradicts comes
    // second, so the first is already in.
    let mut contradicted = BTreeSet::new();
    for (row, record) in numbered(&mut reader) {
        let read = record
            .map_err(ApplicationsCsvError::from)
            .and_then(|record| columns.parse(&record, row));
        match read {
            Err(_) => unreadable += 1,
            Ok((pool_id, code)) => match codes.insert(pool_id, code.clone()) {
                Some(first) if first != code => {
                    contradicted.insert(pool_id);
                }
                _ => {}
            },
        }
    }
    for pool_id in &contradicted {
        codes.remove(pool_id);
    }
    Ok(Registered {
        codes,
        unreadable,
        contradicted,
    })
}

/// Where the two columns sit in a header that may carry others.
struct Columns {
    pool: usize,
    code: usize,
}

impl Columns {
    fn parse(
        &self,
        record: &csv::StringRecord,
        row: usize,
    ) -> Result<(PoolId, ApplicationCode), ApplicationsCsvError> {
        let field = |at: usize| record.get(at).unwrap_or_default();
        Ok((
            PoolId::from_bech32(field(self.pool).trim())
                .map_err(|source| ApplicationsCsvError::NotAPoolId { row, source })?,
            ApplicationCode::parse(field(self.code))
                .map_err(|source| ApplicationsCsvError::NotACode { row, source })?,
        ))
    }
}

/// Both halves arrive as CSV with the same two columns, found by name so the
/// columns around them are whatever wrote the file carries.
fn open(text: &str) -> Result<(csv::Reader<&[u8]>, Columns), ApplicationsCsvError> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let header = reader.headers()?.clone();
    let column = |name: &str| header.iter().position(|found| found == name);
    let (Some(pool), Some(code)) = (column(POOL_ID_COLUMN), column(CODE_COLUMN)) else {
        return Err(ApplicationsCsvError::MissingColumns {
            found: header.iter().collect::<Vec<_>>().join(", "),
        });
    };
    Ok((reader, Columns { pool, code }))
}

/// The records against the line numbers an operator counting rows in their file
/// would reach: the header is row 1.
fn numbered<'a>(
    reader: &'a mut csv::Reader<&'a [u8]>,
) -> impl Iterator<Item = (usize, csv::Result<csv::StringRecord>)> + 'a {
    reader
        .records()
        .enumerate()
        .map(|(index, record)| (index + 2, record))
}

/// Why a pool that applied is not allowlisted. Each one is a different fix for
/// its operator, which is why the summary prints them per pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Excluded {
    /// Applied, but its current registration carries no application code.
    NotRegistered,
    /// Applied with a code that is not the registered one.
    CodeMismatch { registered: ApplicationCode },
    /// Applied, and `read_registered` had more than one code for the pool, so
    /// it kept none. Distinct from `NotRegistered`: the operator registered
    /// something, and re-registering one code is what fixes it.
    ContradictoryCodes,
}

/// The gate's verdict, and everything the summary reports. Every pool either
/// half named is in exactly one of the three.
#[derive(Debug)]
pub struct Gate {
    pub allowed: Codes,
    /// One entry per pool that applied and did not pass. Bounded by the
    /// applications export, so the summary can name each.
    pub excluded: BTreeMap<PoolId, Excluded>,
    /// Pools the registered half named that never applied. A count, not
    /// identities, because every live pool on the chain is in that half —
    /// strangers would decide how many lines the summary is.
    pub did_not_apply: usize,
    /// Carried from `Registered`: the same argument makes it a count.
    pub unreadable: usize,
}

impl Gate {
    /// The pairs as a TOML document of their own, ordered by pool id. No
    /// `[ingest.allowlist]` header: these bytes are that key's value, and where
    /// it is spliced in is the Nix module's business, not this file's.
    pub fn to_toml(&self) -> String {
        self.allowed
            .iter()
            .map(|(pool_id, code)| format!("{pool_id} = \"{code}\"\n"))
            .collect()
    }
}

pub fn gate(applied: Codes, registered: Registered) -> Gate {
    let mut allowed = Codes::new();
    let mut excluded = BTreeMap::new();
    for (pool_id, code) in &applied {
        // Ahead of the lookup: reading `Registered::contradicted`.s absence from
        // `codes` as "registered nothing" would send the operator after a
        // registration they already made.
        if registered.contradicted.contains(pool_id) {
            excluded.insert(*pool_id, Excluded::ContradictoryCodes);
            continue;
        }
        match registered.codes.get(pool_id) {
            None => {
                excluded.insert(*pool_id, Excluded::NotRegistered);
            }
            Some(found) if found == code => {
                allowed.insert(*pool_id, code.clone());
            }
            Some(found) => {
                excluded.insert(
                    *pool_id,
                    Excluded::CodeMismatch {
                        registered: found.clone(),
                    },
                );
            }
        }
    }
    // Both halves of what the chain named, so no pool falls out of the report.
    // Disjoint, so nothing is counted twice.
    let did_not_apply = registered
        .codes
        .keys()
        .chain(registered.contradicted.iter())
        .filter(|pool_id| !applied.contains_key(pool_id))
        .count();
    Gate {
        allowed,
        excluded,
        did_not_apply,
        unreadable: registered.unreadable,
    }
}
