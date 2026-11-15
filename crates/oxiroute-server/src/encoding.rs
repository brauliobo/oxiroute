pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

#[inline]
pub(crate) fn decode_base64(value: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine as _;

    // The dependency vectorizes at least 64 nonterminal bytes; its final four bytes are decoded separately.
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    ))]
    if value.len() >= 68 {
        return decode_base64_simd(value);
    }
    base64::engine::general_purpose::STANDARD.decode(value)
}

#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_feature = "neon")
))]
#[inline(never)]
fn decode_base64_simd(value: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::{
        Engine as _,
        engine::{Simd, general_purpose::PAD},
    };
    static ENGINE: std::sync::LazyLock<Simd> = std::sync::LazyLock::new(|| Simd::standard(PAD));
    ENGINE.decode(value)
}

#[cfg(test)]
mod tests {
    use super::lower_hex;

    #[test]
    fn lower_hex_is_exact_and_fixed_width() {
        assert_eq!(lower_hex(&[]), "");
        assert_eq!(lower_hex(&[0x00, 0x0f, 0x10, 0xff]), "000f10ff");
    }

    #[test]
    fn base64_decoding_preserves_padding_errors_and_octets() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        for length in 0..=160 {
            let input: Vec<u8> = (0..=255).cycle().take(length).collect();
            let encoded = STANDARD.encode(&input);
            assert_eq!(super::decode_base64(encoded.as_bytes()), Ok(input));
            for suffix in ["", "=", "==", "===", " ", "\n", "-"] {
                let value = format!("{encoded}{suffix}");
                assert_eq!(
                    super::decode_base64(value.as_bytes()),
                    STANDARD.decode(&value)
                );
            }
        }
        let mut encoded = STANDARD.encode([b'x'; 128]).into_bytes();
        for index in 0..encoded.len() {
            let original = encoded[index];
            for byte in 0..=255 {
                encoded[index] = byte;
                assert_eq!(super::decode_base64(&encoded), STANDARD.decode(&encoded));
            }
            encoded[index] = original;
        }
    }

    #[test]
    fn base64_decoding_preserves_errors_at_the_vector_crossover() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        for length in 63..=69 {
            let mut encoded = STANDARD.encode([b'x'; 64]).into_bytes();
            encoded.truncate(length);
            for index in 0..length {
                let original = encoded[index];
                for byte in 0..=255 {
                    encoded[index] = byte;
                    assert_eq!(super::decode_base64(&encoded), STANDARD.decode(&encoded));
                }
                encoded[index] = original;
            }
        }
    }
}
