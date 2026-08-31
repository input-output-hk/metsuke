//! Envelope schemas and their sealed wire form: a zstd skippable frame
//! carrying a JSON header, then a zstd data frame carrying the payload's JSON
//! Lines, with one raw detached Ed25519 signature over both (ADR 0001). The
//! header is readable by seeking past eight bytes, so `split` answers "who
//! sent this" with no key and no decompressor; `seal` and `open` are the only
//! way to produce or consume a whole submission, so a consumer cannot inflate a
//! data frame whose signature it has not checked.

use std::io::Read;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::leios::{self, LeiosPublicKey, LeiosSignature, LeiosSigningKey};

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

/// The schema versions this crate speaks, one per `Payload` variant; which one
/// an envelope carries is `Payload::schema_version`.
pub const SCHEMA_VERSION_SCRAPES: u32 = 1;
pub const SCHEMA_VERSION_LINES: u32 = 2;

/// The zstd skippable-frame magic (RFC 8878 §3.1.2) a submission begins with.
/// Every conforming zstd tool skips this frame and decompresses the data frame
/// after it, so the payload reads back without knowing this format exists.
pub const CONTAINER_MAGIC: u32 = 0x184D_2A50;

/// Where the header's JSON starts: past the magic and the u32 length beside it.
pub const HEADER_OFFSET: usize = 8;

/// Upload request headers (ADR 0001): verification key and detached signature
/// as lowercase hex over the body bytes as sent.
pub const HEADER_VKEY: &str = "x-metsuke-vkey";
pub const HEADER_SIGNATURE: &str = "x-metsuke-signature";

/// Which pool the submission is from, bech32. A **Cold Key** answers this on
/// its own, so under one this header is a second copy that has to agree; a
/// **Leios Key** answers nothing, so under one it is the only thing that says
/// where to look and it is believed only once the roster and the signature
/// agree with it (ADR 0011).
pub const HEADER_POOL: &str = "x-metsuke-pool";

/// The Ed25519 lengths, named because the decoder tells the two schemes apart
/// by them.
const VKEY_BYTES: usize = 32;
const SIGNATURE_BYTES: usize = 64;

/// What a stored object's signature is checked with, as it travels: the archive
/// holds it beside the bytes (ADR 0005) and a download carries it back in the
/// same two headers an upload arrived with. Both or neither, because a check
/// needs the pair, so half of it is the same as none of it.
///
/// Which scheme signed is the length of the pair and nothing else. The two are
/// 32/64 and 96/48, so no third header has to be agreed on and no submission
/// can name one scheme while carrying another's bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum Attestation {
    ColdKey {
        vkey: VerifyingKey,
        signature: Signature,
    },
    LeiosKey {
        key: LeiosPublicKey,
        signature: LeiosSignature,
    },
}

/// Which of the pair was wrong and how. Every seam that reads the pair uses
/// this one decoder, so a refusal reads the same off an upload's headers and
/// off a stored object's metadata (metsuke-jfb.41).
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("{header} header is missing")]
    Missing { header: &'static str },
    #[error("{header} is not hex")]
    NotHex { header: &'static str },
    #[error(
        "a {key} byte key with a {signature} byte signature is neither an \
         Ed25519 pair ({VKEY_BYTES}/{SIGNATURE_BYTES}) nor a Leios one \
         ({leios_key}/{leios_signature})",
        leios_key = leios::PUBLIC_KEY_BYTES,
        leios_signature = leios::SIGNATURE_BYTES,
    )]
    NoSuchScheme { key: usize, signature: usize },
    #[error("{HEADER_VKEY} is not an Ed25519 verification key: {reason}")]
    Vkey { reason: String },
    #[error(transparent)]
    Leios(#[from] leios::LeiosKeyError),
}

