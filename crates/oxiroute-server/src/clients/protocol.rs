#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Protocol {
    Http1,
    H2,
    H3,
    Tcp,
    Udp,
    Rtmp,
    WebSocket,
}

impl Protocol {
    pub(crate) const ALL: [Self; 7] = [
        Self::Http1,
        Self::H2,
        Self::H3,
        Self::Tcp,
        Self::Udp,
        Self::Rtmp,
        Self::WebSocket,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http1 => "http/1.1",
            Self::H2 => "h2",
            Self::H3 => "h3",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Rtmp => "rtmp",
            Self::WebSocket => "websocket",
        }
    }

    pub(crate) const fn serves_requests(self) -> bool {
        matches!(self, Self::Http1 | Self::H2 | Self::H3)
    }

    pub(super) const fn bit(self) -> u16 {
        1 << self as u8
    }

    pub(super) fn from_u8(value: u8) -> Self {
        Self::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or(Self::Tcp)
    }

    pub(super) fn names(mask: u16) -> Vec<&'static str> {
        Self::ALL
            .into_iter()
            .filter(|protocol| mask & protocol.bit() != 0)
            .map(Self::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_lists_its_protocols_in_order() {
        let mask = Protocol::H2.bit() | Protocol::Http1.bit();
        assert_eq!(Protocol::names(mask), ["http/1.1", "h2"]);
    }

    #[test]
    fn every_protocol_round_trips_through_its_number() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::from_u8(protocol as u8), protocol);
        }
    }
}
