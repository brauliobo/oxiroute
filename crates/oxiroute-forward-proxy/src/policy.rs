use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use async_trait::async_trait;

use crate::{Destination, Host, Principal, Protocol};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResolveError {
    #[error("destination name did not resolve")]
    NoAddresses,
    #[error("destination name resolution failed")]
    Failed,
}

#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, ResolveError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyContext {
    pub protocol: Protocol,
    pub principal: Principal,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PolicyError {
    #[error("destination name is locally scoped")]
    LocalName,
    #[error("destination resolves to a forbidden address")]
    ForbiddenAddress,
    #[error("destination port is forbidden")]
    ForbiddenPort,
    #[error("destination was rejected by policy")]
    Rejected,
}

pub trait DestinationPolicy: Send + Sync {
    /// Authorizes every address produced by the single resolution used for connection.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the destination must not be connected.
    fn authorize(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
    ) -> Result<(), PolicyError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovedDestination {
    pub destination: Destination,
    pub socket_addresses: Arc<[SocketAddr]>,
}

impl ApprovedDestination {
    pub(crate) fn new(destination: Destination, addresses: Vec<IpAddr>) -> Self {
        let socket_addresses = addresses
            .into_iter()
            .map(|address| SocketAddr::new(address, destination.port))
            .collect::<Vec<_>>()
            .into();
        Self {
            destination,
            socket_addresses,
        }
    }
}

/// Fail-closed policy for explicit proxies exposed to untrusted clients.
///
/// It rejects localhost names and every address in common non-public, documentation, multicast,
/// link-local, private, benchmark, and reserved ranges. A DNS answer is rejected if any returned
/// address is forbidden, preventing clients from selecting a safe answer during policy evaluation
/// and a private answer during connection.
#[derive(Clone, Debug, Default)]
pub struct ForbiddenDestinationPolicy;

impl DestinationPolicy for ForbiddenDestinationPolicy {
    fn authorize(
        &self,
        _context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
    ) -> Result<(), PolicyError> {
        if matches!(&destination.host, Host::Dns(name) if name == "localhost" || name.ends_with(".localhost"))
        {
            return Err(PolicyError::LocalName);
        }
        if addresses.iter().copied().any(is_forbidden_address) {
            return Err(PolicyError::ForbiddenAddress);
        }
        Ok(())
    }
}

fn is_forbidden_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_forbidden_v4(address),
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_forbidden_v4(mapped);
            }
            is_forbidden_v6(address)
        }
    }
}

fn is_forbidden_v4(address: Ipv4Addr) -> bool {
    let value = u32::from(address);
    in_v4(value, 0x0000_0000, 8)
        || in_v4(value, 0x0a00_0000, 8)
        || in_v4(value, 0x6440_0000, 10)
        || in_v4(value, 0x7f00_0000, 8)
        || in_v4(value, 0xa9fe_0000, 16)
        || in_v4(value, 0xac10_0000, 12)
        || in_v4(value, 0xc000_0000, 24)
        || in_v4(value, 0xc000_0200, 24)
        || in_v4(value, 0xc0a8_0000, 16)
        || in_v4(value, 0xc612_0000, 15)
        || in_v4(value, 0xc633_6400, 24)
        || in_v4(value, 0xcb00_7100, 24)
        || in_v4(value, 0xe000_0000, 4)
        || in_v4(value, 0xf000_0000, 4)
}

fn in_v4(value: u32, network: u32, prefix: u8) -> bool {
    let mask = u32::MAX << (32 - prefix);
    value & mask == network & mask
}

fn is_forbidden_v6(address: Ipv6Addr) -> bool {
    let value = u128::from(address);
    address.is_unspecified()
        || address.is_loopback()
        || in_v6(value, 0, 96)
        || in_v6(value, 0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96)
        || in_v6(value, 0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48)
        || in_v6(value, 0x0100_0000_0000_0000_0000_0000_0000_0000, 64)
        || in_v6(value, 0x2001_0000_0000_0000_0000_0000_0000_0000, 23)
        || in_v6(value, 0x2001_0db8_0000_0000_0000_0000_0000_0000, 32)
        || in_v6(value, 0x2002_0000_0000_0000_0000_0000_0000_0000, 16)
        || in_v6(value, 0x3fff_0000_0000_0000_0000_0000_0000_0000, 20)
        || in_v6(value, 0xfc00_0000_0000_0000_0000_0000_0000_0000, 7)
        || in_v6(value, 0xfe80_0000_0000_0000_0000_0000_0000_0000, 10)
        || in_v6(value, 0xfec0_0000_0000_0000_0000_0000_0000_0000, 10)
        || in_v6(value, 0xff00_0000_0000_0000_0000_0000_0000_0000, 8)
}

fn in_v6(value: u128, network: u128, prefix: u8) -> bool {
    let mask = u128::MAX << (128 - prefix);
    value & mask == network & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PolicyContext {
        PolicyContext {
            protocol: Protocol::Http1,
            principal: Principal::new("test"),
        }
    }

    #[test]
    fn denies_private_or_mixed_dns_answers() {
        let destination = Destination {
            host: Host::Dns("example.com".into()),
            port: 443,
        };
        let policy = ForbiddenDestinationPolicy;
        assert_eq!(
            policy.authorize(
                &context(),
                &destination,
                &[
                    "93.184.216.34".parse().unwrap(),
                    "127.0.0.1".parse().unwrap()
                ]
            ),
            Err(PolicyError::ForbiddenAddress)
        );
    }

    #[test]
    fn permits_public_addresses() {
        let destination = Destination {
            host: Host::Ip("2606:4700:4700::1111".parse().unwrap()),
            port: 443,
        };
        assert!(
            ForbiddenDestinationPolicy
                .authorize(
                    &context(),
                    &destination,
                    &["2606:4700:4700::1111".parse().unwrap()]
                )
                .is_ok()
        );
    }
}