impl Attestation {
    /// The pair as a download's headers, in the encoding `decode` reads.
    pub fn headers(&self) -> [(&'static str, String); 2] {
        [
            (HEADER_VKEY, crate::hex::encode(&self.key_bytes())),
            (
                HEADER_SIGNATURE,
                crate::hex::encode(&self.signature_bytes()),
            ),
        ]
    }

    /// The verification key as it travels.
    pub fn key_bytes(&self) -> Vec<u8> {
        match self {
            Attestation::ColdKey { vkey, .. } => vkey.as_bytes().to_vec(),
            Attestation::LeiosKey { key, .. } => key.to_bytes().to_vec(),
        }
    }

    /// The signature as it travels.
    pub fn signature_bytes(&self) -> Vec<u8> {
        match self {
            Attestation::ColdKey { signature, .. } => signature.to_bytes().to_vec(),
            Attestation::LeiosKey { signature, .. } => signature.to_bytes().to_vec(),
        }
    }

    /// The pool this attestation names on its own. `None` under a **Leios
    /// Key**, which names none: a caller holding one has to look it up and
    /// cannot mistake the absence for a pool.
    pub fn attributes(&self) -> Option<PoolId> {
        match self {
            Attestation::ColdKey { vkey, .. } => Some(PoolId::from_cold_key(vkey)),
            Attestation::LeiosKey { .. } => None,
        }
    }

    /// Whether the signature stands over `message` as given. Ed25519 goes
    /// through `verify_strict`, which rejects signatures that only pass under
    /// malleable or mixed-order-point interpretations.
    pub fn verifies(&self, message: &[u8]) -> bool {
        match self {
            Attestation::ColdKey { vkey, signature } => {
                vkey.verify_strict(message, signature).is_ok()
            }
            Attestation::LeiosKey { key, signature } => signature.verifies(message, key),
        }
    }

    /// The pair as two hex strings, whichever scheme they are.
    pub fn decode(
        vkey: Option<&str>,
        signature: Option<&str>,
    ) -> Result<Attestation, AttestationError> {
        let key = decode_hex(vkey, HEADER_VKEY)?;
        let signature = decode_hex(signature, HEADER_SIGNATURE)?;
        match (key.len(), signature.len()) {
            (VKEY_BYTES, SIGNATURE_BYTES) => Ok(Attestation::ColdKey {
                vkey: VerifyingKey::from_bytes(&sized(&key)).map_err(|error| {
                    AttestationError::Vkey {
                        reason: error.to_string(),
                    }
                })?,
                signature: Signature::from_bytes(&sized(&signature)),
            }),
            (leios::PUBLIC_KEY_BYTES, leios::SIGNATURE_BYTES) => Ok(Attestation::LeiosKey {
                key: LeiosPublicKey::from_bytes(&sized(&key))?,
                signature: LeiosSignature::from_bytes(&sized(&signature))?,
            }),
            (key, signature) => Err(AttestationError::NoSuchScheme { key, signature }),
        }
    }

    /// The pair off an answer's head. `None` where either header is absent or
    /// unreadable: what an unverifiable object means is the caller's to say,
    /// and on this path a download is not refused for it.
    pub fn from_headers(vkey: Option<&str>, signature: Option<&str>) -> Option<Attestation> {
        Attestation::decode(Some(vkey?), Some(signature?)).ok()
    }
}

/// The key an Agent signs with. Which one it holds is what its key file said
/// it was, and it is the only thing that decides which scheme a submission
/// goes out under (`metsuke::keys`).
pub enum SubmissionKey {
    ColdKey(SigningKey),
    LeiosKey(LeiosSigningKey),
}

impl SubmissionKey {
    /// The signature over these bytes, beside the key that made it: the two
    /// halves an upload presents, produced together so they cannot disagree.
    pub fn attest(&self, wire_bytes: &[u8]) -> Attestation {
        match self {
            SubmissionKey::ColdKey(key) => {
                use ed25519_dalek::Signer;
                Attestation::ColdKey {
                    vkey: key.verifying_key(),
                    signature: key.sign(wire_bytes),
                }
            }
            SubmissionKey::LeiosKey(key) => Attestation::LeiosKey {
                key: key.public_key(),
                signature: key.sign(wire_bytes),
            },
        }
    }

