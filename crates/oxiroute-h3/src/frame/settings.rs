//! SETTINGS (RFC 9114 section 7.2.4): the frame this server sends and the validation of the peer's.
use crate::code::Code;
use crate::varint;

const QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
const MAX_FIELD_SECTION_SIZE: u64 = 0x06;
const QPACK_BLOCKED_STREAMS: u64 = 0x07;
const ENABLE_CONNECT_PROTOCOL: u64 = 0x08;
const H3_DATAGRAM: u64 = 0x33;

/// Identifiers HTTP/2 used for other purposes: receiving one is a settings error.
const fn is_reserved_h2(id: u64) -> bool {
    matches!(id, 0x0 | 0x2 | 0x3 | 0x4 | 0x5)
}

/// What the peer announced; settings this layer does not know are validated and dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerSettings {
    pub qpack_max_table_capacity: u64,
    pub qpack_blocked_streams: u64,
    /// `None` while the peer announced no limit.
    pub max_field_section_size: Option<u64>,
    pub enable_connect_protocol: bool,
}

/// Appends the SETTINGS frame of this server: no QPACK dynamic table, no blocked streams, and the field section
/// limit.
pub fn encode(out: &mut Vec<u8>, max_field_section_size: u64) {
    let mut payload = Vec::with_capacity(16);
    for (id, value) in [
        (QPACK_MAX_TABLE_CAPACITY, 0),
        (MAX_FIELD_SECTION_SIZE, max_field_section_size),
        (QPACK_BLOCKED_STREAMS, 0),
    ] {
        varint::encode(&mut payload, id);
        varint::encode(&mut payload, value);
    }
    super::write_header(out, super::SETTINGS, payload.len() as u64);
    out.extend_from_slice(&payload);
}

/// Parses and validates the payload of a peer SETTINGS frame.
///
/// # Errors
///
/// `H3_FRAME_ERROR` for a truncated setting, `H3_SETTINGS_ERROR` for a reserved, repeated or invalid one.
pub fn parse(mut payload: &[u8]) -> Result<PeerSettings, Code> {
    let mut settings = PeerSettings::default();
    let mut ids = Vec::new();
    while !payload.is_empty() {
        let (Some(id), Some(value)) = (varint::decode(&mut payload), varint::decode(&mut payload))
        else {
            return Err(Code::H3_FRAME_ERROR);
        };
        if is_reserved_h2(id) {
            return Err(Code::H3_SETTINGS_ERROR);
        }
        ids.push(id);
        match id {
            QPACK_MAX_TABLE_CAPACITY => settings.qpack_max_table_capacity = value,
            QPACK_BLOCKED_STREAMS => settings.qpack_blocked_streams = value,
            MAX_FIELD_SECTION_SIZE => settings.max_field_section_size = Some(value),
            ENABLE_CONNECT_PROTOCOL if value <= 1 => settings.enable_connect_protocol = value == 1,
            H3_DATAGRAM if value <= 1 => {}
            ENABLE_CONNECT_PROTOCOL | H3_DATAGRAM => return Err(Code::H3_SETTINGS_ERROR),
            _ => {}
        }
    }
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Code::H3_SETTINGS_ERROR);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(pairs: &[(u64, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(id, value) in pairs {
            varint::encode(&mut out, id);
            varint::encode(&mut out, value);
        }
        out
    }

    #[test]
    fn encoded_frame_announces_no_dynamic_table_and_the_field_limit() {
        let mut out = Vec::new();
        encode(&mut out, 16_384);
        assert_eq!(&out[..2], [0x04, out.len() as u8 - 2]);
        let settings = parse(&out[2..]).unwrap();
        assert_eq!(settings.qpack_max_table_capacity, 0);
        assert_eq!(settings.qpack_blocked_streams, 0);
        assert_eq!(settings.max_field_section_size, Some(16_384));
    }

    #[test]
    fn empty_and_unknown_settings_are_accepted() {
        assert_eq!(parse(&[]), Ok(PeerSettings::default()));
        let grease = 0x1f * 3 + 0x21;
        assert!(parse(&payload(&[(grease, 7), (0x4242, 1)])).is_ok());
    }

    #[test]
    fn reserved_repeated_and_invalid_settings_are_settings_errors() {
        for id in [0, 2, 3, 4, 5] {
            assert_eq!(parse(&payload(&[(id, 0)])), Err(Code::H3_SETTINGS_ERROR));
        }
        assert_eq!(
            parse(&payload(&[(1, 0), (1, 0)])),
            Err(Code::H3_SETTINGS_ERROR)
        );
        assert_eq!(
            parse(&payload(&[(0x4242, 0), (6, 1), (0x4242, 1)])),
            Err(Code::H3_SETTINGS_ERROR)
        );
        assert_eq!(
            parse(&payload(&[(ENABLE_CONNECT_PROTOCOL, 2)])),
            Err(Code::H3_SETTINGS_ERROR)
        );
        assert_eq!(
            parse(&payload(&[(H3_DATAGRAM, 2)])),
            Err(Code::H3_SETTINGS_ERROR)
        );
    }

    #[test]
    fn truncated_setting_is_a_frame_error() {
        assert_eq!(parse(&[0x01]), Err(Code::H3_FRAME_ERROR));
        assert_eq!(parse(&[0x01, 0x40]), Err(Code::H3_FRAME_ERROR));
    }

    #[test]
    fn peer_values_are_kept() {
        let settings = parse(&payload(&[(1, 4096), (7, 100), (6, 65_536), (8, 1)])).unwrap();
        assert_eq!(
            settings,
            PeerSettings {
                qpack_max_table_capacity: 4096,
                qpack_blocked_streams: 100,
                max_field_section_size: Some(65_536),
                enable_connect_protocol: true,
            }
        );
    }
}
