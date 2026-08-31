//! The Leios key as this project signs and verifies with it: BLS12-381,
//! minimal-signature-size, under `DST` and never under the consensus tag
//! (ADR 0011). Every constructor validates, so a `LeiosPublicKey` in hand is a
//! subgroup element and not a hostile encoding.

use blst::BLST_ERROR;
use blst::min_sig::{PublicKey, SecretKey, Signature};

/// This project's domain separation tag. It is the standard proof-of-possession
/// ciphersuite string with our own prefix, so a signature made here is not a
/// signature in the scheme the node votes with, and one made there is not a
/// signature here.
pub const DST: &[u8] = b"METSUKE_SUBMISSION_V1_BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_";

/// Compressed sizes for the minimal-signature-size variant: the public key is
/// the G2 point and the signature the G1 one, which is the way round Leios
/// picked (`spsLeiosKey.leiosPubKey` is 96 bytes).
pub const PUBLIC_KEY_BYTES: usize = 96;
pub const SIGNATURE_BYTES: usize = 48;

/// The 32-byte scalar a `BlsSigningKey_bls12-381-…` TextEnvelope holds.
pub const SIGNING_KEY_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
#[error("not a BLS12-381 minimal-signature-size {what}: {reason:?}")]
pub struct LeiosKeyError {
    what: &'static str,
    reason: BLST_ERROR,
}

fn refuse(what: &'static str) -> impl Fn(BLST_ERROR) -> LeiosKeyError {
    move |reason| LeiosKeyError { what, reason }
}

/// A pool's registered Leios verification key, as `pool-state` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeiosPublicKey(PublicKey);

impl LeiosPublicKey {
    /// Rejects the infinity point and anything outside the subgroup, which is
    /// what makes holding one mean something.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_BYTES]) -> Result<Self, LeiosKeyError> {
        PublicKey::key_validate(bytes)
            .map(LeiosPublicKey)
            .map_err(refuse("verification key"))
    }

    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_BYTES] {
        self.0.to_bytes()
    }
}

/// One detached signature over a submission's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeiosSignature(Signature);

impl LeiosSignature {
    pub fn from_bytes(bytes: &[u8; SIGNATURE_BYTES]) -> Result<Self, LeiosKeyError> {
        Signature::sig_validate(bytes, true)
            .map(LeiosSignature)
            .map_err(refuse("signature"))
    }

    pub fn to_bytes(&self) -> [u8; SIGNATURE_BYTES] {
        self.0.to_bytes()
    }

    /// Whether this signature stands over `message` under `key`. The group
    /// checks are asked for again here: a signature that reached this call any
    /// way other than `from_bytes` has not had them.
    pub fn verifies(&self, message: &[u8], key: &LeiosPublicKey) -> bool {
        self.0.verify(true, message, DST, &[], &key.0, true) == BLST_ERROR::BLST_SUCCESS
    }
}

/// The signing half, which only an Agent holds.
pub struct LeiosSigningKey(SecretKey);

impl LeiosSigningKey {
    pub fn from_bytes(bytes: &[u8; SIGNING_KEY_BYTES]) -> Result<Self, LeiosKeyError> {
        SecretKey::from_bytes(bytes)
            .map(LeiosSigningKey)
            .map_err(refuse("signing key"))
    }

    pub fn public_key(&self) -> LeiosPublicKey {
        LeiosPublicKey(self.0.sk_to_pk())
    }

    pub fn sign(&self, message: &[u8]) -> LeiosSignature {
        LeiosSignature(self.0.sign(message, DST, &[]))
    }
}

impl std::fmt::Debug for LeiosSigningKey {
    /// The public half only: a signing key that renders itself into a log line
    /// is a signing key in the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LeiosSigningKey")
            .field(&self.public_key())
            .finish()
    }
}