    /// The pool this key speaks for on its own, which only a cold key does.
    pub fn attributes(&self) -> Option<PoolId> {
        match self {
            SubmissionKey::ColdKey(key) => Some(PoolId::from_cold_key(&key.verifying_key())),
            SubmissionKey::LeiosKey(_) => None,
        }
    }

    /// The public half, hex, as it goes in `HEADER_VKEY`. What a test or a log
    /// line names a loaded key by, since neither may see the other half.
    pub fn public_key_hex(&self) -> String {
        match self {
            SubmissionKey::ColdKey(key) => crate::hex::encode(key.verifying_key().as_bytes()),
            SubmissionKey::LeiosKey(key) => crate::hex::encode(&key.public_key().to_bytes()),
        }
    }
}

impl std::fmt::Debug for SubmissionKey {
    /// The scheme and the public half. A signing key that renders itself into
    /// a log line or a test failure is a signing key in the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = match self {
            SubmissionKey::ColdKey(_) => "ColdKey",
            SubmissionKey::LeiosKey(_) => "LeiosKey",
        };
        f.debug_tuple(scheme).field(&self.public_key_hex()).finish()
    }
}

/// Hex to bytes, carrying which header was wrong. The width is not fixed here:
/// which scheme the pair is comes from what the widths turn out to be.
fn decode_hex(value: Option<&str>, header: &'static str) -> Result<Vec<u8>, AttestationError> {
    let value = value.ok_or(AttestationError::Missing { header })?;
    crate::hex::decode_bytes(value).map_err(|_| AttestationError::NotHex { header })
}

/// The slice as the array its length already is.
fn sized<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes.try_into().expect("N bytes, as the match arm says")
}

/// The server's answer to an accepted upload. `latest_version` is the
/// client-crate version embedded at server build (ADR 0006).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    pub latest_version: String,
}

/// Which Agent reported a submission: lowercase ASCII alphanumerics in
/// dash-separated runs. Two constructors, because the two callers want
/// different things from a name. `slugify` turns any hostname into an id, so a
/// host called `Relay_1` reports instead of refusing to start, and `parse`
/// takes only the form `slugify` emits, so an id read off the wire is an id
/// something made (`slugify_folds_a_hostname_into_an_agent_id`,
/// `parse_refuses_what_slugify_would_never_emit`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);

#[derive(Debug, thiserror::Error)]
pub enum AgentIdError {
    #[error("{name:?} holds no letter or digit to name an agent by")]
    Nameless { name: String },
    #[error("agent id {found:?} is not dash-separated runs of a-z and 0-9")]
    NotASlug { found: String },
}

