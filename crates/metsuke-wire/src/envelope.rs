//! Envelope schemas and their sealed wire form: a zstd skippable frame
//! carrying a JSON header, then a zstd data frame carrying the payload's JSON
//! Lines, with one raw detached Ed25519 signature over both (ADR 0001). The
//! header is readable by seeking past eight bytes, so `split` answers "who
//! sent this" with no key and no decompressor; `seal` and `open` are the only
//! way to produce or consume a whole submission, which is what keeps
//! verify-before-decompress (ADR 0002) the only expressible call sequence.

use std::io::Read;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

/// The schema versions this crate speaks, one per `Payload` variant; which one
/// an envelope carries is `Payload::schema_version`.
pub const SCHEMA_VERSION_SAMPLES: u32 = 1;
pub const SCHEMA_VERSION_LINES: u32 = 2;

/// The zstd skippable-frame magic (RFC 8878 §3.1.2) a submission begins with.
/// Every conforming zstd tool skips this frame and decompresses the data frame
/// after it, so the payload reads back without knowing this format exists.
pub const CONTAINER_MAGIC: u32 = 0x184D_2A50;

/// Where the header's JSON starts: past the magic and the u32 length beside it.
pub const HEADER_OFFSET: usize = 8;

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

/// One signed upload batch. The replay `counter` and `timestamp` live in the
/// header frame, inside the signed bytes (ADR 0002).
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

/// The skippable frame's content: everything about a submission that is not
/// the payload itself. It holds no payload key, so which schema a submission
/// declares is answerable without inflating a byte.
#[derive(Serialize, Deserialize)]
struct Header {
    schema_version: u32,
    pool_id: PoolId,
    agent_version: String,
    counter: u64,
    #[serde(with = "time::serde::rfc3339")]
    timestamp: OffsetDateTime,
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

/// The two bounds `open` puts on bytes it did not produce. Both are server
/// configuration (`IngestConfig`); this crate holds no default for either.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_header_bytes: u64,
    pub max_decompressed_bytes: u64,
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
    /// The newline is what terminates one trace line, so a line holding one
    /// would open as two under a valid signature.
    #[error("trace line {index} holds a newline")]
    LineHoldsNewline { index: usize },
    /// The skippable frame states its length in a u32, so a header past that
    /// has no frame to travel in.
    #[error("header is {found} bytes, past what a skippable frame can declare")]
    HeaderTooLarge { found: usize },
}

/// Why a body is not a submission container at all. Separate from `OpenError`
/// because `split` answers it before a key or a decompressor is involved.
#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    #[error("body does not begin with a zstd skippable frame")]
    NotAContainer,
    #[error("header frame declares {declared} bytes, over the {max} byte limit")]
    OversizedHeader { declared: u64, max: u64 },
    #[error("header frame declares {declared} bytes, but only {found} follow it")]
    ShortHeader { declared: u64, found: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("signature verification failed: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),
    #[error(transparent)]
    Container(#[from] ContainerError),
    #[error("zstd decompression failed: {0}")]
    Decompress(#[source] std::io::Error),
    #[error("decompressed size exceeds the {max_decompressed_bytes} byte limit")]
    TooLarge { max_decompressed_bytes: u64 },
    #[error("JSON deserialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("payload is not UTF-8")]
    NotUtf8,
    /// Every line is newline-terminated, so bytes after the last one are a
    /// line the sender did not finish writing.
    #[error("payload's last line has no terminating newline")]
    UnterminatedLine,
    #[error(
        "envelope schema version {found}, this build speaks \
         v{SCHEMA_VERSION_SAMPLES} and v{SCHEMA_VERSION_LINES}"
    )]
    UnsupportedSchemaVersion { found: u32 },
}

/// A submission's two frames, borrowed from the bytes as received.
pub struct Frames<'a> {
    /// The skippable frame's content: the header JSON, uncompressed.
    pub header: &'a [u8],
    /// The zstd data frame, still compressed.
    pub data: &'a [u8],
}

/// Split a submission into its frames, bounding the header at
/// `max_header_bytes`. No key, no decompressor, and no allocation: this is
/// what an ingest path runs before it spends anything on a body.
pub fn split(bytes: &[u8], max_header_bytes: u64) -> Result<Frames<'_>, ContainerError> {
    let prefix: [u8; HEADER_OFFSET] = bytes
        .get(..HEADER_OFFSET)
        .and_then(|prefix| prefix.try_into().ok())
        .ok_or(ContainerError::NotAContainer)?;
    let (magic, declared) = prefix.split_at(4);
    if u32::from_le_bytes(magic.try_into().expect("four bytes")) != CONTAINER_MAGIC {
        return Err(ContainerError::NotAContainer);
    }
    let declared = u32::from_le_bytes(declared.try_into().expect("four bytes")) as u64;
    if declared > max_header_bytes {
        return Err(ContainerError::OversizedHeader {
            declared,
            max: max_header_bytes,
        });
    }
    let rest = &bytes[HEADER_OFFSET..];
    // `declared` is under `max_header_bytes`, a u64 the config holds, so the
    // cast only narrows a value already smaller than what `rest` can be.
    let split_at = declared as usize;
    if rest.len() < split_at {
        return Err(ContainerError::ShortHeader {
            declared,
            found: rest.len(),
        });
    }
    let (header, data) = rest.split_at(split_at);
    Ok(Frames { header, data })
}

