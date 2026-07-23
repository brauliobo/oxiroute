use std::{
    error::Error,
    fmt, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering, fence},
    },
};

use http::{
    Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, UpstreamAlgorithm, UpstreamEndpoint, canonicalize_http_path,
};
use serde::{Serialize, Serializer};

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostMatcher {
    Any,
    ExactAuthority(String),
    NormalizedExact(String),
    Wildcard(String),
}

impl HostMatcher {
    fn rank_if_matches(&self, authority: Option<&Authority>) -> Option<u8> {
        match self {
            Self::Any => Some(0),
            Self::ExactAuthority(expected) => authority
                .is_some_and(|authority| authority.as_str() == expected)
                .then_some(3),
            Self::NormalizedExact(expected) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| host == *expected)
                .then_some(2),
            Self::Wildcard(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| wildcard_matches(&host, suffix))
                .then_some(1),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathMatcher {
    RawPrefix(String),
    SegmentPrefix(String),
    Exact(String),
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
        };
        matches.then_some((rank, value.len()))
    }

    fn value(&self) -> &str {
        match self {
            Self::RawPrefix(value) | Self::SegmentPrefix(value) | Self::Exact(value) => value,
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
        | HttpPathSelector::Exact { value } => value,
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

fn normalized_authority_host(authority: &Authority) -> Option<String> {
    let host = authority.host();
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

    pub(crate) async fn resolve(&self) -> io::Result<Vec<SocketAddr>> {
        match self {
            Self::Socket { address } => Ok(vec![*address]),
            Self::Dns { host, port } => {
                let mut addresses = tokio::net::lookup_host((host.as_str(), *port))
                    .await?
                    .collect::<Vec<_>>();
                addresses.sort_unstable();
                addresses.dedup();
                if addresses.is_empty() {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("DNS endpoint `{host}:{port}` returned no addresses"),
                    ))
                } else {
                    Ok(addresses)
                }
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

    const fn from_u8(value: u8) -> Self {
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

#[derive(Debug)]
struct PoolEndpoint {
    active_leases: Arc<AtomicU64>,
    endpoint: RuntimeEndpoint,
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
    fn new(endpoint: RuntimeEndpoint, checked: bool) -> Self {
        Self {
            active_leases: Arc::new(AtomicU64::new(0)),
            endpoint,
            state: AtomicU8::new(u8::from(checked)),
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
            active_leases: self.active_leases.load(Ordering::Relaxed),
            address: self.endpoint.clone(),
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
    #[serde(serialize_with = "serialize_u64_string")]
    pub active_leases: u64,
    pub address: RuntimeEndpoint,
    pub state: EndpointHealthState,
    pub last_checked_at_unix_ms: Option<u64>,
    pub last_transition_at_unix_ms: Option<u64>,
    #[serde(serialize_with = "serialize_u64_string")]
    pub successful_checks: u64,
    #[serde(serialize_with = "serialize_u64_string")]
    pub failed_checks: u64,
    #[serde(serialize_with = "serialize_u64_string")]
    pub consecutive_successes: u64,
    #[serde(serialize_with = "serialize_u64_string")]
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
    #[serde(serialize_with = "serialize_u64_string")]
    pub unavailable_selections: u64,
    pub endpoints: Vec<EndpointHealthSnapshot>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` callback receives `&T`.
fn serialize_u64_string<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

/// A health-aware selector over a fixed, nonempty endpoint list.
#[derive(Debug)]
pub struct EndpointPool {
    algorithm: UpstreamAlgorithm,
    name: String,
    endpoints: Box<[PoolEndpoint]>,
    health_version: AtomicU64,
    health_writer: Mutex<()>,
    selection: Arc<Mutex<SelectionState>>,
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
    active_leases: Arc<AtomicU64>,
    endpoint: RuntimeEndpoint,
    selection: Arc<Mutex<SelectionState>>,
}

impl EndpointLease {
    #[must_use]
    pub const fn endpoint(&self) -> &RuntimeEndpoint {
        &self.endpoint
    }
}

impl Drop for EndpointLease {
    fn drop(&mut self) {
        let _selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let released =
            self.active_leases
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |active| {
                    active.checked_sub(1)
                });
        debug_assert!(released.is_ok(), "endpoint lease counter underflow");
    }
}

impl EndpointPool {
    /// Creates a pool whose first selection returns the first endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided.
    pub fn new(endpoints: impl IntoIterator<Item = SocketAddr>) -> Result<Self, PoolError> {
        Self::from_endpoints(
            endpoints.into_iter().map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
        )
    }

    /// Creates a pool with the configured balancing algorithm.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the endpoint list is empty or an endpoint cannot be represented
    /// by the runtime without connecting to it.
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
        let endpoints = endpoints
            .into_iter()
            .map(|endpoint| {
                endpoint.preflight()?;
                Ok(PoolEndpoint::new(endpoint, checked))
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
            selection: Arc::new(Mutex::new(SelectionState::default())),
            unavailable_selections: AtomicU64::new(0),
        })
    }

    #[must_use]
    pub fn select(&self) -> Option<EndpointLease> {
        self.select_excluding(&[])
    }

    /// Selects the next endpoint not present in a request's attempted-endpoint set.
    #[must_use]
    pub fn select_excluding(&self, excluded: &[RuntimeEndpoint]) -> Option<EndpointLease> {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = selection.next;
        let (selected, pool_available) = self.read_health(|endpoints| {
            let pool_available = endpoints
                .iter()
                .any(|endpoint| endpoint.state().selectable());
            let candidates = (0..endpoints.len()).filter_map(|offset| {
                let index = (start + offset) % endpoints.len();
                let endpoint = &endpoints[index];
                (endpoint.state().selectable()
                    && !excluded.contains(&endpoint.endpoint)
                    && endpoint.active_leases.load(Ordering::Relaxed) < u64::MAX)
                    .then_some((index, endpoint.active_leases.load(Ordering::Relaxed)))
            });
            let selected = match self.algorithm {
                UpstreamAlgorithm::RoundRobin => candidates.map(|(index, _)| index).next(),
                UpstreamAlgorithm::LeastConnections => candidates
                    .fold(None, |best, candidate| match best {
                        Some((_, best_active)) if best_active <= candidate.1 => best,
                        _ => Some(candidate),
                    })
                    .map(|(index, _)| index),
            };
            (selected, pool_available)
        });
        let Some(index) = selected else {
            if !pool_available {
                self.note_unavailable_selection();
            }
            return None;
        };
        let endpoint = &self.endpoints[index];
        endpoint
            .active_leases
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |active| {
                active.checked_add(1)
            })
            .ok()?;
        selection.next = (index + 1) % self.endpoints.len();
        Some(EndpointLease {
            active_leases: Arc::clone(&endpoint.active_leases),
            endpoint: endpoint.endpoint.clone(),
            selection: Arc::clone(&self.selection),
        })
    }

    #[must_use]
    pub fn algorithm(&self) -> UpstreamAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        self.read_health(|endpoints| {
            endpoints.iter().any(|endpoint| {
                endpoint.state().selectable() && !attempted.contains(&endpoint.endpoint)
            })
        })
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|endpoint| endpoint.state().selectable())
        })
    }

    pub(crate) fn note_unavailable_selection(&self) {
        self.unavailable_selections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn endpoints(&self) -> impl Iterator<Item = (usize, RuntimeEndpoint)> + '_ {
        self.endpoints
            .iter()
            .enumerate()
            .map(|(index, endpoint)| (index, endpoint.endpoint.clone()))
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
        let transition = self.endpoints.get(index).and_then(|endpoint| {
            endpoint.record(
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
        });
        self.health_version.fetch_add(1, Ordering::Release);
        transition
    }

    #[must_use]
    pub fn health_snapshot(&self) -> PoolHealthSnapshot {
        let endpoints = self.read_health(|endpoints| {
            endpoints
                .iter()
                .map(PoolEndpoint::snapshot)
                .collect::<Vec<_>>()
        });
        PoolHealthSnapshot {
            name: self.name.clone(),
            algorithm: algorithm_name(self.algorithm),
            available_endpoints: endpoints
                .iter()
                .filter(|endpoint| endpoint.state.selectable())
                .count(),
            total_endpoints: endpoints.len(),
            unavailable_selections: self.unavailable_selections.load(Ordering::Relaxed),
            endpoints,
        }
    }

    fn read_health<T>(&self, read: impl Fn(&[PoolEndpoint]) -> T) -> T {
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

const fn algorithm_name(algorithm: UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
    }
}

/// Errors produced while constructing an endpoint pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolError {
    Empty,
    InvalidSocketEndpoint(SocketAddr),
    InvalidDnsEndpoint { host: String, port: u16 },
    InvalidUnixEndpoint(PathBuf),
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("upstream endpoint pool cannot be empty"),
            Self::InvalidSocketEndpoint(address) => {
                write!(formatter, "invalid socket endpoint `{address}`")
            }
            Self::InvalidDnsEndpoint { host, port } => {
                write!(formatter, "invalid DNS endpoint `{host}:{port}`")
            }
            Self::InvalidUnixEndpoint(path) => {
                write!(formatter, "invalid Unix endpoint `{}`", path.display())
            }
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

    use super::*;

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
            .active_leases
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
        assert_eq!(json["endpoints"][0]["activeLeases"], exact);
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