impl AgentId {
    /// Only a name with nothing alphanumeric in it leaves no id to make.
    pub fn slugify(name: &str) -> Result<AgentId, AgentIdError> {
        let dashed: String = name
            .chars()
            .map(|character| match character.to_ascii_lowercase() {
                lowered if lowered.is_ascii_alphanumeric() => lowered,
                _ => '-',
            })
            .collect();
        let slug = dashed
            .split('-')
            .filter(|run| !run.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        match slug.is_empty() {
            true => Err(AgentIdError::Nameless {
                name: name.to_string(),
            }),
            false => Ok(AgentId(slug)),
        }
    }

    /// The strict reader: `slugify` itself, required to have folded nothing.
    pub fn parse(text: &str) -> Result<AgentId, AgentIdError> {
        match AgentId::slugify(text) {
            Ok(slug) if slug.0 == text => Ok(slug),
            _ => Err(AgentIdError::NotASlug {
                found: text.to_string(),
            }),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AgentId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        AgentId::parse(&text).map_err(serde::de::Error::custom)
    }
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
    #[error("not 28 bytes of hex: {0}")]
    Hex(#[from] crate::hex::HexError),
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

    /// The hex form, which is how the chain's own tooling answers: the key of
    /// a `cardano-cli query pool-state` map (ADR 0011).
    pub fn from_hex(text: &str) -> Result<Self, PoolIdError> {
        Ok(PoolId(crate::hex::decode::<28>(text)?))
    }

    /// The pool id a cold verification key hashes to. See CONTEXT.md,
    /// **Cold Key**.
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

/// The one key a payload line reserves for metsuke's own fields; why one key
/// rather than a flat merge is ADR 0010.
pub const PROVENANCE_KEY: &str = "metsuke";

/// What every payload line carries under `PROVENANCE_KEY`: which pool and
/// Agent wrote the line, so a line read out of the archive on its own still
/// says where it came from.
///
/// The two values an agent knows before it spools a line, and no more. The
/// submission's counter and timestamp are not among them: both are drawn when a submission
/// is sealed, and a row whose upload failed is sealed into a later one, so a line
/// stamped with either would name a submission it did not travel in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub pool_id: PoolId,
    pub agent_id: AgentId,
}

/// One payload line as it goes on the wire and as the spool holds it: a
/// payload value's own JSON object with this agent's provenance under the one
/// reserved key, rendered once, here.
///
/// Rendered at the write end, never read back to send. What that buys is
/// ADR 0010.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadLine(String);

impl PayloadLine {
    /// Stamp one scrape.
    pub fn scrape(scrape: &Scrape, provenance: &Provenance) -> Result<PayloadLine, SealError> {
        PayloadLine::stamp(scrape, provenance)
    }

    /// Stamp one selected trace line. `TraceLine`'s own invariant is that it
    /// holds no `PROVENANCE_KEY`, so stamping one overwrites nothing the node
    /// said.
    pub fn trace_line(line: &TraceLine, provenance: &Provenance) -> Result<PayloadLine, SealError> {
        PayloadLine::stamp(line, provenance)
    }

    fn stamp<T: Serialize>(line: &T, provenance: &Provenance) -> Result<PayloadLine, SealError> {
        let metsuke = serde_json::value::to_raw_value(provenance)?;
        Ok(PayloadLine(serde_json::to_string(&Stamped {
            line,
            metsuke: &metsuke,
        })?))
    }

    /// A line this build already stamped, as the spool stored it. The spool is
    /// not a sender: what it hands back is text this crate wrote, so it is
    /// taken as the line it is rather than parsed to prove it.
    pub fn spooled(text: String) -> PayloadLine {
        PayloadLine(text)
    }

    /// The line without the newline that terminates it on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// What this line costs a sealed payload: its own bytes and the newline
    /// after them. The one place a row's wire cost is computed, so the spool's
    /// stored `bytes` and a submission's budget cannot disagree (metsuke-jfb.9).
    pub fn wire_bytes(&self) -> u64 {
        self.0.len() as u64 + 1
    }

    /// The bytes `wire_bytes` counts, appended. Beside it so the framing is one
    /// edit rather than two.
    fn write_wire(&self, body: &mut Vec<u8>) {
        body.extend_from_slice(self.0.as_bytes());
        body.push(b'\n');
    }
}

/// One trace line as the JSON object the node wrote, with `PROVENANCE_KEY`
/// absent: a held `TraceLine` is a line the sealing path can stamp without
/// overwriting anything the node said.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceLine(serde_json::Map<String, serde_json::Value>);

#[derive(Debug, thiserror::Error)]
pub enum TraceLineError {
    #[error("not one whole JSON object: {0}")]
    NotAnObject(#[from] serde_json::Error),
    #[error("declares the reserved {PROVENANCE_KEY:?} key")]
    ReservedKey,
}

impl TraceLine {
    pub fn parse(line: &str) -> Result<TraceLine, TraceLineError> {
        TraceLine::of(serde_json::from_str(line)?)
    }

    fn of(object: serde_json::Map<String, serde_json::Value>) -> Result<TraceLine, TraceLineError> {
        match object.contains_key(PROVENANCE_KEY) {
            true => Err(TraceLineError::ReservedKey),
            false => Ok(TraceLine(object)),
        }
    }

    /// What the line reads back as, and what it is measured and spooled as:
    /// the object, compact, on one line.
    pub fn to_line(&self) -> String {
        // Every value came out of a parse, so there is no non-finite float and
        // no non-string key for the writer to refuse.
        serde_json::to_string(&self.0).expect("a parsed JSON object re-renders")
    }

    /// One of the line's own top-level fields, for the rules that read it.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }
}

impl Serialize for TraceLine {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// What one submission carries: its lines, already stamped, and which schema they
/// are. Both constructors set the version from the shape they were given, so an
/// envelope never states the two apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    schema_version: u32,
    lines: Vec<PayloadLine>,
}

impl Payload {
    pub fn scrapes(lines: Vec<PayloadLine>) -> Payload {
        Payload {
            schema_version: SCHEMA_VERSION_SCRAPES,
            lines,
        }
    }

    pub fn trace_lines(lines: Vec<PayloadLine>) -> Payload {
        Payload {
            schema_version: SCHEMA_VERSION_LINES,
            lines,
        }
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

/// One signed submission. The `counter` and `timestamp` live in the header
/// frame, inside the signed bytes, where a consumer reads them without
/// inflating the payload.
///
/// `schema_version` and `payload` are private and only `new` sets them: an
/// envelope in hand always declares the version its payload has.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    schema_version: u32,
    /// Which pool and which of its Agents sealed the submission, and what every
    /// line of it is stamped with.
    pub provenance: Provenance,
    pub agent_version: String,
    /// Per-agent monotonic counter: a gap in one agent's run of it is a submission
    /// the archive never got.
    pub counter: u64,
    /// Submission creation time, RFC 3339 UTC.
    pub timestamp: OffsetDateTime,
    payload: Payload,
}

/// The skippable frame's content: everything about a submission that is not
/// the payload itself. It holds no payload key, so which schema a submission
/// declares is answerable without inflating a byte, which is what
/// `read_header` is for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub schema_version: u32,
    /// Flattened, so the header's keys stay where they were before the two
    /// fields became one (`the_header_renders_the_same_bytes_as_it_always_has`).
    #[serde(flatten)]
    pub provenance: Provenance,
    pub agent_version: String,
    pub counter: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

/// Which of this build's two schemas a declared version names.
enum Schema {
    Scrapes,
    Lines,
}

impl Schema {
    fn of(version: u32) -> Result<Schema, OpenError> {
        match version {
            SCHEMA_VERSION_SCRAPES => Ok(Schema::Scrapes),
            SCHEMA_VERSION_LINES => Ok(Schema::Lines),
            found => Err(OpenError::UnsupportedSchemaVersion { found }),
        }
    }
}

impl Envelope {
    pub fn new(
        provenance: Provenance,
        agent_version: String,
        counter: u64,
        timestamp: OffsetDateTime,
        payload: Payload,
    ) -> Envelope {
        Envelope {
            schema_version: payload.schema_version(),
            provenance,
            agent_version,
            counter,
            timestamp,
            payload,
        }
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The submission's scrapes, for a reader that wants the fields rather than the
    /// bytes. A consumer's call, never the agent's (`PayloadLine`).
    pub fn scrapes(&self) -> Result<Vec<Scrape>, ReadError> {
        self.read(SCHEMA_VERSION_SCRAPES)
    }

    /// The same for a trace-line submission.
    pub fn trace_lines(&self) -> Result<Vec<TraceLine>, ReadError> {
        Ok(self
            .read::<serde_json::Map<String, serde_json::Value>>(SCHEMA_VERSION_LINES)?
            .into_iter()
            // `Unstamped` consumed the reserved key, so nothing here can hold
            // one and `TraceLine`'s invariant survives the round trip.
            .map(TraceLine)
            .collect())
    }

    fn read<T: serde::de::DeserializeOwned>(&self, asked: u32) -> Result<Vec<T>, ReadError> {
        if self.schema_version != asked {
            return Err(ReadError::PayloadIsNot {
                asked,
                found: self.schema_version,
            });
        }
        self.payload
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_str::<Unstamped<T>>(line.as_str())
                    .map(|unstamped| unstamped.line)
                    .map_err(|source| ReadError::LineFields { index, source })
            })
            .collect()
    }
}

impl Header {
    fn of(envelope: &Envelope) -> Header {
        Header {
            schema_version: envelope.schema_version,
            provenance: envelope.provenance.clone(),
            agent_version: envelope.agent_version.clone(),
            counter: envelope.counter,
            timestamp: envelope.timestamp,
        }
    }
}

/// One **Scrape** (CONTEXT.md) as it goes on the wire. The agent never sends
/// both a `failure` and a metric, and does send an empty `metrics` with no
/// failure, for a body that yielded no metric. Making the first
/// unrepresentable rather than a habit is metsuke-uxw.7.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scrape {
    /// Scrape time, RFC 3339 UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub scraped_at: OffsetDateTime,
    pub clock_offset_ms: Option<i64>,
    pub failure: Option<Failure>,
    /// Whatever the endpoint stated, in the order it stated it.
    pub metrics: Vec<Metric>,
}

/// One metric as the endpoint wrote it. `value` is a JSON number, so a counter
/// past an f64's exact-integer range keeps its digits; `declared_type` is
/// `None` when the body carried no `# TYPE` line for the name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub labels: std::collections::BTreeMap<String, String>,
    pub value: serde_json::Number,
    pub declared_type: Option<String>,
}

/// Why a scrape carries no metrics. `reason` is what a consumer groups by and
/// `detail` is the message the agent had, which names the port, the status or
/// the limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub reason: Reason,
    pub detail: String,
}

/// What stopped a scrape, as few cases as the agent can actually tell apart.
/// Serialized as its own name in snake case, so grouping the archive by it is a
/// string comparison and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Nothing answered on the endpoint's port.
    Unreachable,
    /// The endpoint answered, and not with metrics.
    Refused,
    /// The body was past the agent's configured limit.
    TooLarge,
    /// The answer began and broke off before its body was read.
    Unreadable,
}

