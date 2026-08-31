//! Signing-key loading: the cardano-cli TextEnvelope format SPO key files
//! already use (`cold.skey`, `bls.skey`). One accepted format keeps the
//! parsing surface small; anything else fails loudly at startup.
//!
//! Which scheme a key is comes from the envelope's `type`, never from its
//! bytes. Both files hold 32 bytes behind the same CBOR prefix, so a cold key
//! read as a Leios one would load, sign, and speak for nobody.

use std::path::Path;

use serde::Deserialize;

use metsuke_wire::envelope::{SigningKey, SubmissionKey};
use metsuke_wire::hex::{self, HexError};
use metsuke_wire::leios::{LeiosKeyError, LeiosSigningKey};

/// CBOR major type 2 (byte string), length 32, the prefix cardano-cli puts
/// before a raw key seed in `cborHex`. Matching it as a literal keeps CBOR out
/// of the runtime (ADR 0001).
const SEED_PREFIX: &str = "5820";

/// The `type` cardano-cli writes for each of the two keys an Agent may sign
/// with: `node key-gen`'s cold key and `node key-gen-BLS`'s Leios key.
const COLD_KEY_TYPE: &str = "StakePoolSigningKey_ed25519";
const LEIOS_KEY_TYPE: &str = "BlsSigningKey_bls12-381-BLS-Signature-Mininimal-Signature-Size";

/// The subset of a cardano-cli TextEnvelope this agent reads.
#[derive(Deserialize)]
struct TextEnvelope {
    #[serde(rename = "type")]
    key_type: String,
    #[serde(rename = "cborHex")]
    cbor_hex: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("cannot read signing key {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("signing key {path} is not a TextEnvelope JSON file: {source}")]
    Envelope {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "signing key {path} is a {found:?}, and an Agent signs with a \
         {COLD_KEY_TYPE:?} or a {LEIOS_KEY_TYPE:?}"
    )]
    NotASigningKey { path: String, found: String },
    #[error(
        "signing key {path}: cborHex does not begin with {SEED_PREFIX}, so it \
         is not a 32-byte seed"
    )]
    NotASeed { path: String },
    #[error("signing key {path}: cborHex after {SEED_PREFIX} {source}")]
    SeedNotHex {
        path: String,
        #[source]
        source: HexError,
    },
    #[error("signing key {path}: {source}")]
    NotALeiosKey {
        path: String,
        #[source]
        source: LeiosKeyError,
    },
    #[error(
        "no signing key: pass --signing-key <path> (systemd: \
         --signing-key \"${{CREDENTIALS_DIRECTORY}}/signing-key\") or set \
         signing_key in the config file"
    )]
    Missing,
}

/// Resolve the signing key: `--signing-key` flag over the config path,
/// absent both a loud startup failure.
pub fn resolve_signing_key(
    flag: Option<&Path>,
    config: Option<&Path>,
) -> Result<SubmissionKey, KeyError> {
    load_signing_key(flag.or(config).ok_or(KeyError::Missing)?)
}

/// Load a signing key from a TextEnvelope file, as whichever of the two
/// schemes its `type` names.
pub fn load_signing_key(path: &Path) -> Result<SubmissionKey, KeyError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| KeyError::Read {
        path: display.clone(),
        source,
    })?;
    let envelope: TextEnvelope =
        serde_json::from_str(&text).map_err(|source| KeyError::Envelope {
            path: display.clone(),
            source,
        })?;
    let digits = envelope
        .cbor_hex
        .strip_prefix(SEED_PREFIX)
        .ok_or(KeyError::NotASeed {
            path: display.clone(),
        })?;
    let seed = hex::decode::<32>(digits).map_err(|source| KeyError::SeedNotHex {
        path: display.clone(),
        source,
    })?;
    match envelope.key_type.as_str() {
        COLD_KEY_TYPE => Ok(SubmissionKey::ColdKey(SigningKey::from_bytes(&seed))),
        LEIOS_KEY_TYPE => Ok(SubmissionKey::LeiosKey(
            LeiosSigningKey::from_bytes(&seed).map_err(|source| KeyError::NotALeiosKey {
                path: display,
                source,
            })?,
        )),
        found => Err(KeyError::NotASigningKey {
            path: display,
            found: found.to_string(),
        }),
    }
}
