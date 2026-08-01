use std::{
    error::Error,
    fmt, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering, fence},
    },
};

use http::{
    Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::{
    HealthStartup, HttpHostSelector, HttpPathSelector, UpstreamAlgorithm, UpstreamEndpoint,
    canonicalize_http_path,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    sync::Notify,
    time::{Instant, timeout_at},
};

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostMatcher {
    Any,
    ExactAuthority(String),
    AsciiCaseInsensitiveExactAuthority(String),
    NormalizedExact(String),
    Wildcard(String),
    NginxLeadingWildcard(String),
    NginxLeadingDot(String),
}

impl HostMatcher {
    fn rank_if_matches(&self, authority: Option<&Authority>) -> Option<(u8, usize)> {
        match self {
            Self::Any => Some((0, 0)),
            Self::ExactAuthority(expected) => authority
                .is_some_and(|authority| authority.as_str() == expected)
                .then_some((3, expected.len())),
            Self::AsciiCaseInsensitiveExactAuthority(expected) => authority
                .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(expected))
                .then_some((3, expected.len())),
            Self::NormalizedExact(expected) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| host == *expected)
                .then_some((2, expected.len())),
            Self::Wildcard(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| wildcard_matches(&host, suffix))
                .then_some((1, suffix.len())),
            Self::NginxLeadingWildcard(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| nginx_suffix_matches(&host, suffix, false))
                .then_some((1, suffix.len())),
            Self::NginxLeadingDot(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| nginx_suffix_matches(&host, suffix, true))
                .then_some((1, suffix.len())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathMatcher {
    RawPrefix(String),
    SegmentPrefix(String),
    Exact(String),
    AsciiCaseInsensitiveExact(String),
}

impl PathMatcher {
    fn rank_if_matches(&self, path: &str) -> Option<(u8, usize)> {
        let (rank, value, matches) = match self {
            Self::RawPrefix(value) => (0, value, path.starts_with(value)),
            Self::SegmentPrefix(value) => (
                1,
                value,
                value == "/"
                    || path == value
                    || path
                        .strip_prefix(value)
                        .is_some_and(|remainder| remainder.starts_with('/')),
            ),
            Self::Exact(value) => (2, value, path == value),
            Self::AsciiCaseInsensitiveExact(value) => (2, value, path.eq_ignore_ascii_case(value)),
        };
        matches.then_some((rank, value.len()))
    }

    fn value(&self) -> &str {
        match self {
            Self::RawPrefix(value)
            | Self::SegmentPrefix(value)
            | Self::Exact(value)
            | Self::AsciiCaseInsensitiveExact(value) => value,
        }
    }
}

/// A normalized HTTP route associated with a pool identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    host: HostMatcher,
    path: PathMatcher,
    methods: Option<Box<[Method]>>,
    identity: String,
}

impl Route {
    /// Creates a route from canonical host/path selectors, an optional method set, and an identity.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError`] when the host pattern or path prefix is invalid, the method set is
    /// empty, or the route identity is empty.
    pub fn new(
        host: Option<HttpHostSelector>,
        path: HttpPathSelector,
        methods: Option<Vec<Method>>,
        route_id: impl Into<String>,
    ) -> Result<Self, RouteError> {
        let host = normalize_host(host)?;
        let path = normalize_path(path)?;
        let methods = match methods {
            Some(methods) if methods.is_empty() => return Err(RouteError::EmptyMethodSet),
            Some(methods) => Some(methods.into_boxed_slice()),
            None => None,
        };
        let identity = route_id.into();
        if identity.is_empty() {
            return Err(RouteError::EmptyRouteIdentity);
        }

        Ok(Self {
            host,
            path,
            methods,
            identity,
        })
    }

    #[must_use]
    pub fn path_value(&self) -> &str {
        self.path.value()
    }

    #[must_use]
    pub fn route_id(&self) -> &str {
        &self.identity
    }

    fn matches_method(&self, method: &Method) -> bool {
        self.methods
            .as_ref()
            .is_none_or(|methods| methods.contains(method))
    }
}

/// Errors produced while normalizing a route definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteError {
    InvalidHost(String),
    InvalidPathPrefix(String),
    EmptyMethodSet,
    EmptyRouteIdentity,
    UnsupportedHostSelector,
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHost(host) => write!(formatter, "invalid route host pattern `{host}`"),
            Self::InvalidPathPrefix(path) => {
                write!(formatter, "invalid route path prefix `{path}`")
            }
            Self::EmptyMethodSet => formatter.write_str("route method set cannot be empty"),
            Self::EmptyRouteIdentity => formatter.write_str("route identity cannot be empty"),
            Self::UnsupportedHostSelector => {
                formatter.write_str("route host selector is not implemented by runtime")
            }
        }
    }
}

impl Error for RouteError {}

/// An immutable table that selects from routes in their definition order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteTable {
    routes: Box<[Route]>,
}

impl RouteTable {
    #[must_use]
    pub fn new(routes: Vec<Route>) -> Self {
        Self {
            routes: routes.into_boxed_slice(),
        }
    }

    /// Selects the highest-priority route for the request.
    ///
    /// Host precedence is exact authority, normalized exact/IP, wildcard, then catch-all. Path
    /// precedence is exact, segment prefix, then raw prefix, with longer prefixes winning within a
    /// kind. A method-specific route precedes an any-method route and source order resolves ties.
    #[must_use]
    pub fn select(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<&Route> {
        let authority = authority.or_else(|| uri.authority());
        let path = canonicalize_http_path(uri.path())?;
        let mut best = None;

        for route in &self.routes {
            if !route.matches_method(method) {
                continue;
            }
            let Some(host_rank) = route.host.rank_if_matches(authority) else {
                continue;
            };
            let Some((path_rank, path_length)) = route.path.rank_if_matches(path.as_ref()) else {
                continue;
            };
            let method_rank = u8::from(route.methods.is_some());
            let score = (host_rank, path_rank, path_length, method_rank);

            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((route, score)),
            }
        }

        best.map(|(route, _)| route)
    }
}

fn normalize_host(host: Option<HttpHostSelector>) -> Result<HostMatcher, RouteError> {
    let Some(host) = host else {
        return Ok(HostMatcher::Any);
    };
    match host {
        HttpHostSelector::ExactAuthority { value } => {
            let valid = value.len() <= 255
                && !value.contains(['*', '@'])
                && value
                    .parse::<Authority>()
                    .is_ok_and(|authority| !authority.host().is_empty());
            if !valid {
                return Err(RouteError::InvalidHost(value));
            }
            Ok(HostMatcher::ExactAuthority(value))
        }
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            let valid = value.is_ascii()
                && value.len() <= 255
                && !value.contains(['*', '@'])
                && value
                    .parse::<Authority>()
                    .is_ok_and(|authority| !authority.host().is_empty());
            if !valid {
                return Err(RouteError::InvalidHost(value));
            }
            Ok(HostMatcher::AsciiCaseInsensitiveExactAuthority(value))
        }
        HttpHostSelector::NormalizedHost { mut value } => {
            value.make_ascii_lowercase();
            if let Some(suffix) = value.strip_prefix("*.") {
                if suffix.parse::<IpAddr>().is_ok() || !is_dns_name(suffix) {
                    return Err(RouteError::InvalidHost(value));
                }
                return Ok(HostMatcher::Wildcard(suffix.into()));
            }
            if let Some(ip) = parse_host_ip(&value) {
                return Ok(HostMatcher::NormalizedExact(ip.to_string()));
            }
            if value.len() > 253 || !is_dns_name(&value) {
                return Err(RouteError::InvalidHost(value));
            }
            Ok(HostMatcher::NormalizedExact(value))
        }
        HttpHostSelector::NginxLeadingWildcard { mut value } => {
            value.make_ascii_lowercase();
            if value.len() > 253 || !is_dns_name(&value) {
                return Err(RouteError::InvalidHost(value));
            }
            Ok(HostMatcher::NginxLeadingWildcard(value))
        }
        HttpHostSelector::NginxLeadingDot { mut value } => {
            value.make_ascii_lowercase();
            if value.len() > 253 || !is_dns_name(&value) {
                return Err(RouteError::InvalidHost(value));
            }
            Ok(HostMatcher::NginxLeadingDot(value))
        }
    }
}

fn parse_host_ip(host: &str) -> Option<IpAddr> {
    host.parse::<IpAddr>().ok().or_else(|| {
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .and_then(|host| host.parse::<IpAddr>().ok())
            .filter(IpAddr::is_ipv6)
    })
}