impl Reason {
    /// Every case, for the callers that have to cover all of them. The
    /// instructions page renders its list from here. Complete or the crate does
    /// not build: see the const assertion below it.
    pub const ALL: [Reason; 4] = [
        Reason::Unreachable,
        Reason::Refused,
        Reason::TooLarge,
        Reason::Unreadable,
    ];

    /// The case after this one, `None` at the end. This chain is what says how
    /// many cases there are: the match is exhaustive, so a new variant is a
    /// compile error here, beside the array it has to join.
    const fn after(self) -> Option<Reason> {
        match self {
            Reason::Unreachable => Some(Reason::Refused),
            Reason::Refused => Some(Reason::TooLarge),
            Reason::TooLarge => Some(Reason::Unreadable),
            Reason::Unreadable => None,
        }
    }
}

/// `ALL` is the whole chain, in its order. A case linked into `after` but left
/// out of the array walks past its end, which is a build error, and a reordering
/// fails the assertion.
const _: () = {
    let mut at = 0;
    let mut case = Some(Reason::ALL[0]);
    while let Some(reason) = case {
        assert!(
            reason as usize == Reason::ALL[at] as usize,
            "Reason::ALL is out of order"
        );
        case = reason.after();
        at += 1;
    }
    assert!(at == Reason::ALL.len(), "Reason::ALL is missing a case");
};

