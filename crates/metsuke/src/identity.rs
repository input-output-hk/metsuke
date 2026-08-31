//! Who this agent is: the id every line it ships is stamped with, and whether
//! the key it signs with speaks for the pool it claims. Both are answered once
//! at startup, so an Agent that cannot say either never spools a row.

use metsuke_wire::envelope::{AgentId, AgentIdError, PoolId, SubmissionKey};

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("agent_id: {0}")]
    AgentId(#[from] AgentIdError),
    #[error("this Agent's hostname is not UTF-8: {found:?}")]
    HostnameNotUtf8 { found: std::ffi::OsString },
    #[error(
        "the signing key speaks for {implied}, but pool_id is {configured}: \
         a pool id is its cold verification key's hash, so the two have to agree"
    )]
    PoolIdMismatch { configured: PoolId, implied: PoolId },
}

/// The configured id, or this Agent's hostname when there is none. Both go
/// through `slugify`, so `agent_id = "Edge_1"` names the same Agent one whose
/// hostname is `Edge_1` does, and neither refuses to start over a character.
///
/// The hostname comes from the kernel rather than `/proc`, which the unit only
/// sees when trace collection is on (ADR 0010).
pub fn agent_id(configured: Option<&str>) -> Result<AgentId, IdentityError> {
    let name = match configured {
        Some(configured) => configured.to_string(),
        None => gethostname::gethostname()
            .into_string()
            .map_err(|found| IdentityError::HostnameNotUtf8 { found })?,
    };
    Ok(AgentId::slugify(&name)?)
}

/// Refuse a key that does not hash to the configured pool id. The server
/// checks the same thing per upload; failing here means an operator hears it
/// once at startup rather than as a rejection an hour later.
///
/// A Leios key hashes to nothing, so there is nothing to disagree with and
/// nothing to check: what it signs is admitted against the server's roster, and
/// the earliest an Agent holding the wrong one hears about it is that refusal
/// (ADR 0011).
pub fn check_pool_id(configured: PoolId, key: &SubmissionKey) -> Result<(), IdentityError> {
    match key.attributes() {
        None => Ok(()),
        Some(implied) if implied == configured => Ok(()),
        Some(implied) => Err(IdentityError::PoolIdMismatch {
            configured,
            implied,
        }),
    }
}
