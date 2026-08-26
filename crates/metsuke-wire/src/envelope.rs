//! Envelope schemas and their sealed wire form: a JSON header line, then the
//! payload's own lines, zstd compressed, raw detached Ed25519 over the
//! compressed bytes (ADR 0001). `seal` and `open` are the whole interface, so
//! verify-before-decompress (ADR 0002) is the only expressible call sequence.

use std::io::Read;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

/// The schema versions this crate speaks, one per `Payload` variant; which one
/// an envelope carries is `Payload::schema_version`.
pub const SCHEMA_VERSION_SAMPLES: u32 = 1;
pub const SCHEMA_VERSION_LINES: u32 = 2;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

    /// The 28 bytes themselves, for the one caller that reads a pool id out of
    /// something that is not bech32: a CIP-151 registration scope.
    pub fn as_hash(&self) -> &[u8; 28] {
        &self.0
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

/// What one batch carries, and therefore which schema version it is. A
/// version names a payload shape, so an envelope never states the two apart.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Samples { samples: Vec<Sample> },
    Lines { lines: Vec<String> },
}

impl Payload {
    pub fn schema_version(&self) -> u32 {
        match self {
            Payload::Samples { .. } => SCHEMA_VERSION_SAMPLES,
            Payload::Lines { .. } => SCHEMA_VERSION_LINES,
        }
    }
}

/// One signed upload batch. The replay `counter` and `timestamp` live here,
/// inside the signed payload (ADR 0002).
///
/// `schema_version` and `payload` are private and only `new` sets them: an
/// envelope in hand always declares the version its payload has.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    schema_version: u32,
    pub pool_id: PoolId,
    pub agent_version: String,
    /// Per-pool monotonic replay counter.
    pub counter: u64,
    /// Batch creation time, RFC 3339 UTC.
    pub timestamp: OffsetDateTime,
    payload: Payload,
}

/// The body's first line, and the only JSON either version parses.
///
/// v1 carries its samples here, because that is where v1 put them and v1's
/// bytes are frozen; v2 leaves the key out and writes its trace lines after
/// this line instead. `serde_json` escapes every newline it meets, so a
/// serialized header is one line whatever it holds — which is what makes a v1
/// body, one JSON object and nothing else, a header line with no lines after
/// it.
#[derive(Serialize, Deserialize)]
struct HeaderLine {
    schema_version: u32,
    pool_id: PoolId,
    agent_version: String,
    counter: u64,
    #[serde(with = "time::serde::rfc3339")]
    timestamp: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    samples: Option<Vec<Sample>>,
}

/// Which of this build's two schemas a declared version names.
enum Schema {
    Samples,
    Lines,
}

impl Schema {
    fn of(version: u32) -> Result<Schema, OpenError> {
        match version {
            SCHEMA_VERSION_SAMPLES => Ok(Schema::Samples),
            SCHEMA_VERSION_LINES => Ok(Schema::Lines),
            found => Err(OpenError::UnsupportedSchemaVersion { found }),
        }
    }
}

impl Envelope {
    pub fn new(
        pool_id: PoolId,
        agent_version: String,
        counter: u64,
        timestamp: OffsetDateTime,
        payload: Payload,
    ) -> Envelope {
        Envelope {
            schema_version: payload.schema_version(),
            pool_id,
            agent_version,
            counter,
            timestamp,
            payload,
        }
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn payload(&self) -> &Payload {
        &self.payload
    }
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
    /// The newline is what separates one trace line from the next, so a line
    /// holding one would open as two under a valid signature.
    #[error("trace line {index} holds a newline")]
    LineHoldsNewline { index: usize },
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
    #[error("payload is not UTF-8")]
    NotUtf8,
    #[error(
        "envelope schema version {found}, this build speaks \
         v{SCHEMA_VERSION_SAMPLES} and v{SCHEMA_VERSION_LINES}"
    )]
    UnsupportedSchemaVersion { found: u32 },
    /// The header names a version whose body it does not have, so the envelope
    /// contradicts itself and neither reading is the sender's.
    #[error("envelope declares schema v{declared} and carries {found}")]
    BodyContradictsVersion { declared: u32, found: &'static str },
}