/// The two bounds `open` puts on bytes it did not produce. Both are server
/// configuration (`IngestConfig`); this crate holds no default for either.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_header_bytes: u64,
    pub max_decompressed_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zstd compression failed: {0}")]
    Compress(#[source] std::io::Error),
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
    #[error("signature does not verify over the bytes as given")]
    Signature,
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
    /// Every line states the submission it travelled in. A line stating another one
    /// is a payload assembled from two of them, and a reader taking provenance
    /// off the line rather than the header would never notice.
    #[error("payload line {index} does not carry this submission's provenance")]
    LineProvenance { index: usize },
    /// Named by index, because serde's own position is inside the one line it
    /// was handed and says "line 1" for every one of them.
    #[error("payload line {index} carries no readable {PROVENANCE_KEY:?} stamp: {source}")]
    LineStamp {
        index: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "envelope schema version {found}, this build speaks \
         v{SCHEMA_VERSION_SCRAPES} and v{SCHEMA_VERSION_LINES}"
    )]
    UnsupportedSchemaVersion { found: u32 },
}

/// Why reading a submission's lines as their fields failed
/// (`Envelope::scrapes`, `Envelope::trace_lines`).
///
/// Its own error rather than an `OpenError`: a submission is accepted and
/// archived without anything reading a payload line's own fields, so these are
/// a consumer's failures and never an ingest path's.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The submission is fine and the question was wrong.
    #[error("payload is schema v{found}, read as v{asked}")]
    PayloadIsNot { asked: u32, found: u32 },
    /// Named by index, because serde's own position is inside the one line it
    /// was handed and says "line 1" for every one of them.
    #[error("payload line {index} does not read as this schema's shape: {source}")]
    LineFields {
        index: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// A submission's two frames, borrowed from the bytes as received.
pub struct Frames<'a> {
    /// The skippable frame's content: the header JSON, uncompressed.
    pub header: &'a [u8],
    /// The zstd data frame, still compressed.
    pub data: &'a [u8],
}

