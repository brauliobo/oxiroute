//! SETTINGS payload parsing (RFC 9113 section 6.5).
use crate::error::Reason;
use crate::frame::{DEFAULT_MAX_FRAME_SIZE, MAX_MAX_FRAME_SIZE, MAX_WINDOW};

/// Values carried by one SETTINGS frame; unknown identifiers are ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub header_table_size: Option<u32>,
    pub enable_push: Option<u32>,
    pub max_concurrent_streams: Option<u32>,
    pub initial_window_size: Option<u32>,
    pub max_frame_size: Option<u32>,
    pub max_header_list_size: Option<u32>,
}

/// Why a SETTINGS payload is invalid; always a connection error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsError(pub Reason);

impl Settings {
    /// Parses and validates a non-ACK payload.
    ///
    /// # Errors
    ///
    /// `FRAME_SIZE_ERROR` when the length is not a multiple of six, `PROTOCOL_ERROR` for an out-of-range
    /// `ENABLE_PUSH` or `MAX_FRAME_SIZE`, `FLOW_CONTROL_ERROR` for an `INITIAL_WINDOW_SIZE` above 2^31-1.
    pub fn parse(payload: &[u8]) -> Result<Self, SettingsError> {
        if payload.len() % 6 != 0 {
            return Err(SettingsError(Reason::FRAME_SIZE_ERROR));
        }
        let mut s = Self::default();
        for e in payload.chunks_exact(6) {
            let value = u32::from_be_bytes([e[2], e[3], e[4], e[5]]);
            match u16::from_be_bytes([e[0], e[1]]) {
                1 => s.header_table_size = Some(value),
                2 if value > 1 => return Err(SettingsError(Reason::PROTOCOL_ERROR)),
                2 => s.enable_push = Some(value),
                3 => s.max_concurrent_streams = Some(value),
                4 if value > MAX_WINDOW => return Err(SettingsError(Reason::FLOW_CONTROL_ERROR)),
                4 => s.initial_window_size = Some(value),
                5 if !(DEFAULT_MAX_FRAME_SIZE..=MAX_MAX_FRAME_SIZE).contains(&value) => {
                    return Err(SettingsError(Reason::PROTOCOL_ERROR));
                }
                5 => s.max_frame_size = Some(value),
                6 => s.max_header_list_size = Some(value),
                _ => {}
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u16, v: u32) -> Vec<u8> {
        [id.to_be_bytes().as_slice(), &v.to_be_bytes()].concat()
    }

    #[test]
    fn parses_known_and_skips_unknown() {
        let p = [entry(4, 1000), entry(0x99, 7), entry(5, 20_000)].concat();
        let s = Settings::parse(&p).unwrap();
        assert_eq!(
            (s.initial_window_size, s.max_frame_size),
            (Some(1000), Some(20_000))
        );
    }

    #[test]
    fn rejects_invalid_values() {
        assert_eq!(
            Settings::parse(&entry(2, 2)),
            Err(SettingsError(Reason::PROTOCOL_ERROR))
        );
        assert_eq!(
            Settings::parse(&entry(4, 1 << 31)),
            Err(SettingsError(Reason::FLOW_CONTROL_ERROR))
        );
        assert_eq!(
            Settings::parse(&entry(5, 100)),
            Err(SettingsError(Reason::PROTOCOL_ERROR))
        );
        assert_eq!(
            Settings::parse(&[0; 5]),
            Err(SettingsError(Reason::FRAME_SIZE_ERROR))
        );
    }
}