/// The header frame's content, uncompressed. The agent budgets a batch against
/// this length rather than against a second account of the header's fields.
pub fn header_json(envelope: &Envelope) -> Result<Vec<u8>, SealError> {
    Ok(serde_json::to_vec(&Header {
        schema_version: envelope.schema_version,
        pool_id: envelope.pool_id,
        agent_version: envelope.agent_version.clone(),
        counter: envelope.counter,
        timestamp: envelope.timestamp,
    })?)
}

/// The data frame's content before compression: one JSON object per sample, or
/// one trace line as the node emitted it, each terminated by a newline. What
/// the server's `max_decompressed_bytes` bounds, and what `zstd -d` emits.
pub fn payload_lines(envelope: &Envelope) -> Result<Vec<u8>, SealError> {
    let mut body = Vec::new();
    match &envelope.payload {
        Payload::Samples { samples } => {
            for sample in samples {
                serde_json::to_writer(&mut body, sample)?;
                body.push(b'\n');
            }
        }
        Payload::Lines { lines } => {
            for (index, line) in lines.iter().enumerate() {
                if line.contains('\n') {
                    return Err(SealError::LineHoldsNewline { index });
                }
                body.extend_from_slice(line.as_bytes());
                body.push(b'\n');
            }
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
    let header = header_json(envelope)?;
    let declared = u32::try_from(header.len()).map_err(|_| SealError::HeaderTooLarge {
        found: header.len(),
    })?;
    let data = zstd::encode_all(payload_lines(envelope)?.as_slice(), level)
        .map_err(SealError::Compress)?;

    let mut wire_bytes = Vec::with_capacity(HEADER_OFFSET + header.len() + data.len());
    wire_bytes.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
    wire_bytes.extend_from_slice(&declared.to_le_bytes());
    wire_bytes.extend_from_slice(&header);
    wire_bytes.extend_from_slice(&data);

    use ed25519_dalek::Signer;
    let signature = key.sign(&wire_bytes);
    Ok((wire_bytes, signature))
}

/// How much decompressed output `open` copies per read. Granularity, not a
/// limit: it bounds the scratch buffer, never what is accepted.
const DECOMPRESS_CHUNK_BYTES: usize = 64 * 1024;

/// Verify the signature over the wire bytes as received, then read the header
/// out of the skippable frame and decompress the data frame — refusing to
/// inflate past `limits.max_decompressed_bytes`. Uses `verify_strict` to
/// reject signatures that only pass under malleable or mixed-order-point
/// interpretations.
pub fn open(
    key: &VerifyingKey,
    wire_bytes: &[u8],
    signature: &Signature,
    limits: Limits,
) -> Result<Envelope, OpenError> {
    key.verify_strict(wire_bytes, signature)?;
    let frames = split(wire_bytes, limits.max_header_bytes)?;
    // The version alone first, so a version this build never spoke is named as
    // such whatever else its header holds — a v3 that dropped a field every
    // version so far carries would otherwise report that missing field.
    let peek: SchemaVersionPeek = serde_json::from_slice(frames.header)?;
    let schema = Schema::of(peek.schema_version)?;
    let header: Header = serde_json::from_slice(frames.header)?;
    let body = inflate(frames.data, limits.max_decompressed_bytes)?;
    let lines = match body.strip_suffix('\n') {
        Some(rest) => rest.split('\n').collect(),
        None if body.is_empty() => Vec::new(),
        None => return Err(OpenError::UnterminatedLine),
    };
    let payload = match schema {
        Schema::Samples => Payload::Samples {
            samples: lines
                .iter()
                .map(|line| serde_json::from_str(line))
                .collect::<Result<_, _>>()?,
        },
        Schema::Lines => Payload::Lines {
            lines: lines.into_iter().map(str::to_string).collect(),
        },
    };
    // Through `new`, so what comes out declares the version its payload has
    // rather than the one the header claimed.
    Ok(Envelope::new(
        header.pool_id,
        header.agent_version,
        header.counter,
        header.timestamp,
        payload,
    ))
}

/// Decompress one data frame, stopping one byte past the ceiling.
fn inflate(data: &[u8], max_decompressed_bytes: u64) -> Result<String, OpenError> {
    let decoder = zstd::Decoder::new(data).map_err(OpenError::Decompress)?;
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
    String::from_utf8(body).map_err(|_| OpenError::NotUtf8)
}

#[derive(Deserialize)]
struct SchemaVersionPeek {
    schema_version: u32,
}
