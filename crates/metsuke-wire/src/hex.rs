//! Hex, fixed-width where a width is fixed: `decode` names the byte count it
//! needs, so callers get an array and never a length check of their own.

#[derive(Debug, thiserror::Error)]
pub enum HexError {
    #[error("not pairs of hex digits")]
    NotHex,
    #[error("decodes to {found} bytes, expected {expected}")]
    WrongLength { found: usize, expected: usize },
}

pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Decode exactly `N` bytes. Uppercase digits decode too: `encode` emits
/// lowercase, but refusing the other casing would only turn a verifiable
/// upload into a rejection.
pub fn decode<const N: usize>(text: &str) -> Result<[u8; N], HexError> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return Err(HexError::NotHex);
    }
    if digits.len() / 2 != N {
        return Err(HexError::WrongLength {
            found: digits.len() / 2,
            expected: N,
        });
    }
    let bytes = decode_bytes(text)?;
    Ok(bytes.try_into().expect("N bytes, as the length check says"))
}

/// Decode however many bytes the text holds, for what nothing fixes a width
/// for: a CIP-151 registration is as long as its witnesses make it.
pub fn decode_bytes(text: &str) -> Result<Vec<u8>, HexError> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return Err(HexError::NotHex);
    }
    digits
        .chunks(2)
        .map(|pair| {
            // `from_str_radix` would take a sign character; only digits are hex.
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return Err(HexError::NotHex);
            }
            let pair = std::str::from_utf8(pair).expect("ascii hex digits are utf8");
            u8::from_str_radix(pair, 16).map_err(|_| HexError::NotHex)
        })
        .collect()
}
