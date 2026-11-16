use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

const CAPACITY: usize = 128;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;

/// A `sockaddr_storage`-sized byte buffer the kernel fills (accept) or reads (connect) while an operation is in flight.
pub(crate) struct Address {
    bytes: [u8; CAPACITY],
    length: u32,
}

impl Address {
    pub(crate) const fn empty() -> Self {
        Self {
            bytes: [0; CAPACITY],
            length: CAPACITY as u32,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.length = CAPACITY as u32;
    }

    pub(crate) fn out_pointers(&mut self) -> (*mut u8, *mut u32) {
        (self.bytes.as_mut_ptr(), &raw mut self.length)
    }

    pub(crate) fn in_pointers(&self) -> (*const u8, u32) {
        (self.bytes.as_ptr(), self.length)
    }

    pub(crate) fn from_socket(address: SocketAddr) -> Self {
        let mut storage = Self::empty();
        let bytes = &mut storage.bytes;
        bytes[2..4].copy_from_slice(&address.port().to_be_bytes());
        storage.length = match address {
            SocketAddr::V4(v4) => {
                bytes[..2].copy_from_slice(&AF_INET.to_ne_bytes());
                bytes[4..8].copy_from_slice(&v4.ip().octets());
                16
            }
            SocketAddr::V6(v6) => {
                bytes[..2].copy_from_slice(&AF_INET6.to_ne_bytes());
                bytes[4..8].copy_from_slice(&v6.flowinfo().to_ne_bytes());
                bytes[8..24].copy_from_slice(&v6.ip().octets());
                bytes[24..28].copy_from_slice(&v6.scope_id().to_ne_bytes());
                28
            }
        };
        storage
    }

    pub(crate) fn socket(&self) -> Option<SocketAddr> {
        let bytes = &self.bytes;
        let port = u16::from_be_bytes([bytes[2], bytes[3]]);
        match u16::from_ne_bytes([bytes[0], bytes[1]]) {
            AF_INET if self.length >= 16 => {
                let ip = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
                Some(SocketAddrV4::new(ip, port).into())
            }
            AF_INET6 if self.length >= 28 => {
                let octets: [u8; 16] = bytes[8..24].try_into().ok()?;
                let flowinfo = u32::from_ne_bytes(bytes[4..8].try_into().ok()?);
                let scope = u32::from_ne_bytes(bytes[24..28].try_into().ok()?);
                Some(SocketAddrV6::new(Ipv6Addr::from(octets), port, flowinfo, scope).into())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_survives_the_kernel_layout_in_both_families() {
        for text in ["192.0.2.7:8080", "[2001:db8::1]:443"] {
            let address: SocketAddr = text.parse().expect("address");
            assert_eq!(Address::from_socket(address).socket(), Some(address));
        }
    }

    #[test]
    fn a_family_the_crate_does_not_know_has_no_peer() {
        assert_eq!(Address::empty().socket(), None);
    }

    #[test]
    fn the_kernel_receives_the_connect_length_of_each_family() {
        let v4 = Address::from_socket("192.0.2.1:80".parse().expect("address"));
        let v6 = Address::from_socket("[2001:db8::1]:80".parse().expect("address"));
        assert_eq!((v4.in_pointers().1, v6.in_pointers().1), (16, 28));
    }
}