fn is_dns_name(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|label| {
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

fn normalize_path(path: HttpPathSelector) -> Result<PathMatcher, RouteError> {
    let value = match &path {
        HttpPathSelector::SegmentPrefix { value }
        | HttpPathSelector::RawPrefix { value }
        | HttpPathSelector::Exact { value }
        | HttpPathSelector::AsciiCaseInsensitiveExact { value } => value,
    };
    let valid_path = value.starts_with('/')
        && value
            .parse::<PathAndQuery>()
            .is_ok_and(|path| path.query().is_none() && path.path() == value);
    if !valid_path {
        return Err(RouteError::InvalidPathPrefix(value.clone()));
    }
    let Some(canonical) = canonicalize_http_path(value) else {
        return Err(RouteError::InvalidPathPrefix(value.clone()));
    };
    if canonical.as_ref() != value {
        return Err(RouteError::InvalidPathPrefix(value.clone()));
    }
    Ok(match path {
        HttpPathSelector::SegmentPrefix { value } => PathMatcher::SegmentPrefix(value),
        HttpPathSelector::RawPrefix { value } => PathMatcher::RawPrefix(value),
        HttpPathSelector::Exact { value } => PathMatcher::Exact(value),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => {
            PathMatcher::AsciiCaseInsensitiveExact(value)
        }
    })
}

fn wildcard_matches(host: &str, suffix: &str) -> bool {
    let Some(suffix_start) = host.len().checked_sub(suffix.len()) else {
        return false;
    };
    let Some(dot_index) = suffix_start.checked_sub(1) else {
        return false;
    };

    host.as_bytes().get(dot_index) == Some(&b'.')
        && host[suffix_start..].eq_ignore_ascii_case(suffix)
        && dot_index > 0
        && !host[..dot_index].contains('.')
}

fn nginx_suffix_matches(host: &str, suffix: &str, include_base: bool) -> bool {
    if include_base && host == suffix {
        return true;
    }
    host.len() > suffix.len()
        && host.ends_with(suffix)
        && host
            .as_bytes()
            .get(host.len() - suffix.len() - 1)
            .is_some_and(|separator| *separator == b'.')
}

fn normalized_authority_host(authority: &Authority) -> Option<String> {
    let authority_host = authority.host();
    let host = authority_host.strip_suffix('.').unwrap_or(authority_host);
    if let Some(ip) = parse_host_ip(host) {
        Some(ip.to_string())
    } else if is_dns_name(host) {
        Some(host.to_ascii_lowercase())
    } else {
        None
    }
}

/// A validated upstream endpoint retained in its canonical, typed form at runtime.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeEndpoint {
    Socket { address: SocketAddr },
    Dns { host: String, port: u16 },
    Unix { path: PathBuf },
}

pub(crate) const MAX_RESOLVED_ENDPOINT_ADDRESSES: usize = 16;

impl RuntimeEndpoint {
    fn preflight(&self) -> Result<(), PoolError> {
        match self {
            Self::Socket { address } if address.port() == 0 => {
                Err(PoolError::InvalidSocketEndpoint(*address))
            }
            Self::Dns { host, port }
                if *port == 0 || host.parse::<IpAddr>().is_ok() || !is_dns_name(host) =>
            {
                Err(PoolError::InvalidDnsEndpoint {
                    host: host.clone(),
                    port: *port,
                })
            }
            Self::Unix { path } if !valid_unix_endpoint(path) => {
                Err(PoolError::InvalidUnixEndpoint(path.clone()))
            }
            Self::Socket { .. } | Self::Dns { .. } | Self::Unix { .. } => Ok(()),
        }
    }

    pub(crate) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        match self {
            Self::Socket { address } => Ok(vec![*address]),
            Self::Dns { host, port } => {
                let addresses = tokio::net::lookup_host((host.as_str(), *port)).await?;
                self.order_addresses(addresses)
            }
            Self::Unix { path } => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Unix endpoint `{}` does not resolve through DNS",
                    path.display()
                ),
            )),
        }
    }

    pub(crate) fn order_addresses(
        &self,
        addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> io::Result<Vec<SocketAddr>> {
        let mut normalized = Vec::with_capacity(MAX_RESOLVED_ENDPOINT_ADDRESSES);
        for address in addresses {
            if normalized.contains(&address) {
                continue;
            }
            if normalized.len() == MAX_RESOLVED_ENDPOINT_ADDRESSES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "DNS endpoint `{self}` returned more than {MAX_RESOLVED_ENDPOINT_ADDRESSES} addresses"
                    ),
                ));
            }
            normalized.push(address);
        }
        let mut addresses = normalized;
        addresses.sort_unstable();
        if addresses.is_empty() {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("DNS endpoint `{self}` returned no addresses"),
            ))
        } else {
            Ok(addresses)
        }
    }
}

impl From<SocketAddr> for RuntimeEndpoint {
    fn from(address: SocketAddr) -> Self {
        Self::Socket { address }
    }
}

impl TryFrom<&UpstreamEndpoint> for RuntimeEndpoint {
    type Error = PoolError;

    fn try_from(endpoint: &UpstreamEndpoint) -> Result<Self, Self::Error> {
        let endpoint = match endpoint {
            UpstreamEndpoint::Socket { address } => Self::Socket { address: *address },
            UpstreamEndpoint::Dns { host, port } => Self::Dns {
                host: host.clone(),
                port: *port,
            },
            UpstreamEndpoint::Unix { path } => Self::Unix { path: path.clone() },
        };
        endpoint.preflight()?;
        Ok(endpoint)
    }
}

impl fmt::Display for RuntimeEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { address } => address.fmt(formatter),
            Self::Dns { host, port } => write!(formatter, "{host}:{port}"),
            Self::Unix { path } => path.display().fmt(formatter),
        }
    }
}

impl Serialize for RuntimeEndpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

fn valid_unix_endpoint(path: &Path) -> bool {
    if path.to_str().is_none() || !path.is_absolute() {
        return false;
    }
    #[cfg(unix)]
    {
        std::os::unix::net::SocketAddr::from_pathname(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum EndpointHealthState {
    Unchecked,
    Unknown,
    Healthy,
    Unhealthy,
}

impl EndpointHealthState {
    const fn selectable(self) -> bool {
        matches!(self, Self::Unchecked | Self::Healthy)
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Unchecked,
            2 => Self::Healthy,
            3 => Self::Unhealthy,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum HealthFailure {
    Timeout = 1,
    ConnectFailed = 2,
    UnexpectedStatus = 3,
    ProtocolError = 4,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum AdministrativeState {
    #[default]
    Ready,
    Drain,
    Maintenance,
}

impl AdministrativeState {
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Drain,
            2 => Self::Maintenance,
            _ => Self::Ready,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum HealthOverride {
    #[default]
    Auto,
    Up,
    Down,
}

impl HealthOverride {
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Up,
            2 => Self::Down,
            _ => Self::Auto,
        }
    }
}

impl HealthFailure {
    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Timeout),
            2 => Some(Self::ConnectFailed),
            3 => Some(Self::UnexpectedStatus),
            4 => Some(Self::ProtocolError),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeServer {
    pub(crate) name: String,
    pub(crate) endpoint: RuntimeEndpoint,
    pub(crate) max_connections: Option<u64>,
    pub(crate) pinned_addresses: Option<Arc<[SocketAddr]>>,
    pub(crate) protected_addresses: Arc<[SocketAddr]>,
}

#[derive(Debug)]
struct PoolEndpoint {
    active_work: AtomicU64,
    administrative_state: AtomicU8,
    checks_enabled: AtomicBool,
    health_override: AtomicU8,
    endpoint: RuntimeEndpoint,
    configured_max_connections: Option<u64>,
    max_connections_override: AtomicU64,
    name: String,
    pinned_addresses: Option<RwLock<Arc<[SocketAddr]>>>,
    protected_addresses: Arc<[SocketAddr]>,
    state: AtomicU8,
    last_checked_at_unix_ms: AtomicU64,
    last_transition_at_unix_ms: AtomicU64,
    successful_checks: AtomicU64,
    failed_checks: AtomicU64,
    consecutive_successes: AtomicU64,
    consecutive_failures: AtomicU64,
    last_failure: AtomicU8,
}

impl PoolEndpoint {
    fn new(server: RuntimeServer, startup: Option<HealthStartup>) -> Self {
        let state = match startup {
            None => EndpointHealthState::Unchecked,
            Some(HealthStartup::Healthy) => EndpointHealthState::Healthy,
            Some(HealthStartup::Unhealthy) => EndpointHealthState::Unhealthy,
            Some(HealthStartup::Checking) => EndpointHealthState::Unknown,
        };
        Self {
            active_work: AtomicU64::new(0),
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            checks_enabled: AtomicBool::new(true),
            health_override: AtomicU8::new(HealthOverride::Auto as u8),
            endpoint: server.endpoint,
            configured_max_connections: server.max_connections,
            max_connections_override: AtomicU64::new(0),
            name: server.name,
            pinned_addresses: server.pinned_addresses.map(RwLock::new),
            protected_addresses: server.protected_addresses,
            state: AtomicU8::new(state as u8),
            last_checked_at_unix_ms: AtomicU64::new(0),
            last_transition_at_unix_ms: AtomicU64::new(0),
            successful_checks: AtomicU64::new(0),
            failed_checks: AtomicU64::new(0),
            consecutive_successes: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_failure: AtomicU8::new(0),
        }
    }

    fn state(&self) -> EndpointHealthState {
        EndpointHealthState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn selectable(&self) -> bool {
        self.administrative_state() == AdministrativeState::Ready
            && match self.health_override() {
                HealthOverride::Auto => self.state().selectable(),
                HealthOverride::Up => true,
                HealthOverride::Down => false,
            }
    }

    fn has_capacity(&self) -> bool {
        self.max_connections()
            .is_none_or(|limit| self.active_work.load(Ordering::Acquire) < limit)
    }

    fn administrative_state(&self) -> AdministrativeState {
        AdministrativeState::from_u8(self.administrative_state.load(Ordering::Acquire))
    }

    fn health_override(&self) -> HealthOverride {
        HealthOverride::from_u8(self.health_override.load(Ordering::Acquire))
    }

    fn max_connections(&self) -> Option<u64> {
        match self.max_connections_override.load(Ordering::Acquire) {
            0 => self.configured_max_connections,
            limit => Some(limit),
        }
    }

    fn checks_running(&self) -> bool {
        self.checks_enabled.load(Ordering::Acquire)
            && self.administrative_state() != AdministrativeState::Maintenance
    }

    fn try_acquire(self: &Arc<Self>, queue: &Arc<PoolQueue>) -> Option<EndpointLease> {
        if !self.selectable() {
            return None;
        }
        self.try_acquire_capacity()?;
        if !self.selectable() {
            self.release_capacity(queue);
            return None;
        }
        Some(EndpointLease::acquired(Arc::clone(self), Arc::clone(queue)))
    }

    fn try_acquire_capacity(&self) -> Option<()> {
        self.active_work
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if !self.selectable() || self.max_connections().is_some_and(|limit| active >= limit)
                {
                    None
                } else {
                    active.checked_add(1)
                }
            })
            .ok()?;
        Some(())
    }

    fn release_capacity(&self, queue: &PoolQueue) {
        let released =
            self.active_work
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_sub(1)
                });
        debug_assert!(released.is_ok(), "endpoint lease counter underflow");
        queue.notify_capacity_waiters();
    }

    async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        let addresses = if let Some(addresses) = &self.pinned_addresses {
            addresses
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_vec()
        } else {
            self.endpoint.resolve_addresses().await?
        };
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses)
    }

    async fn resolve_fresh_dns(&self) -> io::Result<Vec<SocketAddr>> {
        if !matches!(self.endpoint, RuntimeEndpoint::Dns { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "upstream server does not use DNS",
            ));
        }
        let addresses = self.endpoint.resolve_addresses().await?;
        self.reject_protected_addresses(&addresses)?;
        Ok(addresses)
    }

    fn commit_dns(&self, addresses: &[SocketAddr]) -> io::Result<()> {
        if !matches!(self.endpoint, RuntimeEndpoint::Dns { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "upstream server does not use DNS",
            ));
        }
        self.reject_protected_addresses(addresses)?;
        if let Some(pinned) = &self.pinned_addresses {
            *pinned
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = addresses.to_vec().into();
        }
        Ok(())
    }

    fn reject_protected_addresses(&self, addresses: &[SocketAddr]) -> io::Result<()> {
        if addresses.iter().any(|address| {
            self.protected_addresses.iter().any(|protected| {
                address.port() == protected.port()
                    && (address.ip() == protected.ip() || protected.ip().is_unspecified())
            })
        }) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "upstream DNS resolved to a protected management or statistics listener",
            ))
        } else {
            Ok(())
        }
    }

    fn record(
        &self,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> Option<(EndpointHealthState, EndpointHealthState)> {
        if let Some(at_unix_ms) = at_unix_ms {
            self.last_checked_at_unix_ms
                .store(at_unix_ms, Ordering::Relaxed);
        }
        let previous = self.state();
        let next = if healthy {
            self.successful_checks.fetch_add(1, Ordering::Relaxed);
            let consecutive = self
                .consecutive_successes
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.last_failure.store(0, Ordering::Relaxed);
            if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Unhealthy
            ) && consecutive >= u64::from(healthy_threshold)
            {
                EndpointHealthState::Healthy
            } else {
                previous
            }
        } else {
            self.failed_checks.fetch_add(1, Ordering::Relaxed);
            let consecutive = self
                .consecutive_failures
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            self.consecutive_successes.store(0, Ordering::Relaxed);
            self.last_failure.store(
                failure.map_or(0, |failure| failure as u8),
                Ordering::Relaxed,
            );
            if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Healthy
            ) && consecutive >= u64::from(unhealthy_threshold)
            {
                EndpointHealthState::Unhealthy
            } else {
                previous
            }
        };
        if next == previous {
            None
        } else {
            self.state.store(next as u8, Ordering::Release);
            if let Some(at_unix_ms) = at_unix_ms {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
            Some((previous, next))
        }
    }

    fn snapshot(&self) -> EndpointHealthSnapshot {
        EndpointHealthSnapshot {
            active_connections: self.active_work.load(Ordering::Relaxed),
            administrative_state: self.administrative_state(),
            address: self.endpoint.clone(),
            checks_enabled: self.checks_enabled.load(Ordering::Acquire),
            checks_running: self.checks_running(),
            configured_max_connections: self.configured_max_connections,
            health_override: self.health_override(),
            max_connections: self.max_connections(),
            name: self.name.clone(),
            state: self.state(),
            last_checked_at_unix_ms: nonzero(self.last_checked_at_unix_ms.load(Ordering::Relaxed)),
            last_transition_at_unix_ms: nonzero(
                self.last_transition_at_unix_ms.load(Ordering::Relaxed),
            ),
            successful_checks: self.successful_checks.load(Ordering::Relaxed),
            failed_checks: self.failed_checks.load(Ordering::Relaxed),
            consecutive_successes: self.consecutive_successes.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            last_failure: HealthFailure::from_u8(self.last_failure.load(Ordering::Relaxed)),
        }
    }
}

