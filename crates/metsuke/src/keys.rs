//! Signing-key loading: the cardano-cli TextEnvelope format SPO key files
//! already use (`pool.skey`, `pool.calidus.skey`). One accepted format keeps
//! the parsing surface small; anything else fails loudly at startup.

use std::path::Path;

use serde::Deserialize;

use crate::envelope::SigningKey;

/// CBOR major type 2 (byte string), length 32 — the prefix cardano-cli puts
/// before a raw Ed25519 seed in `cborHex`. Matching it as a literal keeps
/// CBOR out of the runtime (ADR 0001).
const SEED_PREFIX: &str = "5820";

/// The subset of a cardano-cli TextEnvelope this agent reads.
#[derive(Deserialize)]
struct TextEnvelope {
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
        "signing key {path}: cborHex is not a 32-byte Ed25519 seed \
         (expected {SEED_PREFIX} followed by 64 hex digits)"
    )]
    NotAnEd25519Seed { path: String },
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
) -> Result<SigningKey, KeyError> {
    load_signing_key(flag.or(config).ok_or(KeyError::Missing)?)
}

/// Load an Ed25519 signing key from a TextEnvelope file.
pub fn load_signing_key(path: &Path) -> Result<SigningKey, KeyError> {
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
    let seed = envelope
        .cbor_hex
        .strip_prefix(SEED_PREFIX)
        .and_then(decode_seed)
        .ok_or(KeyError::NotAnEd25519Seed { path: display })?;
    Ok(SigningKey::from_bytes(&seed))
}

fn decode_seed(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut seed = [0u8; 32];
    for (byte, pair) in seed.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(seed)
}
