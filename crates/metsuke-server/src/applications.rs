//! The code a pool was onboarded against, as the allowlist holds it. What the
//! pair of application and pool registration proves is CONTEXT.md, under
//! **Application Code**; checking the two halves against each other is an
//! offline step that never runs on the serving path (metsuke-jfb.7).

use std::collections::BTreeMap;

use metsuke_wire::envelope::PoolId;
use serde::{Deserialize, Serialize};

/// Where a pool registration carries its application code: CIP-20 fixes the
/// label, the key inside it is the rewards program's own. Public because the
/// instructions page tells an operator to write exactly this pair.
pub const METADATA_LABEL: i64 = 674;
pub const METADATA_KEY: &str = "musashinet_incentives_application_code";

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

/// One code per pool, which is what the allowlist is.
pub type Codes = BTreeMap<PoolId, ApplicationCode>;