const fn nonzero(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointHealthSnapshot {
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub active_connections: u64,
    pub administrative_state: AdministrativeState,
    pub address: RuntimeEndpoint,
    pub checks_enabled: bool,
    pub checks_running: bool,
    pub configured_max_connections: Option<u64>,
    pub health_override: HealthOverride,
    pub max_connections: Option<u64>,
    pub name: String,
    pub state: EndpointHealthState,
    pub last_checked_at_unix_ms: Option<u64>,
    pub last_transition_at_unix_ms: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub successful_checks: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub failed_checks: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub consecutive_successes: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub consecutive_failures: u64,
    pub last_failure: Option<HealthFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolHealthSnapshot {
    pub name: String,
    pub algorithm: &'static str,
    pub available_endpoints: usize,
    pub total_endpoints: usize,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub unavailable_selections: u64,
    pub queued: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queued_total: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queue_timeouts: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub queue_cancellations: u64,
    pub endpoints: Vec<EndpointHealthSnapshot>,
}

#[derive(Debug)]
struct PoolQueue {
    // The pool's immutable queue timeout owns both selector and connector lifetime waiting.
    capacity_waiters_possible: bool,
    cancellations: AtomicU64,
    generation: AtomicU64,
    notify: Notify,
    #[cfg(test)]
    notifications: AtomicU64,
    queued: AtomicU64,
    queued_total: AtomicU64,
    #[cfg(test)]
    lifetime_waiters: AtomicU64,
    timeouts: AtomicU64,
}

impl PoolQueue {
    fn new(capacity_waiters_possible: bool) -> Self {
        Self {
            capacity_waiters_possible,
            cancellations: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            notify: Notify::new(),
            #[cfg(test)]
            notifications: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            queued_total: AtomicU64::new(0),
            #[cfg(test)]
            lifetime_waiters: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
        }
    }

    fn notify_capacity_waiters(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        #[cfg(test)]
        self.notifications.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
}

/// A health-aware selector over a fixed, nonempty named-server list.
#[derive(Debug)]
pub struct EndpointPool {
    algorithm: UpstreamAlgorithm,
    name: String,
    endpoints: Box<[Arc<PoolEndpoint>]>,
    health_version: AtomicU64,
    health_writer: Mutex<()>,
    selection: Mutex<SelectionState>,
    queue: Arc<PoolQueue>,
    queue_timeout: Option<std::time::Duration>,
    unavailable_selections: AtomicU64,
}

#[derive(Debug, Default)]
struct SelectionState {
    next: usize,
}

/// Compatibility name retained for monitoring and topology consumers during the pool transition.
pub type RoundRobinPool = EndpointPool;

#[derive(Debug)]
pub struct EndpointLease {
    inner: Arc<EndpointLeaseInner>,
}

#[derive(Debug)]
struct EndpointLeaseInner {
    acquired: AtomicBool,
    deadline: Option<Instant>,
    queue: Arc<PoolQueue>,
    server: Arc<PoolEndpoint>,
}

impl EndpointLease {
    fn acquired(server: Arc<PoolEndpoint>, queue: Arc<PoolQueue>) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(true),
                deadline: None,
                queue,
                server,
            }),
        }
    }

    fn pending(
        server: Arc<PoolEndpoint>,
        queue: Arc<PoolQueue>,
        queue_timeout: Option<std::time::Duration>,
    ) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(false),
                deadline: queue_timeout.map(|timeout| Instant::now() + timeout),
                queue,
                server,
            }),
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &RuntimeEndpoint {
        &self.inner.server.endpoint
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.inner.server.name
    }

    pub(crate) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        self.inner.server.resolve_addresses().await
    }

    pub(crate) fn connection_lifetime(
        &self,
    ) -> std::sync::Weak<dyn pingora::protocols::ConnectionLifetime> {
        let lifetime: Arc<dyn pingora::protocols::ConnectionLifetime> = self.inner.clone();
        Arc::downgrade(&lifetime)
    }
}

impl Drop for EndpointLeaseInner {
    fn drop(&mut self) {
        if !self.acquired.load(Ordering::Acquire) {
            return;
        }
        self.server.release_capacity(&self.queue);
    }
}

#[async_trait::async_trait]
impl pingora::protocols::ConnectionLifetime for EndpointLeaseInner {
    fn try_acquire(&self) -> pingora::Result<bool> {
        if self.acquired.load(Ordering::Acquire) {
            return Ok(true);
        }
        if self.server.try_acquire_capacity().is_none() {
            return Ok(false);
        }
        self.acquired.store(true, Ordering::Release);
        Ok(true)
    }

    fn capacity_generation(&self) -> u64 {
        self.queue.generation.load(Ordering::Acquire)
    }

    async fn wait_for_capacity(&self, generation: u64) -> pingora::Result<()> {
        let Some(deadline) = self.deadline else {
            return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
        };
        #[cfg(test)]
        let _waiting = LifetimeWaitGuard::new(&self.queue.lifetime_waiters);
        loop {
            if self.queue.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.queue.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            if timeout_at(deadline, notified).await.is_err() {
                self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
                return Err(pingora::Error::new_up(pingora::ErrorType::HTTPStatus(503)));
            }
        }
    }

    fn notify_reusable(&self) {
        if self.queue.capacity_waiters_possible {
            self.queue.notify_capacity_waiters();
        }
    }
}

#[cfg(test)]
struct LifetimeWaitGuard<'a> {
    waiters: &'a AtomicU64,
}

#[cfg(test)]
impl<'a> LifetimeWaitGuard<'a> {
    fn new(waiters: &'a AtomicU64) -> Self {
        waiters.fetch_add(1, Ordering::Relaxed);
        Self { waiters }
    }
}

#[cfg(test)]
impl Drop for LifetimeWaitGuard<'_> {
    fn drop(&mut self) {
        let released = self
            .waiters
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |waiters| {
                waiters.checked_sub(1)
            });
        debug_assert!(released.is_ok(), "lifetime waiter counter underflow");
    }
}

struct QueueWaitGuard {
    completed: bool,
    queue: Arc<PoolQueue>,
}