/// The body `seal` compresses: the header line, then one line per trace line,
/// as the node emitted it. Public because the agent budgets a batch against
/// these bytes and the operator page renders them — both want what this build
/// sends rather than a second measure of it.
pub fn body(envelope: &Envelope) -> Result<Vec<u8>, SealError> {
    let header = HeaderLine {
        schema_version: envelope.schema_version,
        pool_id: envelope.pool_id,
        agent_version: envelope.agent_version.clone(),
        counter: envelope.counter,
        timestamp: envelope.timestamp,
        samples: match &envelope.payload {
            Payload::Samples { samples } => Some(samples.clone()),
            Payload::Lines { .. } => None,
        },
    };
    let mut body = serde_json::to_vec(&header)?;
    if let Payload::Lines { lines } = &envelope.payload {
        for (index, line) in lines.iter().enumerate() {
            if line.contains('\n') {
                return Err(SealError::LineHoldsNewline { index });
            }
            body.push(b'\n');
            body.extend_from_slice(line.as_bytes());
        }
    }
    Ok(body)
}

/// Serialize, compress, and sign an envelope. Returns the wire bytes exactly
/// as they must be sent and archived, plus the detached signature over them.
/// `level` is the zstd compression level (0 = zstd's default).
pub fn seal(
    key: &SigningKey,
    envelope: &Envelope,
    level: i32,
) -> Result<(Vec<u8>, Signature), SealError> {
    if let Payload::Samples { samples } = &envelope.payload {
        for (index, sample) in samples.iter().enumerate() {
            if let Some(value) = sample.sync_progress
                && !value.is_finite()
            {
                return Err(SealError::NonFiniteSyncProgress { index, value });
            }
        }
    }
    let body = body(envelope)?;
    let wire_bytes = zstd::encode_all(body.as_slice(), level).map_err(SealError::Compress)?;
    use ed25519_dalek::Signer;
    let signature = key.sign(&wire_bytes);
    Ok((wire_bytes, signature))
}

/// How much decompressed output `open` copies per read. Granularity, not a
/// limit: it bounds the scratch buffer, never what is accepted.
const DECOMPRESS_CHUNK_BYTES: usize = 64 * 1024;

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
    // One chunk out of the decoder at a time, reserving exactly what each
    // chunk needs: the payload never claims more than its own size, and the
    // reader stops one byte past the ceiling, so nothing bigger is held.
    let mut reader = decoder.take(max_decompressed_bytes.saturating_add(1));
    let mut chunk = vec![0u8; DECOMPRESS_CHUNK_BYTES];
    let mut body: Vec<u8> = Vec::new();
    loop {
        let read = reader.read(&mut chunk).map_err(OpenError::Decompress)?;
        if read == 0 {
            break;
        }
        body.reserve_exact(read);
        body.extend_from_slice(&chunk[..read]);
    }
    if body.len() as u64 > max_decompressed_bytes {
        return Err(OpenError::TooLarge {
            max_decompressed_bytes,
        });
    }
    let body = String::from_utf8(body).map_err(|_| OpenError::NotUtf8)?;
    let mut split = body.split('\n');
    let header_line = split
        .next()
        .expect("splitting on a separator yields at least one part");
    let lines: Vec<String> = split.map(str::to_string).collect();
    // The version alone first, so a version this build never spoke is named as
    // such whatever else its header holds — a v3 that dropped a field every
    // version so far carries would otherwise report that missing field.
    let peek: SchemaVersionPeek = serde_json::from_str(header_line)?;
    let schema = Schema::of(peek.schema_version)?;
    let header: HeaderLine = serde_json::from_str(header_line)?;
    let contradicts = |found| OpenError::BodyContradictsVersion {
        declared: header.schema_version,
        found,
    };
    let payload = match (schema, header.samples) {
        (Schema::Samples, Some(samples)) if lines.is_empty() => Payload::Samples { samples },
        (Schema::Samples, Some(_)) => return Err(contradicts("lines after its header")),
        (Schema::Samples, None) => return Err(contradicts("no samples")),
        (Schema::Lines, None) => Payload::Lines { lines },
        (Schema::Lines, Some(_)) => return Err(contradicts("samples in its header")),
    };
    // Through `new`, so what comes out declares the version its payload has
    // rather than the one the header claimed: the match above is what decides
    // the two agree, and nothing downstream has to take that on trust.
    Ok(Envelope::new(
        header.pool_id,
        header.agent_version,
        header.counter,
        header.timestamp,
        payload,
    ))
}

#[derive(Deserialize)]
struct SchemaVersionPeek {
    schema_version: u32,
}
