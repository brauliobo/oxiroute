use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

use openssl::ssl::{SslConnector, SslMethod, SslStream};

const DEFAULT_FLASH_VERSION: &str = "WIN 23,0,0,207";
const MAX_DESTINATION_ADDRESSES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RtmpTransport {
    #[default]
    Rtmp,
    Rtmps,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RtmpRtmpsMode {
    #[default]
    Disabled,
    Allowed,
    Required,
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCredential {
    username: Arc<str>,
    secret: Arc<[u8]>,
}

impl fmt::Debug for RtmpCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpCredential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl RtmpCredential {
    #[must_use]
    pub fn new(username: impl Into<Arc<str>>, secret: impl AsRef<[u8]>) -> Self {
        Self {
            username: username.into(),
            secret: Arc::from(secret.as_ref()),
        }
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    #[must_use]
    pub(crate) fn secret(&self) -> &[u8] {
        &self.secret
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpClientOptions {
    pub flash_version: String,
    pub playback_buffer_ms: u32,
    pub tc_url: Option<String>,
    pub credential: Option<RtmpCredential>,
}

impl Default for RtmpClientOptions {
    fn default() -> Self {
        Self {
            flash_version: DEFAULT_FLASH_VERSION.into(),
            playback_buffer_ms: 2_000,
            tc_url: None,
            credential: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpOutboundPolicy {
    pub allow_domains: Vec<String>,
    pub deny_domains: Vec<String>,
    pub allow_cidrs: Vec<String>,
    pub deny_cidrs: Vec<String>,
    pub deny_private: bool,
    pub rtmps: RtmpRtmpsMode,
    pub max_chain_depth: u8,
}

impl Default for RtmpOutboundPolicy {
    fn default() -> Self {
        Self {
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            allow_cidrs: Vec::new(),
            deny_cidrs: Vec::new(),
            deny_private: true,
            rtmps: RtmpRtmpsMode::Disabled,
            max_chain_depth: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationPolicyError {
    EmptyHost,
    TooManyAddresses,
    DomainDenied,
    AddressDenied,
    AddressNotAllowed,
    InvalidPolicyCidr,
}

impl fmt::Display for DestinationPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyHost => "destination host is empty",
            Self::TooManyAddresses => "destination resolved to too many addresses",
            Self::DomainDenied => "destination domain is not allowed",
            Self::AddressDenied => "destination resolved to a denied address",
            Self::AddressNotAllowed => "destination resolved outside the allowed CIDRs",
            Self::InvalidPolicyCidr => "destination policy contains an invalid CIDR",
        })
    }
}

impl std::error::Error for DestinationPolicyError {}

impl RtmpOutboundPolicy {
    /// Validates every address returned for a destination before one address is pinned.
    ///
    /// Checking the complete answer, rather than only the selected address, prevents a later
    /// reconnect from switching to an address that was present in the original DNS answer but was
    /// never admitted by policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the address set, domain, CIDR policy, or private-address policy is
    /// invalid.
    pub fn validate_resolved(
        &self,
        host: &str,
        addresses: &[SocketAddr],
    ) -> Result<(), DestinationPolicyError> {
        if host.is_empty() {
            return Err(DestinationPolicyError::EmptyHost);
        }
        if addresses.is_empty() || addresses.len() > MAX_DESTINATION_ADDRESSES {
            return Err(DestinationPolicyError::TooManyAddresses);
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if !self.allow_domains.is_empty()
            && !self
                .allow_domains
                .iter()
                .any(|domain| domain_matches(&host, domain))
        {
            return Err(DestinationPolicyError::DomainDenied);
        }
        if self
            .deny_domains
            .iter()
            .any(|domain| domain_matches(&host, domain))
        {
            return Err(DestinationPolicyError::DomainDenied);
        }

        let allow = parse_cidrs(&self.allow_cidrs)?;
        let deny = parse_cidrs(&self.deny_cidrs)?;
        for address in addresses {
            let ip = address.ip();
            if self.deny_private && is_private_or_local(ip) {
                return Err(DestinationPolicyError::AddressDenied);
            }
            if deny.iter().any(|network| network.matches(ip)) {
                return Err(DestinationPolicyError::AddressDenied);
            }
            if !allow.is_empty() && !allow.iter().any(|network| network.matches(ip)) {
                return Err(DestinationPolicyError::AddressNotAllowed);
            }
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the selected RTMP transport conflicts with the configured RTMPS
    /// requirement.
    pub fn validate_transport(
        &self,
        transport: RtmpTransport,
    ) -> Result<(), DestinationPolicyError> {
        match (self.rtmps, transport) {
            (RtmpRtmpsMode::Disabled, RtmpTransport::Rtmps)
            | (RtmpRtmpsMode::Required, RtmpTransport::Rtmp) => {
                Err(DestinationPolicyError::DomainDenied)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy)]
struct Cidr {
    address: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn matches(self, candidate: IpAddr) -> bool {
        match (self.address, candidate) {
            (IpAddr::V4(address), IpAddr::V4(candidate)) => {
                mask(u32::from(address), 32, self.prefix)
                    == mask(u32::from(candidate), 32, self.prefix)
            }
            (IpAddr::V6(address), IpAddr::V6(candidate)) => {
                mask(u128::from(address), 128, self.prefix)
                    == mask(u128::from(candidate), 128, self.prefix)
            }
            _ => false,
        }
    }
}

fn parse_cidrs(values: &[String]) -> Result<Vec<Cidr>, DestinationPolicyError> {
    values
        .iter()
        .map(|value| {
            let (address, prefix) = value
                .split_once('/')
                .ok_or(DestinationPolicyError::InvalidPolicyCidr)?;
            let address = address
                .parse::<IpAddr>()
                .map_err(|_| DestinationPolicyError::InvalidPolicyCidr)?;
            let prefix = prefix
                .parse::<u8>()
                .map_err(|_| DestinationPolicyError::InvalidPolicyCidr)?;
            (prefix <= if address.is_ipv4() { 32 } else { 128 })
                .then_some(Cidr { address, prefix })
                .ok_or(DestinationPolicyError::InvalidPolicyCidr)
        })
        .collect()
}

fn mask(value: impl Into<u128>, bits: u8, prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        value.into() & (u128::MAX << u32::from(bits - prefix))
    }
}

fn domain_matches(host: &str, configured: &str) -> bool {
    let configured = configured.trim_end_matches('.').to_ascii_lowercase();
    host == configured || host.ends_with(&format!(".{configured}"))
}

fn is_private_or_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address == Ipv4Addr::BROADCAST
                || address.octets()[0] == 100 && (64..=127).contains(&address.octets()[1])
                || address.octets()[0] == 192
                    && address.octets()[1] == 0
                    && address.octets()[2] == 0
                || address.octets()[0] == 198 && (18..=19).contains(&address.octets()[1])
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.segments()[0] & 0xfe00 == 0xfc00
                || address.segments()[0] & 0xffc0 == 0xfe80
        }
    }
}

pub(crate) enum RtmpStream {
    Plain(TcpStream),
    Tls(SslStream<TcpStream>),
}

impl RtmpStream {
    pub(crate) fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.get_mut().set_read_timeout(timeout),
        }
    }

    pub(crate) fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_write_timeout(timeout),
            Self::Tls(stream) => stream.get_mut().set_write_timeout(timeout),
        }
    }

    pub(crate) fn set_nodelay(&mut self, nodelay: bool) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_nodelay(nodelay),
            Self::Tls(stream) => stream.get_mut().set_nodelay(nodelay),
        }
    }
}

delegate_read_write!(RtmpStream { Plain, Tls });

pub(crate) fn connect_stream(
    host: &str,
    address: SocketAddr,
    transport: RtmpTransport,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> io::Result<RtmpStream> {
    let stream = TcpStream::connect_timeout(&address, connect_timeout)?;
    stream.set_read_timeout(Some(io_timeout))?;
    stream.set_write_timeout(Some(io_timeout))?;
    stream.set_nodelay(true)?;
    match transport {
        RtmpTransport::Rtmp => Ok(RtmpStream::Plain(stream)),
        RtmpTransport::Rtmps => {
            let connector = SslConnector::builder(SslMethod::tls_client())
                .map_err(io::Error::other)?
                .build();
            let stream = connector.connect(host, stream).map_err(io::Error::other)?;
            Ok(RtmpStream::Tls(stream))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_answers_even_when_a_public_address_is_first() {
        let policy = RtmpOutboundPolicy::default();
        assert_eq!(
            policy.validate_resolved(
                "origin.example",
                &[
                    "198.51.100.10:1935".parse().expect("public address"),
                    "127.0.0.1:1935".parse().expect("private address"),
                ],
            ),
            Err(DestinationPolicyError::AddressDenied)
        );
    }

    #[test]
    fn domain_and_transport_policy_are_checked_before_connect() {
        let policy = RtmpOutboundPolicy {
            allow_domains: vec!["media.example".into()],
            rtmps: RtmpRtmpsMode::Required,
            ..RtmpOutboundPolicy::default()
        };
        let address = "198.51.100.10:443".parse().expect("public address");
        assert_eq!(
            policy.validate_resolved("other.example", &[address]),
            Err(DestinationPolicyError::DomainDenied)
        );
        assert_eq!(
            policy.validate_transport(RtmpTransport::Rtmp),
            Err(DestinationPolicyError::DomainDenied)
        );
        assert_eq!(policy.validate_transport(RtmpTransport::Rtmps), Ok(()));
    }

    #[test]
    fn credentials_debug_output_is_redacted() {
        let options = RtmpClientOptions {
            credential: Some(RtmpCredential::new("publisher", "private-secret")),
            ..RtmpClientOptions::default()
        };
        let debug = format!("{options:?}");
        assert!(!debug.contains("private-secret"));
        assert!(debug.contains("<redacted>"));
    }
}
