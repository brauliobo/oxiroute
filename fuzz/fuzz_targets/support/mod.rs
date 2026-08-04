use std::borrow::Cow;

/// Decodes optional `hex:` corpus entries while keeping ordinary fuzzer bytes unchanged.
pub fn bounded_input<'a>(data: &'a [u8], maximum: usize) -> Option<Cow<'a, [u8]>> {
    if data.len() > maximum {
        return None;
    }
    let Some(encoded) = data.strip_prefix(b"hex:") else {
        return Some(Cow::Borrowed(data));
    };
    if encoded.len() % 2 != 0 {
        return Some(Cow::Borrowed(data));
    }

    let mut decoded = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.chunks_exact(2) {
        let Some(high) = hex_digit(pair[0]) else {
            return Some(Cow::Borrowed(data));
        };
        let Some(low) = hex_digit(pair[1]) else {
            return Some(Cow::Borrowed(data));
        };
        decoded.push((high << 4) | low);
    }
    Some(Cow::Owned(decoded))
}

#[allow(dead_code)]
pub fn strip_prefix<'a>(data: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    data.strip_prefix(prefix)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
