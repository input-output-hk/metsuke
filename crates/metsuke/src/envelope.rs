//! Envelope schema v1 and its sealed wire form: JSON, zstd compressed, raw
//! detached Ed25519 over the compressed bytes (ADR 0001). `seal` and `open`
//! are the whole interface, so verify-before-decompress (ADR 0002) is the
//! only expressible call sequence.

use std::io::Read;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

/// Envelope schema version emitted by this crate. v2 (log-based schema)
/// bumps this without breaking v1 clients.
pub const SCHEMA_VERSION: u32 = 1;

/// Upload request headers (ADR 0001): pool id as bech32, verification key
/// and detached signature as lowercase hex over the body bytes as sent.
pub const HEADER_POOL_ID: &str = "x-metsuke-pool-id";
pub const HEADER_VKEY: &str = "x-metsuke-vkey";
pub const HEADER_SIGNATURE: &str = "x-metsuke-signature";

/// The server's answer to an accepted upload. `latest_version` is the
/// client-crate version embedded at server build (ADR 0006).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    pub latest_version: String,
}

/// A pool id: the blake2b-224 hash of the pool's cold verification key,
/// bech32 `pool1…` on the wire. The only constructors validate, so a held
/// `PoolId` is always a real 28-byte hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolId([u8; 28]);

/// CIP-5 human-readable prefix for pool ids.
const POOL_HRP: &str = "pool";

#[derive(Debug, thiserror::Error)]
pub enum PoolIdError {
    #[error("not valid bech32: {0}")]
    Bech32(#[from] bech32::primitives::decode::CheckedHrpstringError),
    #[error("human-readable prefix is {found:?}, expected {POOL_HRP:?}")]
    WrongHrp { found: String },
    #[error("payload is {found} bytes, expected 28")]
    WrongLength { found: usize },
}

impl PoolId {
    pub fn from_bech32(s: &str) -> Result<Self, PoolIdError> {
        use bech32::primitives::decode::CheckedHrpstring;
        let checked = CheckedHrpstring::new::<bech32::Bech32>(s)?;
        if checked.hrp().as_str() != POOL_HRP {
            return Err(PoolIdError::WrongHrp {
                found: checked.hrp().to_string(),
            });
        }
        let bytes: Vec<u8> = checked.byte_iter().collect();
        let hash = bytes
            .as_slice()
            .try_into()
            .map_err(|_| PoolIdError::WrongLength { found: bytes.len() })?;
        Ok(PoolId(hash))
    }

    /// The pool id a cold verification key hashes to. The ADR-0003 cold-key
    /// check is `envelope.pool_id == PoolId::from_cold_key(vkey)`.
    pub fn from_cold_key(key: &VerifyingKey) -> Self {
        use blake2::digest::consts::U28;
        use blake2::{Blake2b, Digest};
        PoolId(Blake2b::<U28>::digest(key.as_bytes()).into())
    }

    pub fn to_bech32(&self) -> String {
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse_unchecked(POOL_HRP), &self.0)
            .expect("28-byte payload is within the bech32 length limit")
    }
}

impl std::fmt::Display for PoolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_bech32())
    }
}

impl Serialize for PoolId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_bech32())
    }
}

impl<'de> Deserialize<'de> for PoolId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        PoolId::from_bech32(&s).map_err(serde::de::Error::custom)
    }
}

/// One signed upload batch. The replay `counter` and `timestamp` live here,
/// inside the signed payload (ADR 0002).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub pool_id: PoolId,
    pub agent_version: String,
    /// Per-pool monotonic replay counter.
    pub counter: u64,
    /// Batch creation time, RFC 3339 UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub samples: Vec<Sample>,
}

/// One scrape. Every field is nullable: a failed scrape is itself signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Scrape time, RFC 3339 UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub sampled_at: OffsetDateTime,
    pub block_height: Option<u64>,
    pub slot: Option<u64>,
    pub slot_in_epoch: Option<u64>,
    pub epoch: Option<u64>,
    pub sync_progress: Option<f64>,
    pub node_version: Option<String>,
    pub node_revision: Option<String>,
    pub clock_offset_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// JSON has no representation for non-finite floats; serializing one
    /// would silently become `null` on the wire.
    #[error("sample {index}: sync_progress is not finite ({value})")]
    NonFiniteSyncProgress { index: usize, value: f64 },
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zstd compression failed: {0}")]
    Compress(#[source] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("signature verification failed: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),
    #[error("zstd decompression failed: {0}")]
    Decompress(#[source] std::io::Error),
    #[error("decompressed size exceeds the {max_decompressed_bytes} byte limit")]
    TooLarge { max_decompressed_bytes: u64 },
    #[error("JSON deserialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Serialize, compress, and sign an envelope. Returns the wire bytes exactly
/// as they must be sent and archived, plus the detached signature over them.
/// `level` is the zstd compression level (0 = zstd's default).
pub fn seal(
    key: &SigningKey,
    envelope: &Envelope,
    level: i32,
) -> Result<(Vec<u8>, Signature), SealError> {
    for (index, sample) in envelope.samples.iter().enumerate() {
        if let Some(value) = sample.sync_progress
            && !value.is_finite()
        {
            return Err(SealError::NonFiniteSyncProgress { index, value });
        }
    }
    let json = serde_json::to_vec(envelope)?;
    let wire_bytes = zstd::encode_all(json.as_slice(), level).map_err(SealError::Compress)?;
    use ed25519_dalek::Signer;
    let signature = key.sign(&wire_bytes);
    Ok((wire_bytes, signature))
}

/// Verify the signature over the wire bytes as received, then decompress —
/// refusing to inflate past `max_decompressed_bytes` — and parse. Uses
/// `verify_strict` to reject signatures that only pass under malleable or
/// mixed-order-point interpretations.
pub fn open(
    key: &VerifyingKey,
    wire_bytes: &[u8],
    signature: &Signature,
    max_decompressed_bytes: u64,
) -> Result<Envelope, OpenError> {
    key.verify_strict(wire_bytes, signature)?;
    let decoder = zstd::Decoder::new(wire_bytes).map_err(OpenError::Decompress)?;
    let mut json = Vec::new();
    let read = decoder
        .take(max_decompressed_bytes + 1)
        .read_to_end(&mut json)
        .map_err(OpenError::Decompress)?;
    if read as u64 > max_decompressed_bytes {
        return Err(OpenError::TooLarge {
            max_decompressed_bytes,
        });
    }
    Ok(serde_json::from_slice(&json)?)
}
