//! Who a submission is from. One answer, derived and not claimed: the pool is
//! the blake2b-224 hash of the key that signed, so nothing on the
//! wire has to be believed and nothing has to be looked up.

use metsuke_wire::envelope::{PoolId, Signature, VerifyingKey};

/// An upload as it was signed: the key it presents, the signature, and the
/// bytes both are over. The ingest path builds one from the request headers,
/// the audit from a stored object.
pub struct Signed<'a> {
    pub vkey: VerifyingKey,
    pub signature: Signature,
    /// The body as sent, byte for byte as received.
    pub wire_bytes: &'a [u8],
}

impl Signed<'_> {
    /// Whose submission this is. The one derivation, so the pool the allowlist
    /// is checked against, the pool the limiter charges and the pool the object
    /// is filed under cannot come out three different pools.
    pub fn pool_id(&self) -> PoolId {
        PoolId::from_cold_key(&self.vkey)
    }

    /// Whether the signature stands over the bytes as given. `verify_strict`
    /// rejects signatures that only pass under malleable or mixed-order-point
    /// interpretations.
    pub fn verifies(&self) -> bool {
        self.vkey
            .verify_strict(self.wire_bytes, &self.signature)
            .is_ok()
    }
}