impl QueueWaitGuard {
    fn new(queue: Arc<PoolQueue>) -> Self {
        queue.queued.fetch_add(1, Ordering::Relaxed);
        queue.queued_total.fetch_add(1, Ordering::Relaxed);
        Self {
            completed: false,
            queue,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for QueueWaitGuard {
    fn drop(&mut self) {
        let released =
            self.queue
                .queued
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
                    queued.checked_sub(1)
                });
        debug_assert!(released.is_ok(), "pool queue counter underflow");
        if !self.completed {
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct SelectionAttempt {
    lease: Option<EndpointLease>,
    pool_available: bool,
    saturated: bool,
}

impl EndpointPool {
    /// Creates an unchecked round-robin pool from socket addresses.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an address cannot be represented by the runtime.
    pub fn new(endpoints: impl IntoIterator<Item = SocketAddr>) -> Result<Self, PoolError> {
        Self::from_endpoints(
            endpoints.into_iter().map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
        )
    }

    /// Creates an unchecked pool using the configured balancing algorithm.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
    ) -> Result<Self, PoolError> {
        Self::new_named(String::new(), endpoints, algorithm, false)
    }

    pub(crate) fn new_named(
        name: String,
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        checked: bool,
    ) -> Result<Self, PoolError> {
        let servers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, endpoint)| RuntimeServer {
                name: index.to_string(),
                endpoint,
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            });
        Self::new_named_servers(
            name,
            servers,
            algorithm,
            checked.then_some(HealthStartup::Checking),
            None,
        )
    }

    pub(crate) fn new_named_servers(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
    ) -> Result<Self, PoolError> {
        let endpoints = servers
            .into_iter()
            .map(|server| {
                server.endpoint.preflight()?;
                Ok(Arc::new(PoolEndpoint::new(server, startup)))
            })
            .collect::<Result<Vec<_>, PoolError>>()?
            .into_boxed_slice();
        if endpoints.is_empty() {
            return Err(PoolError::Empty);
        }
        Ok(Self {
            algorithm,
            name,
            endpoints,
            health_version: AtomicU64::new(0),
            health_writer: Mutex::new(()),
            selection: Mutex::new(SelectionState::default()),
            queue: Arc::new(PoolQueue::new(queue_timeout.is_some())),
            queue_timeout,
            unavailable_selections: AtomicU64::new(0),
        })
    }

    fn select_server_excluding(&self, excluded: &[String]) -> SelectionAttempt {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = selection.next;
        let (candidates, pool_available) = self.read_health(|endpoints| {
            let pool_available = endpoints.iter().any(|server| server.selectable());
            let candidates = (0..endpoints.len())
                .filter_map(|offset| {
                    let index = (start + offset) % endpoints.len();
                    let server = &endpoints[index];
                    (server.selectable() && !excluded.contains(&server.name))
                        .then_some((index, server.active_work.load(Ordering::Acquire)))
                })
                .collect::<Vec<_>>();
            (candidates, pool_available)
        });
        let saturated = candidates
            .iter()
            .any(|(index, _)| !self.endpoints[*index].has_capacity());
        let selected = match self.algorithm {
            UpstreamAlgorithm::RoundRobin => candidates
                .iter()
                .map(|(index, _)| *index)
                .find(|index| self.endpoints[*index].has_capacity()),
            UpstreamAlgorithm::LeastConnections => candidates
                .iter()
                .filter(|(index, _)| self.endpoints[*index].has_capacity())
                .min_by_key(|(_, active)| *active)
                .map(|(index, _)| *index),
            UpstreamAlgorithm::First => candidates
                .iter()
                .map(|(index, _)| *index)
                .filter(|index| self.endpoints[*index].has_capacity())
                .min(),
        };
        let lease = selected.and_then(|index| {
            let lease = self.endpoints[index].try_acquire(&self.queue)?;
            if self.algorithm != UpstreamAlgorithm::First {
                selection.next = (index + 1) % self.endpoints.len();
            }
            Some(lease)
        });
        SelectionAttempt {
            lease,
            pool_available,
            saturated: saturated && !candidates.is_empty(),
        }
    }

    pub(crate) fn select_connection_target_excluding(
        &self,
        excluded: &[String],
    ) -> Option<EndpointLease> {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = selection.next;
        let candidates = self.read_health(|endpoints| {
            (0..endpoints.len())
                .filter_map(|offset| {
                    let index = (start + offset) % endpoints.len();
                    let server = &endpoints[index];
                    (server.selectable() && !excluded.contains(&server.name))
                        .then_some((index, server.active_work.load(Ordering::Acquire)))
                })
                .collect::<Vec<_>>()
        });
        let available = candidates
            .iter()
            .copied()
            .filter(|(index, _)| self.endpoints[*index].has_capacity())
            .collect::<Vec<_>>();
        let candidates = if available.is_empty() {
            &candidates
        } else {
            &available
        };
        let selected = match self.algorithm {
            UpstreamAlgorithm::RoundRobin => candidates.first().map(|(index, _)| *index),
            UpstreamAlgorithm::LeastConnections => candidates
                .iter()
                .min_by_key(|(_, active)| *active)
                .map(|(index, _)| *index),
            UpstreamAlgorithm::First => candidates.iter().map(|(index, _)| *index).min(),
        };
        let Some(index) = selected else {
            self.note_unavailable_selection();
            return None;
        };
        if self.algorithm != UpstreamAlgorithm::First {
            selection.next = (index + 1) % self.endpoints.len();
        }
        Some(EndpointLease::pending(
            Arc::clone(&self.endpoints[index]),
            Arc::clone(&self.queue),
            self.queue_timeout,
        ))
    }

    #[must_use]
    pub fn select(&self) -> Option<EndpointLease> {
        self.select_excluding(&[])
    }

    #[must_use]
    pub fn select_excluding(&self, excluded: &[RuntimeEndpoint]) -> Option<EndpointLease> {
        let names = self
            .endpoints
            .iter()
            .filter(|server| excluded.contains(&server.endpoint))
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let attempt = self.select_server_excluding(&names);
        if attempt.lease.is_none() && !attempt.pool_available {
            self.note_unavailable_selection();
        }
        attempt.lease
    }

    pub async fn select_wait(&self) -> Option<EndpointLease> {
        self.select_wait_excluding(&[]).await
    }

    pub(crate) async fn select_wait_excluding(&self, excluded: &[String]) -> Option<EndpointLease> {
        let first = self.select_server_excluding(excluded);
        if let Some(lease) = first.lease {
            return Some(lease);
        }
        let Some(queue_timeout) = self.queue_timeout.filter(|_| first.saturated) else {
            if !first.pool_available {
                self.note_unavailable_selection();
            }
            return None;
        };
        let deadline = Instant::now() + queue_timeout;
        let mut waiting = QueueWaitGuard::new(Arc::clone(&self.queue));
        loop {
            let generation = self.queue.generation.load(Ordering::Acquire);
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let attempt = self.select_server_excluding(excluded);
            if let Some(lease) = attempt.lease {
                waiting.complete();
                return Some(lease);
            }
            if !attempt.pool_available || !attempt.saturated {
                waiting.complete();
                if !attempt.pool_available {
                    self.note_unavailable_selection();
                }
                return None;
            }
            if self.queue.generation.load(Ordering::Acquire) != generation {
                continue;
            }
            if timeout_at(deadline, notified).await.is_err() {
                waiting.complete();
                self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }
    }

    pub(crate) fn select_server_connection_target(&self, name: &str) -> Option<EndpointLease> {
        let server = self.endpoints.iter().find(|server| server.name == name)?;
        server.selectable().then(|| {
            EndpointLease::pending(
                Arc::clone(server),
                Arc::clone(&self.queue),
                self.queue_timeout,
            )
        })
    }

    #[must_use]
    pub fn algorithm(&self) -> UpstreamAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|server| server.selectable() && !attempted.contains(&server.endpoint))
        })
    }

    #[must_use]
    pub(crate) fn has_unattempted_servers(&self, attempted: &[String]) -> bool {
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|server| server.selectable() && !attempted.contains(&server.name))
        })
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|server| server.selectable() && server.has_capacity())
        })
    }

    pub(crate) fn note_unavailable_selection(&self) {
        self.unavailable_selections.fetch_add(1, Ordering::Relaxed);
    }

    /// Changes one named server's administrative state. Existing leases continue draining.
    ///
    /// # Errors
    ///
    /// Returns an error when this pool has no server with the exact canonical name.
    pub fn set_server_administrative_state(
        &self,
        name: &str,
        state: AdministrativeState,
    ) -> Result<(), PoolAdminError> {
        let server = self.server(name)?;
        server
            .administrative_state
            .store(state as u8, Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    pub fn set_administrative_state(&self, state: AdministrativeState) {
        for server in &self.endpoints {
            server
                .administrative_state
                .store(state as u8, Ordering::Release);
        }
        self.queue.notify_capacity_waiters();
    }

    /// Sets a selection-only health override without changing observed health.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_health_override(
        &self,
        name: &str,
        health_override: HealthOverride,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .health_override
            .store(health_override as u8, Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Enables or disables configured health probes for one server.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_checks_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .checks_enabled
            .store(enabled, Ordering::Release);
        Ok(())
    }

    /// Sets a runtime capacity override, or restores configured capacity with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for zero capacity or an unknown exact server name.
    pub fn set_server_max_connections(
        &self,
        name: &str,
        limit: Option<u64>,
    ) -> Result<(), PoolAdminError> {
        if limit == Some(0) {
            return Err(PoolAdminError::InvalidMaxConnections);
        }
        self.server(name)?
            .max_connections_override
            .store(limit.unwrap_or(0), Ordering::Release);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    #[must_use]
    pub fn has_server(&self, name: &str) -> bool {
        self.endpoints.iter().any(|server| server.name == name)
    }

    fn server(&self, name: &str) -> Result<&Arc<PoolEndpoint>, PoolAdminError> {
        self.endpoints
            .iter()
            .find(|server| server.name == name)
            .ok_or_else(|| PoolAdminError::UnknownServer(name.to_owned()))
    }

    pub(crate) fn endpoints(&self) -> impl Iterator<Item = (usize, RuntimeEndpoint)> + '_ {
        self.endpoints
            .iter()
            .enumerate()
            .map(|(index, server)| (index, server.endpoint.clone()))
    }

    pub(crate) fn servers(&self) -> impl Iterator<Item = (usize, String, RuntimeEndpoint)> + '_ {
        self.endpoints
            .iter()
            .enumerate()
            .map(|(index, server)| (index, server.name.clone(), server.endpoint.clone()))
    }

    pub(crate) fn record_health(
        &self,
        index: usize,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> Option<(EndpointHealthState, EndpointHealthState)> {
        let _writer = self
            .health_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.health_version.fetch_add(1, Ordering::AcqRel);
        let transition = self.endpoints.get(index).and_then(|server| {
            server.record(
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
        });
        self.health_version.fetch_add(1, Ordering::Release);
        if transition.is_some() {
            self.queue.notify_capacity_waiters();
        }
        transition
    }

    pub(crate) fn health_state(&self, index: usize) -> Option<EndpointHealthState> {
        self.endpoints.get(index).map(|server| server.state())
    }

    pub(crate) fn health_checks_running(&self, index: usize) -> bool {
        self.endpoints
            .get(index)
            .is_some_and(|server| server.checks_running())
    }

    /// Resolves one server immediately without mutating runtime state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server or failed/empty DNS resolution.
    pub async fn resolve_server_dns(&self, name: &str) -> Result<Vec<SocketAddr>, PoolAdminError> {
        let server = Arc::clone(self.server(name)?);
        server
            .resolve_fresh_dns()
            .await
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    /// Commits an already-resolved address set without performing external I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server, a non-DNS endpoint, or a protected address.
    pub fn commit_server_dns(
        &self,
        name: &str,
        addresses: &[SocketAddr],
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .commit_dns(addresses)
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    pub(crate) async fn resolve_server_addresses(
        &self,
        index: usize,
    ) -> io::Result<Vec<SocketAddr>> {
        let server = self.endpoints.get(index).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "upstream server index is invalid")
        })?;
        server.resolve_addresses().await
    }

    #[must_use]
    pub fn health_snapshot(&self) -> PoolHealthSnapshot {
        let endpoints = self.read_health(|endpoints| {
            endpoints
                .iter()
                .map(|server| server.snapshot())
                .collect::<Vec<_>>()
        });
        PoolHealthSnapshot {
            name: self.name.clone(),
            algorithm: algorithm_name(self.algorithm),
            available_endpoints: endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.administrative_state == AdministrativeState::Ready
                        && match endpoint.health_override {
                            HealthOverride::Auto => endpoint.state.selectable(),
                            HealthOverride::Up => true,
                            HealthOverride::Down => false,
                        }
                })
                .count(),
            total_endpoints: endpoints.len(),
            unavailable_selections: self.unavailable_selections.load(Ordering::Relaxed),
            queued: self.queue.queued.load(Ordering::Relaxed),
            queued_total: self.queue.queued_total.load(Ordering::Relaxed),
            queue_timeouts: self.queue.timeouts.load(Ordering::Relaxed),
            queue_cancellations: self.queue.cancellations.load(Ordering::Relaxed),
            endpoints,
        }
    }

    fn read_health<T>(&self, read: impl Fn(&[Arc<PoolEndpoint>]) -> T) -> T {
        for _ in 0..8 {
            let version = self.health_version.load(Ordering::Acquire);
            if version % 2 != 0 {
                std::thread::yield_now();
                continue;
            }
            let value = read(&self.endpoints);
            fence(Ordering::Acquire);
            if self.health_version.load(Ordering::Relaxed) == version {
                return value;
            }
        }
        let _writer = self
            .health_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(&self.endpoints)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PoolAdminError {
    #[error("unknown upstream server `{0}`")]
    UnknownServer(String),
    #[error("max-connections must be greater than zero")]
    InvalidMaxConnections,
    #[error("upstream DNS refresh failed")]
    DnsRefreshFailed,
}

const fn algorithm_name(algorithm: UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
        UpstreamAlgorithm::First => "first",
    }
}

