#[allow(clippy::wildcard_imports)]
use super::*;

/// Resolves one RTMP destination name without retaining resolver-specific state.
pub trait RtmpDnsResolver: Send + Sync {
    /// Resolves the canonical host and port into a bounded candidate answer.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the resolver cannot produce an answer.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

#[derive(Debug, Default)]
struct SystemRtmpDnsResolver;

impl RtmpDnsResolver for SystemRtmpDnsResolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .take(MAX_RTMP_DESTINATION_ADDRESSES + 1)
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpDestinationResolverError {
    #[error("destination DNS answer is empty")]
    EmptyAnswer,
    #[error("destination DNS answer exceeds the address limit")]
    TooManyAddresses,
    #[error("destination DNS answer contains an invalid address")]
    InvalidAddress,
    #[error("destination address is denied by RTMP outbound policy")]
    Policy,
    #[error("destination resolves to an active RTMP listener")]
    DirectLoop,
    #[error("destination DNS answer has no address in the selected family")]
    FamilyMismatch,
    #[error("destination DNS refresh interval must be nonzero")]
    InvalidRefreshInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpDnsRefreshFailure {
    Resolution,
    AddressSet,
    Policy,
    DirectLoop,
    FamilyMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RtmpDnsRefresh {
    NotDue,
    Refreshed(SocketAddr),
    Failed(RtmpDnsRefreshFailure),
}

struct DestinationResolverState {
    current_address: SocketAddr,
    next_refresh_at: Instant,
    refreshing: bool,
}

/// Shared, bounded DNS state for one compiled RTMP destination.
pub struct RtmpDestinationResolver {
    host: Arc<str>,
    port: u16,
    transport: RtmpTransport,
    address_is_ipv4: bool,
    policy: RtmpOutboundPolicy,
    listener_addresses: Arc<[SocketAddr]>,
    refresh_interval: Duration,
    resolver: Arc<dyn RtmpDnsResolver>,
    state: Arc<Mutex<DestinationResolverState>>,
}

impl fmt::Debug for RtmpDestinationResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpDestinationResolver")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("transport", &self.transport)
            .field("address_is_ipv4", &self.address_is_ipv4)
            .field("refresh_interval", &self.refresh_interval)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RtmpDestinationResolver {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.transport == other.transport
            && self.address_is_ipv4 == other.address_is_ipv4
            && self.policy == other.policy
            && self.listener_addresses == other.listener_addresses
            && self.refresh_interval == other.refresh_interval
            && Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for RtmpDestinationResolver {}

impl RtmpDestinationResolver {
    /// Resolves one bounded startup answer with the system DNS resolver.
    ///
    /// # Errors
    ///
    /// Returns the resolver I/O error when the host cannot be resolved.
    pub fn resolve_startup_addresses(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        RtmpDnsResolver::resolve(&SystemRtmpDnsResolver, host, port)
    }

    /// Compiles a destination from its already resolved startup answer.
    ///
    /// The complete startup answer is checked before the selected address is retained. Refreshes
    /// use the same policy and listener-loop checks, and only an address in this initial family can
    /// replace it.
    ///
    /// # Errors
    ///
    /// Returns an error when the startup answer or refresh interval is invalid.
    pub fn from_startup(
        host: impl Into<Arc<str>>,
        port: u16,
        transport: RtmpTransport,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: RtmpOutboundPolicy,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        refresh_interval: Duration,
    ) -> Result<Self, RtmpDestinationResolverError> {
        Self::from_startup_with_resolver(
            host,
            port,
            transport,
            addresses,
            policy,
            listener_addresses,
            refresh_interval,
            Arc::new(SystemRtmpDnsResolver),
        )
    }

    /// Compiles a destination with an injected resolver, primarily for deterministic loopback tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the startup answer or refresh interval is invalid.
    #[expect(
        clippy::too_many_arguments,
        reason = "startup resolver construction keeps destination policy inputs explicit"
    )]
    pub fn from_startup_with_resolver(
        host: impl Into<Arc<str>>,
        port: u16,
        transport: RtmpTransport,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: RtmpOutboundPolicy,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        refresh_interval: Duration,
        resolver: Arc<dyn RtmpDnsResolver>,
    ) -> Result<Self, RtmpDestinationResolverError> {
        if refresh_interval.is_zero() {
            return Err(RtmpDestinationResolverError::InvalidRefreshInterval);
        }
        let host = host.into();
        let listener_addresses: Arc<[SocketAddr]> = listener_addresses.into_iter().collect();
        let address = validate_rtmp_destination_answer(
            &host,
            port,
            transport,
            &policy,
            &listener_addresses,
            addresses,
            None,
        )?;
        Ok(Self {
            host,
            port,
            transport,
            address_is_ipv4: address.is_ipv4(),
            policy,
            listener_addresses,
            refresh_interval,
            resolver,
            state: Arc::new(Mutex::new(DestinationResolverState {
                current_address: address,
                next_refresh_at: Instant::now() + refresh_interval,
                refreshing: false,
            })),
        })
    }

    /// # Panics
    ///
    /// Panics if the resolver state mutex is poisoned.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.state
            .lock()
            .expect("RTMP destination resolver mutex poisoned")
            .current_address
    }

    pub(super) fn refresh_if_due(&self) -> RtmpDnsRefresh {
        let now = Instant::now();
        {
            let mut state = self
                .state
                .lock()
                .expect("RTMP destination resolver mutex poisoned");
            if state.refreshing || now < state.next_refresh_at {
                return RtmpDnsRefresh::NotDue;
            }
            state.refreshing = true;
        }

        let result = self
            .resolver
            .resolve(&self.host, self.port)
            .map_err(|_| RtmpDnsRefreshFailure::Resolution)
            .and_then(|addresses| {
                validate_rtmp_destination_answer(
                    &self.host,
                    self.port,
                    self.transport,
                    &self.policy,
                    &self.listener_addresses,
                    addresses,
                    Some(self.address_is_ipv4),
                )
                .map_err(|error| match error {
                    RtmpDestinationResolverError::EmptyAnswer
                    | RtmpDestinationResolverError::TooManyAddresses
                    | RtmpDestinationResolverError::InvalidAddress => {
                        RtmpDnsRefreshFailure::AddressSet
                    }
                    RtmpDestinationResolverError::Policy
                    | RtmpDestinationResolverError::InvalidRefreshInterval => {
                        RtmpDnsRefreshFailure::Policy
                    }
                    RtmpDestinationResolverError::DirectLoop => RtmpDnsRefreshFailure::DirectLoop,
                    RtmpDestinationResolverError::FamilyMismatch => {
                        RtmpDnsRefreshFailure::FamilyMismatch
                    }
                })
            });

        let mut state = self
            .state
            .lock()
            .expect("RTMP destination resolver mutex poisoned");
        state.refreshing = false;
        state.next_refresh_at = now
            .checked_add(self.refresh_interval)
            .unwrap_or_else(Instant::now);
        match result {
            Ok(address) => {
                state.current_address = address;
                RtmpDnsRefresh::Refreshed(address)
            }
            Err(error) => RtmpDnsRefresh::Failed(error),
        }
    }
}

fn validate_rtmp_destination_answer(
    host: &str,
    port: u16,
    transport: RtmpTransport,
    policy: &RtmpOutboundPolicy,
    listener_addresses: &[SocketAddr],
    addresses: impl IntoIterator<Item = SocketAddr>,
    family: Option<bool>,
) -> Result<SocketAddr, RtmpDestinationResolverError> {
    let mut addresses: Vec<_> = addresses
        .into_iter()
        .take(MAX_RTMP_DESTINATION_ADDRESSES + 1)
        .collect();
    if addresses.is_empty() {
        return Err(RtmpDestinationResolverError::EmptyAnswer);
    }
    if addresses.len() > MAX_RTMP_DESTINATION_ADDRESSES {
        return Err(RtmpDestinationResolverError::TooManyAddresses);
    }
    if addresses.iter().any(|address| address.port() != port) {
        return Err(RtmpDestinationResolverError::InvalidAddress);
    }
    addresses.sort_unstable();
    addresses.dedup();
    policy
        .validate_resolved(host, &addresses)
        .and_then(|()| policy.validate_transport(transport))
        .map_err(|_| RtmpDestinationResolverError::Policy)?;
    if addresses.iter().any(|destination| {
        listener_addresses.iter().any(|listener| {
            listener.port() == destination.port()
                && (listener.ip() == destination.ip()
                    || listener.ip().is_unspecified()
                        && listener.is_ipv4() == destination.is_ipv4())
        })
    }) {
        return Err(RtmpDestinationResolverError::DirectLoop);
    }
    addresses
        .into_iter()
        .find(|address| family.is_none_or(|is_ipv4| address.is_ipv4() == is_ipv4))
        .ok_or(RtmpDestinationResolverError::FamilyMismatch)
}
