//! Helpers shared by the integration tests that speak the upload wire
//! format; a `tests/support` module so cargo doesn't build it as its own
//! test target.

use metsuke::envelope::SigningKey;

/// The all-sevens test seed used across the suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// Decode the lowercase hex the upload headers carry.
pub fn decode_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
