use metsuke_wire::hex::{self, HexError};

#[test]
fn encodes_lowercase_two_digits_per_byte() {
    assert_eq!(hex::encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
}

#[test]
fn encodes_empty_as_empty() {
    assert_eq!(hex::encode(&[]), "");
}

#[test]
fn decodes_what_encode_wrote() {
    let bytes = [7u8; 32];
    assert_eq!(hex::decode::<32>(&hex::encode(&bytes)).unwrap(), bytes);
}

#[test]
fn decodes_uppercase_digits() {
    assert_eq!(hex::decode::<2>("A0FF").unwrap(), [0xa0, 0xff]);
}

#[test]
fn rejects_an_odd_number_of_digits() {
    assert!(matches!(hex::decode::<2>("abc"), Err(HexError::NotHex)));
}

#[test]
fn rejects_a_non_hex_digit() {
    assert!(matches!(hex::decode::<2>("00zz"), Err(HexError::NotHex)));
}

#[test]
fn rejects_a_non_ascii_digit() {
    assert!(matches!(hex::decode::<2>("00é"), Err(HexError::NotHex)));
}

#[test]
fn rejects_a_signed_digit_pair() {
    assert!(matches!(hex::decode::<1>("+f"), Err(HexError::NotHex)));
}

#[test]
fn rejects_the_wrong_byte_count() {
    assert!(matches!(
        hex::decode::<4>("00ff"),
        Err(HexError::WrongLength {
            found: 2,
            expected: 4
        })
    ));
}
