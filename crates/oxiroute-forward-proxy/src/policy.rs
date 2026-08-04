use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    #[error("destination is outside the configured time policy")]
    ForbiddenTime,
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

/// Canonical domain and CIDR rules for an explicit forward proxy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationRules {
    allow_domains: Vec<DomainRule>,
    deny_domains: Vec<DomainRule>,
    allow_networks: Vec<IpNetwork>,
    deny_networks: Vec<IpNetwork>,
    allow_times: Vec<TimeWindow>,
    deny_times: Vec<TimeWindow>,
    deny_private: bool,
}

impl DestinationRules {
    /// Parses normalized domain and CIDR rule lists.
    ///
    /// Empty allow lists permit destinations not rejected by deny rules or `deny_private`.
    /// Nonempty domain and CIDR allow lists are independent constraints; when both exist, both the
    /// requested DNS name and every resolved address must match their respective allow list.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed domains, CIDRs, duplicate rules, and host-bit CIDRs.
    pub fn new(
        allow_domains: impl IntoIterator<Item = String>,
        deny_domains: impl IntoIterator<Item = String>,
        allow_cidrs: impl IntoIterator<Item = String>,
        deny_cidrs: impl IntoIterator<Item = String>,
        deny_private: bool,
    ) -> Result<Self, RuleError> {
        Ok(Self {
            allow_domains: parse_unique(allow_domains, DomainRule::parse)?,
            deny_domains: parse_unique(deny_domains, DomainRule::parse)?,
            allow_networks: parse_unique(allow_cidrs, IpNetwork::parse)?,
            deny_networks: parse_unique(deny_cidrs, IpNetwork::parse)?,
            allow_times: Vec::new(),
            deny_times: Vec::new(),
            deny_private,
        })
    }

    /// Adds bounded UTC time windows to the destination policy.
    ///
    /// Deny windows take precedence. When one or more allow windows are configured, the current
    /// UTC time must be inside at least one of them.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::InvalidTimeWindow`] for malformed windows or [`RuleError::Duplicate`]
    /// for repeated windows.
    pub fn with_time_windows(
        mut self,
        allow: impl IntoIterator<Item = TimeWindow>,
        deny: impl IntoIterator<Item = TimeWindow>,
    ) -> Result<Self, RuleError> {
        self.allow_times = parse_time_windows(allow)?;
        self.deny_times = parse_time_windows(deny)?;
        Ok(self)
    }

    /// Returns an approved destination after evaluating the policy at the current UTC time.
    ///
    /// The returned socket addresses are exactly the addresses supplied to this call. A runtime
    /// must not resolve the hostname again after receiving this value.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the destination is not allowed.
    pub fn approve(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
    ) -> Result<ApprovedDestination, PolicyError> {
        self.approve_at(context, destination, addresses, SystemTime::now())
    }

    /// Returns an approved destination after evaluating the policy at an explicit UTC instant.
    ///
    /// This deterministic form is useful for policy tests and callers that already own a trusted
    /// clock sample.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the destination is not allowed.
    pub fn approve_at(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
        now: SystemTime,
    ) -> Result<ApprovedDestination, PolicyError> {
        self.authorize_at(context, destination, addresses, now)?;
        Ok(ApprovedDestination::new(
            destination.clone(),
            addresses.to_vec(),
        ))
    }

    /// Authorizes a destination at an explicit UTC instant.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the destination is not allowed.
    pub fn authorize_at(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
        now: SystemTime,
    ) -> Result<(), PolicyError> {
        self.authorize_inner(context, destination, addresses, now)
    }
}

impl DestinationPolicy for DestinationRules {
    fn authorize(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
    ) -> Result<(), PolicyError> {
        self.authorize_at(context, destination, addresses, SystemTime::now())
    }
}

