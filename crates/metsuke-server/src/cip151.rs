//! What separates a pool's own CIP-151 registration from a stranger's claim
//! about that pool: the cold-key witness inside the label-867 metadata, checked
//! here and nowhere else (ADR 0008). `Registration` has no other constructor,
//! so nothing above this module can hand up a row that did not pass.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use ciborium::Value;
use ed25519_dalek::Verifier;
use metsuke_wire::envelope::{PoolId, Signature, VerifyingKey};

/// The CIP-88 label a registration is posted under. `tx_metadata.bytes` carries
/// the whole `{label: registration}` map, not the registration alone.
const METADATA_LABEL: u64 = 867;
/// CIP-151 is CIP-88 at version 2. Version 1 signs different bytes, so a
/// verifier that accepted it would be checking the wrong thing.
const VERSION: u64 = 2;

const KEY_VERSION: u64 = 0;
const KEY_PAYLOAD: u64 = 1;
const KEY_WITNESSES: u64 = 2;

/// Payload keys, in the numeric order CIP-151 fixes for hashing.
const KEY_SCOPE: u64 = 1;
const KEY_NONCE: u64 = 4;
const KEY_CALIDUS: u64 = 7;
/// Scope 1 is the stake-pool scope; its second element is the pool id.
const SCOPE_POOL: u64 = 1;

/// CIP-8 witness keys: the COSE_Key holding the public key, and the COSE_Sign1
/// holding the signature.
const KEY_COSE_KEY: u64 = 1;
const KEY_COSE_SIGN1: u64 = 2;
/// COSE_Key label -2 is an OKP key's public half (RFC 8152 §13.2).
const COSE_KEY_X: i64 = -2;

/// One CIP-151 registration whose witness has been checked against the pool it
/// names. `key` is the 32 bytes as registered rather than a `VerifyingKey`: the
/// witness proves the operator posted them, not that they are a curve point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    nonce: u64,
    key: [u8; 32],
}

impl Registration {
    pub fn nonce(&self) -> u64 {
        self.nonce
    }

    pub fn key(&self) -> [u8; 32] {
        self.key
    }
}

/// Why a label-867 row is not this pool's registration. Every variant is a row
/// to drop: anyone who pays for a transaction can post one, so none of them is
/// the pool's mistake or worth naming in a log.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("not CBOR: {0}")]
    NotCbor(String),
    #[error("no {METADATA_LABEL} entry holding a registration map")]
    NotARegistration,
    #[error("registration version is not {VERSION}")]
    WrongVersion,
    #[error("payload is not a map holding a scope, a nonce and a Calidus key")]
    MalformedPayload,
    #[error("payload scopes another pool")]
    ScopesAnotherPool,
    #[error("no witness both binds to the pool and signs the payload")]
    Unwitnessed,
}

/// The registration a label-867 row makes for `pool_id`, or why it makes none.
///
/// The check the indexers omit is here: a witness speaks for the pool only if
/// its public key hashes to the pool id in the payload scope. Without it, a
/// self-signed registration naming any pool would grant that pool's upload
/// rights to whoever posted it.
pub fn verify(pool_id: PoolId, blob: &[u8]) -> Result<Registration, RegistrationError> {
    let metadata: Value = ciborium::from_reader(blob)
        .map_err(|error| RegistrationError::NotCbor(error.to_string()))?;
    let registration =
        entry(&metadata, METADATA_LABEL).ok_or(RegistrationError::NotARegistration)?;

    let version = entry(registration, KEY_VERSION).and_then(unsigned);
    if version != Some(VERSION) {
        return Err(RegistrationError::WrongVersion);
    }

    let payload = entry(registration, KEY_PAYLOAD).ok_or(RegistrationError::MalformedPayload)?;
    let scope = entry(payload, KEY_SCOPE)
        .and_then(pool_scope)
        .ok_or(RegistrationError::MalformedPayload)?;
    let nonce = entry(payload, KEY_NONCE)
        .and_then(unsigned)
        .ok_or(RegistrationError::MalformedPayload)?;
    let key: [u8; 32] = entry(payload, KEY_CALIDUS)
        .and_then(bytes)
        .and_then(|found| found.as_slice().try_into().ok())
        .ok_or(RegistrationError::MalformedPayload)?;

    if scope != pool_id.as_hash() {
        return Err(RegistrationError::ScopesAnotherPool);
    }

    let signed = payload_hash(payload).ok_or(RegistrationError::MalformedPayload)?;
    let witnesses = entry(registration, KEY_WITNESSES)
        .and_then(Value::as_array)
        .ok_or(RegistrationError::Unwitnessed)?;
    if !witnesses
        .iter()
        .any(|witness| speaks_for(witness, pool_id, &signed))
    {
        return Err(RegistrationError::Unwitnessed);
    }
    Ok(Registration { nonce, key })
}

