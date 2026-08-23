//! Helpers shared by the integration tests that speak the upload wire
//! format; a `tests/support` module so cargo doesn't build it as its own
//! test target.

use metsuke_wire::envelope::SigningKey;

/// The all-sevens test seed used across the suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}