impl DestinationRules {
    fn authorize_inner(
        &self,
        context: &PolicyContext,
        destination: &Destination,
        addresses: &[IpAddr],
        now: SystemTime,
    ) -> Result<(), PolicyError> {
        if self.deny_private {
            ForbiddenDestinationPolicy.authorize(context, destination, addresses)?;
        }
        if matches!(&destination.host, Host::Dns(name) if self.deny_domains.iter().any(|rule| rule.matches(name)))
        {
            return Err(PolicyError::Rejected);
        }
        if !self.allow_domains.is_empty()
            && !matches!(&destination.host, Host::Dns(name) if self.allow_domains.iter().any(|rule| rule.matches(name)))
        {
            return Err(PolicyError::Rejected);
        }
        if self.deny_times.iter().any(|window| window.matches_at(now))
            || (!self.allow_times.is_empty()
                && !self.allow_times.iter().any(|window| window.matches_at(now)))
        {
            return Err(PolicyError::ForbiddenTime);
        }
        if addresses.iter().any(|address| {
            self.deny_networks
                .iter()
                .any(|network| network.contains(*address))
        }) {
            return Err(PolicyError::ForbiddenAddress);
        }
        if !self.allow_networks.is_empty()
            && addresses.iter().any(|address| {
                !self
                    .allow_networks
                    .iter()
                    .any(|network| network.contains(*address))
            })
        {
            return Err(PolicyError::Rejected);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuleError {
    #[error("destination rule is malformed")]
    Malformed,
    #[error("destination rule is duplicated")]
    Duplicate,
    #[error("destination time window is malformed")]
    InvalidTimeWindow,
}

/// A bounded half-open UTC time window for destination policy evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TimeWindow {
    days: u8,
    start_minute: u16,
    end_minute: u16,
}

impl TimeWindow {
    /// Creates a window using Monday bit 0 through Sunday bit 6 and minutes since midnight.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::InvalidTimeWindow`] when no day is selected, a bit outside the week
    /// is set, or the end is not after the start in the inclusive 24-hour bound.
    pub fn new(days: u8, start_minute: u16, end_minute: u16) -> Result<Self, RuleError> {
        if days == 0 || days & !0x7f != 0 || start_minute >= end_minute || end_minute > 24 * 60 {
            return Err(RuleError::InvalidTimeWindow);
        }
        Ok(Self {
            days,
            start_minute,
            end_minute,
        })
    }

    #[must_use]
    pub const fn days(self) -> u8 {
        self.days
    }

    #[must_use]
    pub const fn start_minute(self) -> u16 {
        self.start_minute
    }

    #[must_use]
    pub const fn end_minute(self) -> u16 {
        self.end_minute
    }

    #[must_use]
    pub fn matches_at(self, now: SystemTime) -> bool {
        let Ok(duration) = now.duration_since(UNIX_EPOCH) else {
            return false;
        };
        let days_since_epoch = duration.as_secs() / Duration::from_secs(86_400).as_secs();
        let weekday = u8::try_from((days_since_epoch + 3) % 7).unwrap_or(0);
        let minute = u16::try_from((duration.as_secs() % 86_400) / 60).unwrap_or(0);
        self.days & (1 << weekday) != 0 && (self.start_minute..self.end_minute).contains(&minute)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DomainRule {
    Exact(String),
    OneLabelWildcard(String),
}

impl DomainRule {
    fn parse(value: &str) -> Result<Self, RuleError> {
        let value = value.to_ascii_lowercase();
        if let Some(suffix) = value.strip_prefix("*.") {
            valid_dns_name(suffix)
                .then(|| Self::OneLabelWildcard(suffix.into()))
                .ok_or(RuleError::Malformed)
        } else {
            valid_dns_name(&value)
                .then_some(Self::Exact(value))
                .ok_or(RuleError::Malformed)
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(expected) => name == expected,
            Self::OneLabelWildcard(suffix) => name
                .strip_suffix(suffix)
                .and_then(|prefix| prefix.strip_suffix('.'))
                .is_some_and(|label| !label.is_empty() && !label.contains('.')),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IpNetwork {
    V4 { network: u32, prefix: u8 },
    V6 { network: u128, prefix: u8 },
}

impl IpNetwork {
    fn parse(value: &str) -> Result<Self, RuleError> {
        let (address, prefix) = value.split_once('/').ok_or(RuleError::Malformed)?;
        let address = address
            .parse::<IpAddr>()
            .map_err(|_| RuleError::Malformed)?;
        let prefix = prefix.parse::<u8>().map_err(|_| RuleError::Malformed)?;
        match address {
            IpAddr::V4(address) if prefix <= 32 => {
                let network = u32::from(address);
                (network & v4_mask(prefix) == network)
                    .then_some(Self::V4 { network, prefix })
                    .ok_or(RuleError::Malformed)
            }
            IpAddr::V6(address) if prefix <= 128 => {
                let network = u128::from(address);
                (network & v6_mask(prefix) == network)
                    .then_some(Self::V6 { network, prefix })
                    .ok_or(RuleError::Malformed)
            }
            IpAddr::V4(_) | IpAddr::V6(_) => Err(RuleError::Malformed),
        }
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self, address) {
            (Self::V4 { network, prefix }, IpAddr::V4(address)) => {
                u32::from(address) & v4_mask(prefix) == network
            }
            (Self::V6 { network, prefix }, IpAddr::V6(address)) => {
                u128::from(address) & v6_mask(prefix) == network
            }
            (Self::V4 { .. }, IpAddr::V6(_)) | (Self::V6 { .. }, IpAddr::V4(_)) => false,
        }
    }
}

fn parse_unique<T: Eq>(
    values: impl IntoIterator<Item = String>,
    parse: impl Fn(&str) -> Result<T, RuleError>,
) -> Result<Vec<T>, RuleError> {
    let mut parsed = Vec::new();
    for value in values {
        let value = parse(&value)?;
        if parsed.contains(&value) {
            return Err(RuleError::Duplicate);
        }
        parsed.push(value);
    }
    Ok(parsed)
}

fn parse_time_windows(
    values: impl IntoIterator<Item = TimeWindow>,
) -> Result<Vec<TimeWindow>, RuleError> {
    let mut parsed = Vec::new();
    for value in values {
        if value.days == 0
            || value.days & !0x7f != 0
            || value.start_minute >= value.end_minute
            || value.end_minute > 24 * 60
        {
            return Err(RuleError::InvalidTimeWindow);
        }
        if parsed.contains(&value) {
            return Err(RuleError::Duplicate);
        }
        parsed.push(value);
    }
    Ok(parsed)
}

fn valid_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.ends_with('.')
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

const fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

const fn v6_mask(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
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

    #[test]
    fn empty_allow_lists_permit_anonymous_public_destinations() {
        let policy = DestinationRules::new([], [], [], [], true).expect("public policy");
        let destination = Destination {
            host: Host::Dns("example.com".into()),
            port: 80,
        };

        assert!(
            policy
                .authorize(
                    &context(),
                    &destination,
                    &["93.184.216.34".parse().unwrap()]
                )
                .is_ok()
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &destination,
                &[
                    "93.184.216.34".parse().unwrap(),
                    "10.0.0.1".parse().unwrap()
                ]
            ),
            Err(PolicyError::ForbiddenAddress)
        );
    }

    #[test]
    fn deny_rules_override_and_allow_rules_constrain_the_complete_answer() {
        let policy = DestinationRules::new(
            ["*.example.com".into()],
            ["blocked.example.com".into()],
            ["93.184.216.0/24".into()],
            ["93.184.216.128/25".into()],
            false,
        )
        .expect("bounded policy");
        let destination = |name: &str| Destination {
            host: Host::Dns(name.into()),
            port: 443,
        };

        assert!(
            policy
                .authorize(
                    &context(),
                    &destination("www.example.com"),
                    &["93.184.216.34".parse().unwrap()]
                )
                .is_ok()
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &destination("blocked.example.com"),
                &["93.184.216.34".parse().unwrap()]
            ),
            Err(PolicyError::Rejected)
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &destination("deep.www.example.com"),
                &["93.184.216.34".parse().unwrap()]
            ),
            Err(PolicyError::Rejected)
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &destination("www.example.com"),
                &[
                    "93.184.216.34".parse().unwrap(),
                    "93.184.217.1".parse().unwrap()
                ]
            ),
            Err(PolicyError::Rejected)
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &destination("www.example.com"),
                &["93.184.216.200".parse().unwrap()]
            ),
            Err(PolicyError::ForbiddenAddress)
        );
    }

    #[test]
    fn domain_allow_rules_reject_ip_literals_and_cidr_rules_are_family_exact() {
        let policy = DestinationRules::new(
            ["example.com".into()],
            [],
            ["2001:db9::/32".into()],
            [],
            false,
        )
        .expect("dual constraint policy");
        assert_eq!(
            policy.authorize(
                &context(),
                &Destination {
                    host: Host::Ip("2001:db9::1".parse().unwrap()),
                    port: 443,
                },
                &["2001:db9::1".parse().unwrap()]
            ),
            Err(PolicyError::Rejected)
        );
        assert_eq!(
            policy.authorize(
                &context(),
                &Destination {
                    host: Host::Dns("example.com".into()),
                    port: 443,
                },
                &["93.184.216.34".parse().unwrap()]
            ),
            Err(PolicyError::Rejected)
        );
    }

    #[test]
    fn malformed_duplicate_and_host_bit_rules_are_rejected() {
        assert_eq!(
            DestinationRules::new(["bad..name".into()], [], [], [], true),
            Err(RuleError::Malformed)
        );
        assert_eq!(
            DestinationRules::new(
                ["EXAMPLE.COM".into(), "example.com".into()],
                [],
                [],
                [],
                true
            ),
            Err(RuleError::Duplicate)
        );
        assert_eq!(
            DestinationRules::new([], [], ["192.0.2.1/24".into()], [], true),
            Err(RuleError::Malformed)
        );
        assert_eq!(TimeWindow::new(0, 0, 60), Err(RuleError::InvalidTimeWindow));
        assert_eq!(
            TimeWindow::new(1, 60, 60),
            Err(RuleError::InvalidTimeWindow)
        );
    }

    #[test]
    fn time_windows_are_utc_bounded_and_deny_overrides() {
        let policy = DestinationRules::new([], [], [], [], false)
            .expect("base policy")
            .with_time_windows(
                [TimeWindow::new(1, 9 * 60, 17 * 60).expect("Monday window")],
                [TimeWindow::new(1, 12 * 60, 13 * 60).expect("Monday lunch deny")],
            )
            .expect("time policy");
        let destination = Destination {
            host: Host::Dns("example.com".into()),
            port: 443,
        };
        let monday_morning = UNIX_EPOCH + Duration::from_secs(4 * 86_400 + 10 * 3_600);
        let monday_lunch = UNIX_EPOCH + Duration::from_secs(4 * 86_400 + 12 * 3_600);
        let tuesday_morning = UNIX_EPOCH + Duration::from_secs(5 * 86_400 + 10 * 3_600);

        assert!(
            policy
                .authorize_at(
                    &context(),
                    &destination,
                    &["93.184.216.34".parse().unwrap()],
                    monday_morning,
                )
                .is_ok()
        );
        assert_eq!(
            policy.authorize_at(
                &context(),
                &destination,
                &["93.184.216.34".parse().unwrap()],
                monday_lunch,
            ),
            Err(PolicyError::ForbiddenTime)
        );
        assert_eq!(
            policy.authorize_at(
                &context(),
                &destination,
                &["93.184.216.34".parse().unwrap()],
                tuesday_morning,
            ),
            Err(PolicyError::ForbiddenTime)
        );
    }

    #[test]
    fn approved_destination_retains_only_the_checked_addresses() {
        let policy = DestinationRules::new([], [], [], [], false).expect("base policy");
        let destination = Destination {
            host: Host::Dns("example.com".into()),
            port: 443,
        };
        let addresses = vec!["93.184.216.34".parse().unwrap()];
        let approved = policy
            .approve(&context(), &destination, &addresses)
            .expect("approved destination");

        assert_eq!(approved.destination, destination);
        assert_eq!(
            approved.socket_addresses.as_ref(),
            &["93.184.216.34:443".parse().unwrap()]
        );
    }
}
