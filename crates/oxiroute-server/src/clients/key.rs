use std::net::{IpAddr, Ipv6Addr};

const IPV6_HIGH_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct IpKey([u8; 16]);

impl IpKey {
    pub(crate) fn new(ip: IpAddr, ipv6_prefix: u8) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(ip) => Self(ip.to_ipv6_mapped().octets()),
            IpAddr::V6(ip) => Self(Self::masked(ip.octets(), ipv6_prefix)),
        }
    }

    pub(crate) fn addr(self) -> IpAddr {
        Ipv6Addr::from(self.0).to_canonical()
    }

    fn masked(mut octets: [u8; 16], ipv6_prefix: u8) -> [u8; 16] {
        if ipv6_prefix <= 64 {
            octets[IPV6_HIGH_BYTES..].fill(0);
        }
        octets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str, prefix: u8) -> IpKey {
        IpKey::new(text.parse().expect("an address"), prefix)
    }

    #[test]
    fn ipv4_and_its_mapped_form_are_one_client() {
        assert_eq!(key("203.0.113.7", 64), key("::ffff:203.0.113.7", 64));
        assert_eq!(key("203.0.113.7", 64).addr().to_string(), "203.0.113.7");
    }

    #[test]
    fn a_64_prefix_groups_the_hosts_of_one_network() {
        let first = key("2001:db8:1:2::1", 64);
        assert_eq!(first, key("2001:db8:1:2:ffff::9", 64));
        assert_ne!(first, key("2001:db8:1:3::1", 64));
        assert_eq!(first.addr().to_string(), "2001:db8:1:2::");
    }

    #[test]
    fn a_128_prefix_keeps_every_host() {
        assert_ne!(key("2001:db8:1:2::1", 128), key("2001:db8:1:2::2", 128));
    }
}