/// What CIP-151 v2 signs: blake2b-256 over the payload's CBOR, which is what
/// lets a hardware wallet sign a fixed-size message. Over the bytes and not
/// over their hex, against the CIP's own prose — docs/research/cip-0088-calidus.md.
///
/// Re-encoding is the direction that fails safe. An encoder that disagrees with
/// the signer's produces a hash no witness signed, so the row is dropped;
/// nothing here can turn bytes we did not decode into an accepted registration.
fn payload_hash(payload: &Value) -> Option<[u8; 32]> {
    let mut encoded = Vec::new();
    ciborium::into_writer(payload, &mut encoded).ok()?;
    Some(Blake2b::<U32>::digest(&encoded).into())
}

/// Whether one witness makes the registration the pool's: its key must hash to
/// the pool id, and it must have signed the payload hash.
fn speaks_for(witness: &Value, pool_id: PoolId, signed: &[u8; 32]) -> bool {
    let Some((key, signature, message)) = cose_witness(witness, signed) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&key) else {
        return false;
    };
    PoolId::from_cold_key(&key) == pool_id
        && key
            .verify(&message, &Signature::from_bytes(&signature))
            .is_ok()
}

/// The CIP-8 witness: a COSE_Key naming the public key and a COSE_Sign1 over
/// the payload hash. The only shape cardano-signer, and so every registration
/// recorded under tests/fixtures/calidus, produces.
fn cose_witness(witness: &Value, signed: &[u8; 32]) -> Option<([u8; 32], [u8; 64], Vec<u8>)> {
    let key: [u8; 32] = entry(witness, KEY_COSE_KEY)
        .and_then(|cose_key| labelled(cose_key, COSE_KEY_X))
        .and_then(bytes)
        .and_then(|found| found.as_slice().try_into().ok())?;

    let sign1 = entry(witness, KEY_COSE_SIGN1).and_then(Value::as_array)?;
    let [protected, _unprotected, payload, signature] = sign1.as_slice() else {
        return None;
    };
    let protected = bytes(protected)?;
    if bytes(payload)? != signed {
        return None;
    }
    let signature: [u8; 64] = bytes(signature)?.as_slice().try_into().ok()?;

    // Sig_structure = ["Signature1", protected, external_aad, payload]
    // (RFC 8152 §4.4), with an empty external_aad.
    let structure = Value::Array(vec![
        Value::Text("Signature1".to_string()),
        Value::Bytes(protected.clone()),
        Value::Bytes(Vec::new()),
        Value::Bytes(signed.to_vec()),
    ]);
    let mut message = Vec::new();
    ciborium::into_writer(&structure, &mut message).ok()?;
    Some((key, signature, message))
}

/// The value a CBOR map holds under an unsigned key.
fn entry(map: &Value, key: u64) -> Option<&Value> {
    labelled(map, i64::try_from(key).ok()?)
}

/// The value a CBOR map holds under an integer label, which COSE takes negative.
fn labelled(map: &Value, label: i64) -> Option<&Value> {
    map.as_map()?
        .iter()
        .find(|(found, _)| found.as_integer() == Some(label.into()))
        .map(|(_, value)| value)
}

fn unsigned(value: &Value) -> Option<u64> {
    u64::try_from(value.as_integer()?).ok()
}

fn bytes(value: &Value) -> Option<&Vec<u8>> {
    value.as_bytes()
}

/// The pool id a `[1, h'…']` scope names, and nothing else: another scope is a
/// registration about something that is not a pool.
fn pool_scope(scope: &Value) -> Option<&[u8; 28]> {
    let [kind, pool_id] = scope.as_array()?.as_slice() else {
        return None;
    };
    if unsigned(kind)? != SCOPE_POOL {
        return None;
    }
    bytes(pool_id)?.as_slice().try_into().ok()
}