/// Why a body's header frame is not one this crate wrote. Separate from
/// `OpenError` because reading a header costs no key and no decompressor.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error(transparent)]
    Container(#[from] ContainerError),
    #[error("header frame is not a submission header: {0}")]
    Json(#[from] serde_json::Error),
}

/// The header frame's fields, read with no key and no decompressor. What an
/// ingest path files an object by: the payload stays compressed and unread.
pub fn read_header(bytes: &[u8], max_header_bytes: u64) -> Result<Header, HeaderError> {
    Ok(serde_json::from_slice(
        split(bytes, max_header_bytes)?.header,
    )?)
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

/// The header frame's content, uncompressed. The agent budgets a submission against
/// this length rather than against a second account of the header's fields.
pub fn header_json(envelope: &Envelope) -> Result<Vec<u8>, SealError> {
    Ok(serde_json::to_vec(&Header::of(envelope))?)
}

/// One payload line as it goes on the wire: the scrape's or the node's own
/// object, plus this submission's provenance under the one reserved key. The field
/// name spells `PROVENANCE_KEY`'s value a second time because `serde(rename)`
/// takes a literal; the tests assert against the constant, so a change to
/// either alone fails `every_payload_line_carries_the_submission_s_provenance`.
#[derive(Serialize)]
struct Stamped<'a, T: Serialize> {
    #[serde(flatten)]
    line: &'a T,
    metsuke: &'a serde_json::value::RawValue,
}

/// The same line coming back as whatever the schema says its own fields are.
/// The stamp was checked when the line was received (`stamped`); naming it here
/// only keeps the reserved key from landing among `line`'s fields.
#[derive(Deserialize)]
struct Unstamped<T> {
    #[serde(flatten)]
    line: T,
    #[allow(dead_code, reason = "named to consume the key, never read")]
    metsuke: serde::de::IgnoredAny,
}

/// The data frame's content before compression: the submission's lines, each
/// newline-terminated. What `Limits::max_decompressed_bytes` bounds, and what
/// `zstd -d` emits. Both payload shapes make the same line; ADR 0010 says
/// why.
///
/// Concatenation, because a line was stamped and rendered where it was written
/// (`PayloadLine`). Nothing is serialized here, so a submission's bytes are the sum
/// of what its rows already measured.
pub fn payload_lines(envelope: &Envelope) -> Vec<u8> {
    let mut body = Vec::with_capacity(
        envelope
            .payload
            .lines
            .iter()
            .map(|line| line.wire_bytes() as usize)
            .sum(),
    );
    for line in &envelope.payload.lines {
        line.write_wire(&mut body);
    }
    body
}

/// A short digest of exactly the bytes `payload_lines` produces, which is what
/// `zstd -d` emits for a stored object, so a consumer can recompute it from the
/// archive rather than trust a log line.
///
/// The header is deliberately not covered. A submission the server did not take
/// is resealed under a fresh counter and timestamp, so everything else about it
/// changes: the bytes, their length and the signature. Its rows do not, and
/// this is what says so.
pub fn payload_digest(envelope: &Envelope) -> String {
    use blake2::digest::consts::U8;
    use blake2::{Blake2b, Digest};
    crate::hex::encode(&Blake2b::<U8>::digest(payload_lines(envelope))[..])
}

/// Serialize, compress, and sign an envelope. Returns the wire bytes exactly
/// as they must be sent and archived, plus the detached signature over them.
/// `level` is the zstd compression level (0 = zstd's default).
pub fn seal(
    key: &SubmissionKey,
    envelope: &Envelope,
    level: i32,
) -> Result<(Vec<u8>, Attestation), SealError> {
    let header = header_json(envelope)?;
    let declared = u32::try_from(header.len()).map_err(|_| SealError::HeaderTooLarge {
        found: header.len(),
    })?;
    let data =
        zstd::encode_all(payload_lines(envelope).as_slice(), level).map_err(SealError::Compress)?;

    let mut wire_bytes = Vec::with_capacity(HEADER_OFFSET + header.len() + data.len());
    wire_bytes.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
    wire_bytes.extend_from_slice(&declared.to_le_bytes());
    wire_bytes.extend_from_slice(&header);
    wire_bytes.extend_from_slice(&data);

    let attestation = key.attest(&wire_bytes);
    Ok((wire_bytes, attestation))
}

/// How much decompressed output `open` copies per read. Granularity, not a
/// limit: it bounds the scratch buffer, never what is accepted.
const DECOMPRESS_CHUNK_BYTES: usize = 64 * 1024;

/// Verify the signature over the wire bytes as received, then read the header
/// out of the skippable frame and decompress the data frame, refusing to
/// inflate past `limits.max_decompressed_bytes`. Uses `verify_strict` to
/// reject signatures that only pass under malleable or mixed-order-point
/// interpretations.
pub fn open(
    attestation: &Attestation,
    wire_bytes: &[u8],
    limits: Limits,
) -> Result<Envelope, OpenError> {
    if !attestation.verifies(wire_bytes) {
        return Err(OpenError::Signature);
    }
    let frames = split(wire_bytes, limits.max_header_bytes)?;
    // The version alone first, so a version this build never spoke is named as
    // such whatever else its header holds. A v3 that dropped a field every
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
    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| stamped(index, line, &header.provenance))
        .collect::<Result<Vec<PayloadLine>, OpenError>>()?;
    let payload = match schema {
        Schema::Scrapes => Payload::scrapes(lines),
        Schema::Lines => Payload::trace_lines(lines),
    };
    // Through `new`, so what comes out declares the version its payload has
    // rather than the one the header claimed.
    Ok(Envelope::new(
        header.provenance,
        header.agent_version,
        header.counter,
        header.timestamp,
        payload,
    ))
}

/// Check one received line declares the provenance the header does, and take it
/// as it stands.
///
/// Only the reserved key is read. A line's own fields are the schema's business,
/// and what a consumer makes of them is `Envelope::scrapes`. Reading them here
/// would make every archived line's fate depend on the payload structs of
/// whichever build opened it.
fn stamped(index: usize, line: &str, stamp: &Provenance) -> Result<PayloadLine, OpenError> {
    let peek: StampPeek =
        serde_json::from_str(line).map_err(|source| OpenError::LineStamp { index, source })?;
    match &peek.metsuke == stamp {
        true => Ok(PayloadLine(line.to_string())),
        false => Err(OpenError::LineProvenance { index }),
    }
}

/// One line's stamp and nothing else: serde ignores every field this does not
/// name, which is what keeps the check schema-agnostic.
#[derive(Deserialize)]
struct StampPeek {
    metsuke: Provenance,
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