/// Errors produced while constructing an endpoint pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolError {
    Empty,
    StartupDns { server: String, detail: String },
    InvalidSocketEndpoint(SocketAddr),
    InvalidDnsEndpoint { host: String, port: u16 },
    InvalidUnixEndpoint(PathBuf),
    ProtectedEndpoint { server: String },
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("upstream endpoint pool cannot be empty"),
            Self::StartupDns { server, detail } => {
                write!(
                    formatter,
                    "upstream server `{server}` startup DNS resolution failed: {detail}"
                )
            }
            Self::InvalidSocketEndpoint(address) => {
                write!(formatter, "invalid socket endpoint `{address}`")
            }
            Self::InvalidDnsEndpoint { host, port } => {
                write!(formatter, "invalid DNS endpoint `{host}:{port}`")
            }
            Self::InvalidUnixEndpoint(path) => {
                write!(formatter, "invalid Unix endpoint `{}`", path.display())
            }
            Self::ProtectedEndpoint { server } => write!(
                formatter,
                "upstream server `{server}` resolves to a protected management or statistics listener"
            ),
        }
    }
}

impl Error for PoolError {}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier, mpsc},
        thread,
        time::Duration,
    };

    use pingora::{
        connectors::http::Connector as HttpConnector,
        http::RequestHeader,
        protocols::{ConnectionLifetime as _, http::client::HttpSession},
        upstreams::peer::HttpPeer,
    };
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };

    use super::*;

    #[test]
    fn normalized_nginx_hosts_accept_one_trailing_dot() {
        let authority = "API.EXAMPLE.TEST.:443"
            .parse::<Authority>()
            .expect("trailing-dot authority");

        assert_eq!(
            normalized_authority_host(&authority).as_deref(),
            Some("api.example.test")
        );
    }

    #[test]
    fn dns_addresses_are_ordered_deterministically_for_every_consumer() {
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };
        let first = SocketAddr::from(([192, 0, 2, 1], 443));
        let second = SocketAddr::from(([192, 0, 2, 2], 443));

        let traffic_addresses = endpoint
            .order_addresses([second, first, second])
            .expect("traffic addresses");
        let health_addresses = endpoint
            .order_addresses([first, second])
            .expect("health addresses");

        assert_eq!(traffic_addresses, vec![first, second]);
        assert_eq!(health_addresses, traffic_addresses);
    }

    #[test]
    fn dns_resolution_rejects_an_empty_address_set() {
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };

        let error = endpoint
            .order_addresses([])
            .expect_err("empty DNS resolution must fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("origin.example.test:443"));
    }

    #[test]
    fn dns_resolution_accepts_the_normalized_address_limit() {
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };
        let expected = resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES);
        let input = expected
            .iter()
            .rev()
            .copied()
            .chain(expected.iter().copied());

        let addresses = endpoint
            .order_addresses(input)
            .expect("address limit is accepted");

        assert_eq!(addresses, expected);
    }

    #[test]
    fn dns_resolution_rejects_normalized_address_overflow() {
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };

        let error = endpoint
            .order_addresses(resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES + 1))
            .expect_err("address overflow must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("returned more than 16 addresses")
        );
    }

    fn resolved_addresses(count: usize) -> Vec<SocketAddr> {
        (1..=count)
            .map(|index| {
                SocketAddr::from((
                    [192, 0, 2, u8::try_from(index).expect("test address octet")],
                    443,
                ))
            })
            .collect()
    }

    fn runtime_server(name: &str, port: u16, max_connections: Option<u64>) -> RuntimeServer {
        RuntimeServer {
            name: name.into(),
            endpoint: RuntimeEndpoint::from(SocketAddr::from(([127, 0, 0, 1], port))),
            max_connections,
            pinned_addresses: None,
            protected_addresses: Arc::from([]),
        }
    }

    fn connection_lifetime(pool: &EndpointPool, name: &str) -> Arc<EndpointLeaseInner> {
        let lease = pool
            .select_server_connection_target(name)
            .expect("connection target");
        Arc::clone(&lease.inner)
    }

    fn peer_with_lifetime(
        address: SocketAddr,
        lifetime: &Arc<EndpointLeaseInner>,
        min_http_version: u8,
        max_http_version: u8,
    ) -> HttpPeer {
        let mut peer = HttpPeer::new(address, false, String::new());
        peer.options
            .set_http_version(max_http_version, min_http_version);
        let lifetime: Arc<dyn pingora::protocols::ConnectionLifetime> = lifetime.clone();
        peer.connection_lifetime = Some(Arc::downgrade(&lifetime));
        peer
    }

    async fn read_request_head(stream: &mut TcpStream) {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.expect("request head");
            request.push(byte[0]);
        }
    }

    async fn acquire_after_capacity_change(lifetime: Arc<EndpointLeaseInner>) {
        loop {
            let generation = lifetime.capacity_generation();
            if lifetime.try_acquire().expect("capacity acquisition") {
                return;
            }
            lifetime
                .wait_for_capacity(generation)
                .await
                .expect("capacity notification");
        }
    }

    async fn wait_for_lifetime_waiters(pool: &EndpointPool, expected: u64) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.queue.lifetime_waiters.load(Ordering::Relaxed) != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lifetime waiter count");
    }

    #[test]
    fn administrative_drain_rejects_new_work_without_revoking_existing_leases() {
        let pool = RoundRobinPool::new_named_servers(
            "api".into(),
            [runtime_server("one", 3000, Some(2))],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool");
        let lease = pool.select().expect("existing lease");

        pool.set_server_administrative_state("one", AdministrativeState::Drain)
            .expect("drain");

        assert!(pool.select().is_none());
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 1);
        drop(lease);
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
    }

    #[test]
    fn maintenance_suspends_checks_while_drain_keeps_checks_running() {
        let pool = RoundRobinPool::new_named_servers(
            "api".into(),
            [runtime_server("one", 3000, None)],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool");

        pool.set_server_administrative_state("one", AdministrativeState::Drain)
            .expect("drain");
        assert!(pool.health_checks_running(0));
        pool.set_server_administrative_state("one", AdministrativeState::Maintenance)
            .expect("maintenance");
        assert!(!pool.health_checks_running(0));
        pool.set_server_checks_enabled("one", false)
            .expect("disable checks");
        pool.set_server_administrative_state("one", AdministrativeState::Ready)
            .expect("ready");
        assert!(!pool.health_checks_running(0));
    }

    #[test]
    fn health_override_is_independent_from_observed_health_and_resets_to_auto() {
        let pool = RoundRobinPool::new_named_servers(
            "api".into(),
            [runtime_server("one", 3000, None)],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool");
        pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
        assert!(pool.select().is_none());

        pool.set_server_health_override("one", HealthOverride::Up)
            .expect("force up");
        assert!(pool.select().is_some());
        let snapshot = pool.health_snapshot().endpoints.remove(0);
        assert_eq!(snapshot.state, EndpointHealthState::Unhealthy);
        assert_eq!(snapshot.health_override, HealthOverride::Up);

        pool.set_server_health_override("one", HealthOverride::Auto)
            .expect("automatic health");
        assert!(pool.select().is_none());
    }

    #[test]
    fn max_connections_override_and_reset_preserve_configured_capacity() {
        let pool = RoundRobinPool::new_named_servers(
            "api".into(),
            [runtime_server("one", 3000, Some(2))],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool");
        pool.set_server_max_connections("one", Some(1))
            .expect("override");
        let first = pool.select().expect("first");
        assert!(pool.select().is_none());
        drop(first);

        pool.set_server_max_connections("one", None).expect("reset");
        let first = pool.select().expect("first after reset");
        let second = pool.select().expect("configured second capacity");
        assert_eq!(pool.health_snapshot().endpoints[0].max_connections, Some(2));
        drop((first, second));
    }

    #[test]
    fn first_uses_the_first_healthy_administrative_server_with_capacity() {
        let pool = RoundRobinPool::new_named_servers(
            "first".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("backup", 3001, Some(1)),
            ],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("first pool");

        let primary = pool.select().expect("primary capacity");
        assert_eq!(primary.server_name(), "primary");
        let backup = pool.select().expect("backup capacity");
        assert_eq!(backup.server_name(), "backup");
        drop(primary);
        assert_eq!(
            pool.select().expect("primary restored").server_name(),
            "primary"
        );
        drop(backup);

        pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
        assert_eq!(
            pool.select().expect("healthy backup").server_name(),
            "backup"
        );
    }

    #[test]
    fn least_connections_uses_named_server_work_and_rotates_equal_ties() {
        let pool = RoundRobinPool::new_named_servers(
            "least".into(),
            [
                runtime_server("one", 3000, Some(2)),
                runtime_server("two", 3001, Some(2)),
                runtime_server("three", 3002, Some(2)),
            ],
            UpstreamAlgorithm::LeastConnections,
            None,
            None,
        )
        .expect("least-connections pool");

        let one = pool.select().expect("one");
        let two = pool.select().expect("two");
        let three = pool.select().expect("three");
        assert_eq!(
            [one.server_name(), two.server_name(), three.server_name()],
            ["one", "two", "three"]
        );
        assert_eq!(
            pool.health_snapshot()
                .endpoints
                .iter()
                .map(|server| (server.name.as_str(), server.active_connections))
                .collect::<Vec<_>>(),
            vec![("one", 1), ("two", 1), ("three", 1)]
        );
    }

    #[tokio::test]
    async fn bounded_capacity_queue_releases_and_times_out_exactly_once() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "queued".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_millis(30)),
            )
            .expect("queued pool"),
        );
        let held = pool.select().expect("initial capacity");
        let waiting_pool = Arc::clone(&pool);
        let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health_snapshot().queued != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiter entered queue");
        drop(held);
        let acquired = waiter
            .await
            .expect("waiter task")
            .expect("released capacity");
        assert_eq!(acquired.server_name(), "only");
        drop(acquired);
        let held = pool.select().expect("capacity after release");
        assert!(pool.select_wait().await.is_none());
        drop(held);

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 2);
        assert_eq!(snapshot.queue_timeouts, 1);
        assert_eq!(snapshot.queue_cancellations, 0);
        assert_eq!(snapshot.endpoints[0].active_connections, 0);
    }

    #[tokio::test]
    async fn reusable_notification_is_skipped_when_queueing_is_immutably_disabled() {
        let pool = RoundRobinPool::new_named_servers(
            "unbounded".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("unbounded pool");
        let lifetime = connection_lifetime(&pool, "only");
        assert!(lifetime.try_acquire().expect("initial acquisition"));

        lifetime.notify_reusable();

        assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn bounded_runtime_override_without_queueing_cannot_create_a_waiter() {
        let pool = RoundRobinPool::new_named_servers(
            "immediate".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("immediate pool");
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        pool.set_server_max_connections("only", Some(1))
            .expect("bounded override");
        let notifications = pool.queue.notifications.load(Ordering::Relaxed);
        let generation = pool.queue.generation.load(Ordering::Acquire);
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("saturated acquisition"));

        assert!(second.wait_for_capacity(generation).await.is_err());
        first.notify_reusable();

        assert_eq!(
            pool.queue.notifications.load(Ordering::Relaxed),
            notifications
        );
        assert_eq!(pool.queue.generation.load(Ordering::Acquire), generation);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn reusable_notification_wakes_a_hidden_bounded_lifetime_waiter() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "bounded".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("bounded pool"),
        );
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("saturated acquisition"));
        let generation = second.capacity_generation();
        let waiting = Arc::clone(&second);
        let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
        wait_for_lifetime_waiters(&pool, 1).await;
        let notifications = pool.queue.notifications.load(Ordering::Relaxed);

        first.notify_reusable();

        waiter
            .await
            .expect("waiter task")
            .expect("reusable notification");
        assert_eq!(
            pool.queue.notifications.load(Ordering::Relaxed),
            notifications + 1
        );
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capacity_overrides_preserve_wakes_in_both_directions() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "overrides".into(),
                [runtime_server("only", 3000, None)],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("override pool"),
        );
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("unbounded acquisition"));
        first.notify_reusable();
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);

        pool.set_server_max_connections("only", Some(1))
            .expect("None to Some override");
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 2);
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("bounded acquisition"));
        let generation = second.capacity_generation();
        let waiting = Arc::clone(&second);
        let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
        wait_for_lifetime_waiters(&pool, 1).await;

        pool.set_server_max_connections("only", None)
            .expect("Some to None override");

        waiter
            .await
            .expect("waiter task")
            .expect("override notification");
        assert!(second.try_acquire().expect("restored unbounded capacity"));
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 3);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn reusable_notification_before_waiter_registration_is_observed_by_generation() {
        let pool = RoundRobinPool::new_named_servers(
            "registration".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("registration pool");
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("saturated acquisition"));
        let generation = second.capacity_generation();

        first.notify_reusable();

        tokio::time::timeout(
            Duration::from_millis(100),
            second.wait_for_capacity(generation),
        )
        .await
        .expect("generation change avoids a lost notification")
        .expect("generation notification");
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn cancelling_a_hidden_lifetime_waiter_releases_test_accounting_once() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "lifetime-cancel".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(10)),
            )
            .expect("cancellation pool"),
        );
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("saturated acquisition"));
        let generation = second.capacity_generation();
        let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
        wait_for_lifetime_waiters(&pool, 1).await;

        waiter.abort();
        assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
        wait_for_lifetime_waiters(&pool, 0).await;
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capacity_generation_wrap_still_wakes_registered_waiters() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "generation-wrap".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("generation pool"),
        );
        pool.queue.generation.store(u64::MAX, Ordering::Release);
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        let second = connection_lifetime(&pool, "only");
        assert!(!second.try_acquire().expect("saturated acquisition"));
        let generation = second.capacity_generation();
        let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
        wait_for_lifetime_waiters(&pool, 1).await;

        first.notify_reusable();

        waiter
            .await
            .expect("waiter task")
            .expect("wrapped generation notification");
        assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn pingora_h1_hidden_waiter_wakes_when_the_connection_becomes_reusable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("H1 listener");
        let address = listener.local_addr().expect("H1 address");
        let (responded_tx, responded_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("H1 accept");
            read_request_head(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .expect("H1 response");
            responded_tx.send(()).expect("H1 response signal");
            let _ = finish_rx.await;
        });
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "pingora-h1".into(),
                [runtime_server("only", address.port(), Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("H1 pool"),
        );
        let first_lifetime = connection_lifetime(&pool, "only");
        let second_lifetime = connection_lifetime(&pool, "only");
        let first_peer = peer_with_lifetime(address, &first_lifetime, 1, 1);
        let second_peer = peer_with_lifetime(address, &second_lifetime, 1, 1);
        let connector = Arc::new(HttpConnector::new(None));
        let (mut first_session, reused) = connector
            .get_http_session(&first_peer)
            .await
            .expect("first H1 session");
        assert!(!reused);
        let HttpSession::H1(first_h1) = &mut first_session else {
            panic!("expected H1 session");
        };
        let mut request = Box::new(RequestHeader::build("GET", b"/", None).expect("H1 request"));
        request.append_header("Host", "localhost").expect("H1 host");
        first_h1
            .write_request_header(request)
            .await
            .expect("H1 request write");
        first_h1.read_response().await.expect("H1 response read");
        first_h1.respect_keepalive();
        while first_h1
            .read_body_bytes()
            .await
            .expect("H1 response body")
            .is_some()
        {}
        responded_rx.await.expect("H1 origin response");
        let waiting_connector = Arc::clone(&connector);
        let waiter =
            tokio::spawn(async move { waiting_connector.get_http_session(&second_peer).await });
        wait_for_lifetime_waiters(&pool, 1).await;

        connector
            .release_http_session(first_session, &first_peer, None)
            .await;

        let (second_session, reused) = waiter
            .await
            .expect("H1 waiter task")
            .expect("second H1 session");
        assert!(reused);
        assert!(matches!(second_session, HttpSession::H1(_)));
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
        drop(second_session);
        finish_tx.send(()).expect("finish H1 origin");
        server.await.expect("H1 server task");
    }

    #[tokio::test]
    async fn pingora_h2_hidden_waiter_wakes_when_a_stream_becomes_reusable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("H2 listener");
        let address = listener.local_addr().expect("H2 address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("H2 accept");
            let mut connection = h2::server::handshake(stream).await.expect("H2 handshake");
            while let Some(request) = connection.accept().await {
                let _ = request.expect("H2 request");
            }
        });
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "pingora-h2".into(),
                [runtime_server("only", address.port(), Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("H2 pool"),
        );
        let first_lifetime = connection_lifetime(&pool, "only");
        let second_lifetime = connection_lifetime(&pool, "only");
        let mut first_peer = peer_with_lifetime(address, &first_lifetime, 2, 2);
        first_peer.options.max_h2_streams = 1;
        let mut second_peer = peer_with_lifetime(address, &second_lifetime, 2, 2);
        second_peer.options.max_h2_streams = 1;
        let connector = Arc::new(HttpConnector::new(None));
        let (first_session, reused) = connector
            .get_http_session(&first_peer)
            .await
            .expect("first H2 session");
        assert!(!reused);
        assert!(matches!(first_session, HttpSession::H2(_)));
        let waiting_connector = Arc::clone(&connector);
        let waiter =
            tokio::spawn(async move { waiting_connector.get_http_session(&second_peer).await });
        wait_for_lifetime_waiters(&pool, 1).await;

        connector
            .release_http_session(first_session, &first_peer, None)
            .await;

        let (second_session, reused) = waiter
            .await
            .expect("H2 waiter task")
            .expect("second H2 session");
        assert!(reused);
        assert!(matches!(second_session, HttpSession::H2(_)));
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
        drop(second_session);
        server.abort();
        assert!(
            server
                .await
                .expect_err("H2 server cancelled")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn multiple_hidden_waiters_survive_runtime_override_churn() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "override-churn".into(),
                [runtime_server("only", 3000, None)],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("override churn pool"),
        );
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("unbounded acquisition"));
        pool.set_server_max_connections("only", Some(1))
            .expect("None to Some override");
        let lifetimes = (0..3)
            .map(|_| connection_lifetime(&pool, "only"))
            .collect::<Vec<_>>();
        let waiters = lifetimes
            .iter()
            .map(|lifetime| tokio::spawn(acquire_after_capacity_change(Arc::clone(lifetime))))
            .collect::<Vec<_>>();
        wait_for_lifetime_waiters(&pool, 3).await;

        pool.set_server_max_connections("only", Some(2))
            .expect("first bounded override");
        wait_for_lifetime_waiters(&pool, 2).await;
        pool.set_server_max_connections("only", Some(3))
            .expect("second bounded override");
        wait_for_lifetime_waiters(&pool, 1).await;
        pool.set_server_max_connections("only", None)
            .expect("restore unbounded capacity");

        for waiter in waiters {
            waiter.await.expect("override waiter task");
        }
        assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 4);
        assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 4);
        drop((first, lifetimes));
    }

    #[test]
    fn reload_keeps_each_old_connection_on_its_original_queue_timeout_invariant() {
        let old_without_queue = RoundRobinPool::new_named_servers(
            "old-immediate".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("old immediate pool");
        let new_with_queue = RoundRobinPool::new_named_servers(
            "new-queued".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("new queued pool");
        let old_immediate = connection_lifetime(&old_without_queue, "only");
        let new_queued = connection_lifetime(&new_with_queue, "only");

        old_immediate.notify_reusable();
        new_queued.notify_reusable();

        assert_eq!(
            old_without_queue
                .queue
                .notifications
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            new_with_queue.queue.notifications.load(Ordering::Relaxed),
            1
        );

        let old_with_queue = RoundRobinPool::new_named_servers(
            "old-queued".into(),
            [runtime_server("only", 3001, None)],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("old queued pool");
        let new_without_queue = RoundRobinPool::new_named_servers(
            "new-immediate".into(),
            [runtime_server("only", 3001, None)],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("new immediate pool");
        let old_queued = connection_lifetime(&old_with_queue, "only");
        let new_immediate = connection_lifetime(&new_without_queue, "only");

        old_queued.notify_reusable();
        new_immediate.notify_reusable();

        assert_eq!(
            old_with_queue.queue.notifications.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            new_without_queue
                .queue
                .notifications
                .load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn public_selector_queue_counters_exclude_hidden_lifetime_waits() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "counter-ownership".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(10)),
            )
            .expect("counter pool"),
        );
        let first = connection_lifetime(&pool, "only");
        assert!(first.try_acquire().expect("initial acquisition"));
        let hidden = connection_lifetime(&pool, "only");
        assert!(!hidden.try_acquire().expect("hidden saturation"));
        let generation = hidden.capacity_generation();
        let hidden_waiter = tokio::spawn(async move { hidden.wait_for_capacity(generation).await });
        wait_for_lifetime_waiters(&pool, 1).await;
        let hidden_snapshot = pool.health_snapshot();
        assert_eq!(hidden_snapshot.queued, 0);
        assert_eq!(hidden_snapshot.queued_total, 0);
        assert_eq!(hidden_snapshot.queue_cancellations, 0);

        let selecting_pool = Arc::clone(&pool);
        let selector_waiter = tokio::spawn(async move { selecting_pool.select_wait().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health_snapshot().queued != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("selector waiter registration");
        let combined_snapshot = pool.health_snapshot();
        assert_eq!(combined_snapshot.queued, 1);
        assert_eq!(combined_snapshot.queued_total, 1);
        assert_eq!(combined_snapshot.queue_cancellations, 0);

        hidden_waiter.abort();
        assert!(
            hidden_waiter
                .await
                .expect_err("hidden waiter cancelled")
                .is_cancelled()
        );
        wait_for_lifetime_waiters(&pool, 0).await;
        assert_eq!(pool.health_snapshot().queued, 1);
        assert_eq!(pool.health_snapshot().queue_cancellations, 0);

        selector_waiter.abort();
        assert!(
            selector_waiter
                .await
                .expect_err("selector waiter cancelled")
                .is_cancelled()
        );
        let final_snapshot = pool.health_snapshot();
        assert_eq!(final_snapshot.queued, 0);
        assert_eq!(final_snapshot.queued_total, 1);
        assert_eq!(final_snapshot.queue_cancellations, 1);
    }

    #[tokio::test]
    async fn capacity_release_cannot_race_a_waiter_notification_registration() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "wakeups".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("queued pool"),
        );

        for _ in 0..256 {
            let held = pool.select().expect("held capacity");
            let waiting_pool = Arc::clone(&pool);
            let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
            tokio::task::yield_now().await;
            drop(held);
            let acquired = tokio::time::timeout(Duration::from_millis(100), waiter)
                .await
                .expect("capacity notification was not lost")
                .expect("waiter task")
                .expect("released capacity");
            drop(acquired);
        }
        assert_eq!(pool.health_snapshot().queued, 0);
    }

    #[tokio::test]
    async fn cancelling_a_capacity_waiter_rolls_back_queue_state_once() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "cancelled".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(10)),
            )
            .expect("queued pool"),
        );
        let held = pool.select().expect("initial capacity");
        let waiting_pool = Arc::clone(&pool);
        let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health_snapshot().queued != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiter entered queue");
        waiter.abort();
        assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
        drop(held);

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 1);
        assert_eq!(snapshot.queue_cancellations, 1);
        assert_eq!(snapshot.queue_timeouts, 0);
        assert_eq!(snapshot.endpoints[0].active_connections, 0);
    }

    #[tokio::test]
    async fn startup_dns_uses_only_the_addresses_pinned_in_the_runtime_plan() {
        let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
        let pool = RoundRobinPool::new_named_servers(
            "pinned".into(),
            [RuntimeServer {
                name: "origin".into(),
                endpoint: RuntimeEndpoint::Dns {
                    host: "origin.example.test".into(),
                    port: 443,
                },
                max_connections: None,
                pinned_addresses: Some(vec![pinned].into()),
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("startup-pinned pool");

        let server = pool.select().expect("pinned server");
        assert_eq!(
            server.resolve_addresses().await.expect("pinned addresses"),
            vec![pinned]
        );
        assert_eq!(server.endpoint().to_string(), "origin.example.test:443");
    }

    #[tokio::test]
    async fn dns_refresh_resolution_does_not_mutate_pinned_addresses_before_commit() {
        let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
        let pool = RoundRobinPool::new_named_servers(
            "pinned".into(),
            [RuntimeServer {
                name: "origin".into(),
                endpoint: RuntimeEndpoint::Dns {
                    host: "localhost".into(),
                    port: 443,
                },
                max_connections: None,
                pinned_addresses: Some(vec![pinned].into()),
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("startup-pinned pool");

        let resolved = pool
            .resolve_server_dns("origin")
            .await
            .expect("external DNS resolution");
        let before_commit = pool.select().expect("server before commit");
        assert_eq!(
            before_commit
                .resolve_addresses()
                .await
                .expect("pinned address"),
            vec![pinned]
        );
        drop(before_commit);

        pool.commit_server_dns("origin", &resolved)
            .expect("atomic DNS commit");
        let after_commit = pool.select().expect("server after commit");
        assert_eq!(
            after_commit
                .resolve_addresses()
                .await
                .expect("committed addresses"),
            resolved
        );
    }

    #[tokio::test]
    async fn on_connect_dns_rejects_a_protected_listener_after_resolution() {
        let protected = SocketAddr::from(([127, 0, 0, 1], 18404));
        let pool = RoundRobinPool::new_named_servers(
            "protected".into(),
            [RuntimeServer {
                name: "rebind".into(),
                endpoint: RuntimeEndpoint::Dns {
                    host: "localhost".into(),
                    port: protected.port(),
                },
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([protected]),
            }],
            UpstreamAlgorithm::RoundRobin,
            None,
            None,
        )
        .expect("protected DNS pool");

        let lease = pool.select().expect("selected DNS server");
        let error = lease
            .resolve_addresses()
            .await
            .expect_err("protected address must be rejected after DNS resolution");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn checked_endpoints_transition_through_thresholds() {
        let pool = RoundRobinPool::new_named(
            "checked".into(),
            [RuntimeEndpoint::from(SocketAddr::from((
                [127, 0, 0, 1],
                3000,
            )))],
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool");

        assert!(pool.select().is_none());
        assert_eq!(
            pool.health_snapshot().endpoints[0].state,
            EndpointHealthState::Unknown
        );

        pool.record_health(0, true, None, Some(100), 2, 2);
        assert!(
            !pool.has_available(),
            "one success remains below startup threshold"
        );
        pool.record_health(0, true, None, Some(150), 2, 2);
        assert!(
            pool.has_available(),
            "the startup threshold establishes state"
        );
        pool.record_health(
            0,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(200),
            2,
            2,
        );
        assert!(pool.has_available(), "one failure remains below threshold");
        pool.record_health(
            0,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(300),
            2,
            2,
        );
        assert!(
            !pool.has_available(),
            "the failure threshold removes the endpoint"
        );
        pool.record_health(0, true, None, Some(400), 2, 2);
        assert!(
            !pool.has_available(),
            "one success remains below recovery threshold"
        );
        pool.record_health(0, true, None, Some(500), 2, 2);
        assert!(
            pool.has_available(),
            "the recovery threshold restores the endpoint"
        );

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.available_endpoints, 1);
        assert_eq!(snapshot.endpoints[0].successful_checks, 4);
        assert_eq!(snapshot.endpoints[0].failed_checks, 2);
        assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(500));
        assert_eq!(snapshot.endpoints[0].last_failure, None);
    }

    #[test]
    fn unchecked_endpoints_remain_selectable_without_observations() {
        let endpoint = SocketAddr::from(([127, 0, 0, 1], 3000));
        let pool = RoundRobinPool::new([endpoint]).expect("unchecked pool");

        assert_eq!(
            pool.select().map(|lease| lease.endpoint().clone()),
            Some(RuntimeEndpoint::from(endpoint))
        );
        assert_eq!(
            pool.health_snapshot().endpoints[0].state,
            EndpointHealthState::Unchecked
        );
    }

    #[test]
    fn excluding_every_available_endpoint_does_not_count_pool_unavailability() {
        let endpoints = [
            SocketAddr::from(([127, 0, 0, 1], 3000)),
            SocketAddr::from(([127, 0, 0, 1], 3001)),
        ];
        let pool = RoundRobinPool::new(endpoints).expect("unchecked pool");
        let endpoints = endpoints.map(RuntimeEndpoint::from);

        assert!(pool.select_excluding(&endpoints).is_none());
        assert_eq!(pool.health_snapshot().unavailable_selections, 0);
    }

    #[test]
    fn selection_retries_across_a_pool_health_transition() {
        let endpoints = [
            SocketAddr::from(([127, 0, 0, 1], 3000)),
            SocketAddr::from(([127, 0, 0, 1], 3001)),
        ];
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "checked".into(),
                endpoints.map(RuntimeEndpoint::from),
                UpstreamAlgorithm::RoundRobin,
                true,
            )
            .expect("checked pool"),
        );
        pool.endpoints[0]
            .state
            .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
        pool.endpoints[1]
            .state
            .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
        let writer = pool.health_writer.lock().expect("health writer");
        pool.health_version.store(1, Ordering::Release);
        let barrier = Arc::new(Barrier::new(2));
        let (selection_tx, selection_rx) = mpsc::channel();
        let selection_pool = Arc::clone(&pool);
        let selection_barrier = Arc::clone(&barrier);
        let selection_task = thread::spawn(move || {
            selection_barrier.wait();
            selection_tx
                .send(
                    selection_pool
                        .select()
                        .map(|lease| lease.endpoint().clone()),
                )
                .expect("selection receiver");
        });
        barrier.wait();
        assert!(
            selection_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );

        pool.endpoints[0]
            .state
            .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
        pool.endpoints[1]
            .state
            .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
        pool.health_version.store(2, Ordering::Release);
        drop(writer);

        assert_eq!(
            selection_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("stable selection"),
            Some(RuntimeEndpoint::from(endpoints[0]))
        );
        selection_task.join().expect("selection task");
        assert_eq!(pool.health_snapshot().unavailable_selections, 0);
    }

    #[test]
    fn health_aware_selection_is_fair_across_available_endpoints() {
        let endpoints = [
            SocketAddr::from(([127, 0, 0, 1], 3000)),
            SocketAddr::from(([127, 0, 0, 1], 3001)),
            SocketAddr::from(([127, 0, 0, 1], 3002)),
        ];
        let pool = RoundRobinPool::new_named(
            "checked".into(),
            endpoints.map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool");
        pool.record_health(
            0,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(100),
            1,
            1,
        );
        pool.record_health(1, true, None, Some(100), 1, 1);
        pool.record_health(2, true, None, Some(100), 1, 1);

        let selected = (0..6)
            .map(|_| pool.select().map(|lease| lease.endpoint().clone()))
            .collect::<Vec<_>>();
        let endpoints = endpoints.map(RuntimeEndpoint::from);
        assert_eq!(
            selected,
            vec![
                Some(endpoints[1].clone()),
                Some(endpoints[2].clone()),
                Some(endpoints[1].clone()),
                Some(endpoints[2].clone()),
                Some(endpoints[1].clone()),
                Some(endpoints[2].clone()),
            ]
        );
    }

    #[test]
    fn concurrent_health_aware_selection_distributes_every_available_turn() {
        const THREADS: usize = 8;
        const SELECTIONS_PER_THREAD: usize = 250;

        let endpoints = [
            SocketAddr::from(([127, 0, 0, 1], 3000)),
            SocketAddr::from(([127, 0, 0, 1], 3001)),
            SocketAddr::from(([127, 0, 0, 1], 3002)),
        ];
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "checked".into(),
                endpoints.map(RuntimeEndpoint::from),
                UpstreamAlgorithm::RoundRobin,
                true,
            )
            .expect("checked pool"),
        );
        pool.record_health(
            0,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(100),
            1,
            1,
        );
        pool.record_health(1, true, None, Some(100), 1, 1);
        pool.record_health(2, true, None, Some(100), 1, 1);

        let selected = (0..THREADS)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    (0..SELECTIONS_PER_THREAD)
                        .map(|_| {
                            pool.select()
                                .expect("available endpoint")
                                .endpoint()
                                .clone()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|task| task.join().expect("selection thread"))
            .collect::<Vec<_>>();

        let endpoints = endpoints.map(RuntimeEndpoint::from);
        assert!(!selected.contains(&endpoints[0]));
        for endpoint in &endpoints[1..] {
            assert_eq!(
                selected
                    .iter()
                    .filter(|selected| *selected == endpoint)
                    .count(),
                THREADS * SELECTIONS_PER_THREAD / 2
            );
        }
    }

    #[test]
    fn health_counters_serialize_without_losing_u64_precision() {
        let pool = RoundRobinPool::new_named(
            "checked".into(),
            [RuntimeEndpoint::from(SocketAddr::from((
                [127, 0, 0, 1],
                3000,
            )))],
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool");
        pool.unavailable_selections
            .store(u64::MAX, Ordering::Relaxed);
        pool.endpoints[0]
            .active_work
            .store(u64::MAX, Ordering::Relaxed);
        pool.endpoints[0]
            .successful_checks
            .store(u64::MAX, Ordering::Relaxed);
        pool.endpoints[0]
            .failed_checks
            .store(u64::MAX, Ordering::Relaxed);
        pool.endpoints[0]
            .consecutive_successes
            .store(u64::MAX, Ordering::Relaxed);
        pool.endpoints[0]
            .consecutive_failures
            .store(u64::MAX, Ordering::Relaxed);

        let json = serde_json::to_value(pool.health_snapshot()).expect("health snapshot JSON");
        let exact = u64::MAX.to_string();
        assert_eq!(json["unavailableSelections"], exact);
        assert_eq!(json["endpoints"][0]["activeConnections"], exact);
        assert_eq!(json["endpoints"][0]["successfulChecks"], exact);
        assert_eq!(json["endpoints"][0]["failedChecks"], exact);
        assert_eq!(json["endpoints"][0]["consecutiveSuccesses"], exact);
        assert_eq!(json["endpoints"][0]["consecutiveFailures"], exact);
    }

    #[test]
    fn health_snapshot_waits_for_a_complete_observation() {
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "checked".into(),
                [RuntimeEndpoint::from(SocketAddr::from((
                    [127, 0, 0, 1],
                    3000,
                )))],
                UpstreamAlgorithm::RoundRobin,
                true,
            )
            .expect("checked pool"),
        );
        let writer = pool.health_writer.lock().expect("health writer");
        pool.health_version.store(1, Ordering::Release);
        let barrier = Arc::new(Barrier::new(2));
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let snapshot_pool = Arc::clone(&pool);
        let snapshot_barrier = Arc::clone(&barrier);
        let snapshot_task = thread::spawn(move || {
            snapshot_barrier.wait();
            snapshot_tx
                .send(snapshot_pool.health_snapshot())
                .expect("snapshot receiver");
        });
        barrier.wait();
        assert!(snapshot_rx.recv_timeout(Duration::from_millis(20)).is_err());

        pool.endpoints[0]
            .state
            .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
        pool.endpoints[0]
            .last_checked_at_unix_ms
            .store(100, Ordering::Relaxed);
        pool.endpoints[0]
            .last_transition_at_unix_ms
            .store(100, Ordering::Relaxed);
        pool.endpoints[0]
            .successful_checks
            .store(1, Ordering::Relaxed);
        pool.health_version.store(2, Ordering::Release);
        drop(writer);

        let snapshot = snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("complete snapshot");
        snapshot_task.join().expect("snapshot task");
        assert_eq!(snapshot.endpoints[0].state, EndpointHealthState::Healthy);
        assert_eq!(snapshot.endpoints[0].last_checked_at_unix_ms, Some(100));
        assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(100));
        assert_eq!(snapshot.endpoints[0].successful_checks, 1);
    }
}
