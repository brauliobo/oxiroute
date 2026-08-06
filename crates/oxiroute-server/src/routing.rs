use std::{
    collections::VecDeque,
    error::Error,
    fmt, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{fence, AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, SystemTime},
};

use http::{
    uri::{Authority, PathAndQuery},
    Method, Uri,
};
use oxiroute_config::{
    canonicalize_http_path, HealthStartup, HttpHostSelector, HttpPathSelector, UpstreamAlgorithm,
    PassiveHealthPolicy as ConfigPassiveHealthPolicy, PassiveObserve, PassiveOnError,
    UpstreamEndpoint,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    sync::Notify,
    time::{timeout_at, Instant},
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
                    || path.strip_prefix(value).is_some_and(|remainder| {
                        value.ends_with('/') || remainder.starts_with('/')
                    }),
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
const MAX_WEIGHTED_CYCLE: usize = 25_600;

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

const DEFAULT_PASSIVE_FAILURE_THRESHOLD: u16 = 3;
const DEFAULT_PASSIVE_EJECTION_DURATION: Duration = Duration::from_secs(30);
const DEFAULT_PASSIVE_MAX_EJECTION_DURATION: Duration = Duration::from_secs(300);
const MAX_PASSIVE_FAILURE_THRESHOLD: u16 = 100;
const MAX_PASSIVE_EJECTION_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

/// Bounded policy for passively ejecting endpoints after attributed upstream failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassiveFailurePolicy {
    pub consecutive_failure_threshold: u16,
    pub initial_ejection_duration: Duration,
    pub max_ejection_duration: Duration,
    pub observe: PassiveObserve,
    pub on_error: PassiveOnError,
    pub mark_down: bool,
    pub mark_up: bool,
    pub recovery_threshold: u16,
}

impl PassiveFailurePolicy {
    #[must_use]
    pub const fn new(
        consecutive_failure_threshold: u16,
        initial_ejection_duration: Duration,
        max_ejection_duration: Duration,
    ) -> Self {
        Self {
            consecutive_failure_threshold,
            initial_ejection_duration,
            max_ejection_duration,
            observe: PassiveObserve::Layer7,
            on_error: PassiveOnError::Count,
            mark_down: false,
            mark_up: false,
            recovery_threshold: 1,
        }
    }

    #[must_use]
    pub(crate) fn from_config(policy: &ConfigPassiveHealthPolicy) -> Self {
        Self {
            consecutive_failure_threshold: match policy.on_error {
                PassiveOnError::Count | PassiveOnError::MarkDown => policy.error_limit,
                PassiveOnError::Immediately => 1,
            },
            initial_ejection_duration: Duration::from_millis(policy.initial_backoff_ms),
            max_ejection_duration: Duration::from_millis(policy.max_backoff_ms),
            observe: policy.observe,
            on_error: policy.on_error,
            mark_down: policy.mark_down,
            mark_up: policy.mark_up,
            recovery_threshold: policy.recovery_threshold,
        }
    }

    fn observes(self, failure: HealthFailure) -> bool {
        match self.observe {
            PassiveObserve::Layer4 => {
                matches!(failure, HealthFailure::ConnectFailed | HealthFailure::Timeout)
            }
            PassiveObserve::Layer7 => true,
        }
    }

    fn marks_down(self) -> bool {
        self.mark_down || matches!(self.on_error, PassiveOnError::MarkDown)
    }

    fn validate(self) -> Result<(), PoolError> {
        if self.consecutive_failure_threshold == 0
            || self.consecutive_failure_threshold > MAX_PASSIVE_FAILURE_THRESHOLD
        {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "consecutive failure threshold must be between 1 and 100",
            });
        }
        if self.initial_ejection_duration.is_zero() {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "initial ejection duration must be nonzero",
            });
        }
        if self.initial_ejection_duration > self.max_ejection_duration {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "maximum ejection duration must not be shorter than the initial duration",
            });
        }
        if self.max_ejection_duration > MAX_PASSIVE_EJECTION_DURATION {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "maximum ejection duration must not exceed 24 hours",
            });
        }
        if self.recovery_threshold == 0 || self.recovery_threshold > MAX_PASSIVE_FAILURE_THRESHOLD {
            return Err(PoolError::InvalidPassivePolicy {
                detail: "recovery threshold must be between 1 and 100",
            });
        }
        Ok(())
    }

    fn ejection_duration_for(self, backoff_step: u64) -> Duration {
        let exponent = backoff_step.saturating_sub(1).min(31);
        let multiplier = 1_u32 << exponent;
        self.initial_ejection_duration
            .checked_mul(multiplier)
            .map_or(self.max_ejection_duration, |duration| {
                duration.min(self.max_ejection_duration)
            })
    }
}

impl Default for PassiveFailurePolicy {
    fn default() -> Self {
        Self::new(
            DEFAULT_PASSIVE_FAILURE_THRESHOLD,
            DEFAULT_PASSIVE_EJECTION_DURATION,
            DEFAULT_PASSIVE_MAX_EJECTION_DURATION,
        )
    }
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
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ConnectFailed => "connect_failed",
            Self::UnexpectedStatus => "unexpected_status",
            Self::ProtocolError => "protocol_error",
        }
    }

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
    weight: u16,
    last_checked_at_unix_ms: AtomicU64,
    last_transition_at_unix_ms: AtomicU64,
    successful_checks: AtomicU64,
    failed_checks: AtomicU64,
    consecutive_successes: AtomicU64,
    consecutive_failures: AtomicU64,
    last_failure: AtomicU8,
    passive_ejected: AtomicBool,
    passive_failure_count: AtomicU64,
    passive_consecutive_failures: AtomicU64,
    passive_ejection_count: AtomicU64,
    passive_backoff_step: AtomicU64,
    passive_last_failure: AtomicU8,
    passive_ejected_at_unix_ms: AtomicU64,
    passive_ejection_until_unix_ms: AtomicU64,
    passive_last_recovery_at_unix_ms: AtomicU64,
    passive_recovery_count: AtomicU64,
}

impl PoolEndpoint {
    fn new(server: RuntimeServer, startup: Option<HealthStartup>, weight: u16) -> Self {
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
            weight,
            last_checked_at_unix_ms: AtomicU64::new(0),
            last_transition_at_unix_ms: AtomicU64::new(0),
            successful_checks: AtomicU64::new(0),
            failed_checks: AtomicU64::new(0),
            consecutive_successes: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_failure: AtomicU8::new(0),
            passive_ejected: AtomicBool::new(false),
            passive_failure_count: AtomicU64::new(0),
            passive_consecutive_failures: AtomicU64::new(0),
            passive_ejection_count: AtomicU64::new(0),
            passive_backoff_step: AtomicU64::new(0),
            passive_last_failure: AtomicU8::new(0),
            passive_ejected_at_unix_ms: AtomicU64::new(0),
            passive_ejection_until_unix_ms: AtomicU64::new(0),
            passive_last_recovery_at_unix_ms: AtomicU64::new(0),
            passive_recovery_count: AtomicU64::new(0),
        }
    }

    fn state(&self) -> EndpointHealthState {
        EndpointHealthState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn selectable_at(&self, now_unix_ms: u64) -> bool {
        self.administrative_state() == AdministrativeState::Ready
            && !self.passively_ejected_at(now_unix_ms)
            && match self.health_override() {
                HealthOverride::Auto => self.state().selectable(),
                HealthOverride::Up => true,
                HealthOverride::Down => false,
            }
    }

    fn passively_ejected_at(&self, now_unix_ms: u64) -> bool {
        self.passive_ejected.load(Ordering::Acquire)
            && self.passive_ejection_until_unix_ms.load(Ordering::Acquire) > now_unix_ms
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

    fn try_acquire(
        self: &Arc<Self>,
        queue: &Arc<PoolQueue>,
        health: &Arc<PoolHealthState>,
        now_unix_ms: u64,
    ) -> Option<EndpointLease> {
        if !self.selectable_at(now_unix_ms) {
            return None;
        }
        self.try_acquire_capacity(now_unix_ms)?;
        if !self.selectable_at(now_unix_ms) {
            self.release_capacity(queue);
            return None;
        }
        Some(EndpointLease::acquired(
            Arc::clone(self),
            Arc::clone(queue),
            Arc::clone(health),
        ))
    }

    fn try_acquire_capacity(&self, now_unix_ms: u64) -> Option<()> {
        self.active_work
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if !self.selectable_at(now_unix_ms)
                    || self.max_connections().is_some_and(|limit| active >= limit)
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
        passive_policy: PassiveFailurePolicy,
    ) -> HealthUpdate {
        if let Some(at_unix_ms) = at_unix_ms {
            self.last_checked_at_unix_ms
                .store(at_unix_ms, Ordering::Relaxed);
        }
        let previous = self.state();
        let (next, consecutive) = if healthy {
            self.successful_checks.fetch_add(1, Ordering::Relaxed);
            let consecutive = self
                .consecutive_successes
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.last_failure.store(0, Ordering::Relaxed);
            let next = if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Unhealthy
            ) && consecutive >= u64::from(healthy_threshold)
            {
                EndpointHealthState::Healthy
            } else {
                previous
            };
            (next, consecutive)
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
            let next = if matches!(
                previous,
                EndpointHealthState::Unknown | EndpointHealthState::Healthy
            ) && consecutive >= u64::from(unhealthy_threshold)
            {
                EndpointHealthState::Unhealthy
            } else {
                previous
            };
            (next, consecutive)
        };
        let transition = if next == previous {
            None
        } else {
            self.state.store(next as u8, Ordering::Release);
            if let Some(at_unix_ms) = at_unix_ms {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
            Some((previous, next))
        };
        let passive_recovery = healthy
            .then_some(consecutive)
            .filter(|consecutive| {
                *consecutive >= u64::from(healthy_threshold.max(passive_policy.recovery_threshold))
            })
            .and_then(|_| {
                self.recover_passive(at_unix_ms.unwrap_or_else(now_unix_ms), passive_policy)
            });
        HealthUpdate {
            transition,
            passive_recovery,
        }
    }

    fn record_passive_failure(
        &self,
        failure: HealthFailure,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
    ) -> Option<PassiveEjection> {
        if !policy.observes(failure) {
            return None;
        }
        let failure_count = increment_saturating(&self.passive_failure_count);
        let consecutive = increment_saturating(&self.passive_consecutive_failures);
        self.passive_last_failure
            .store(failure as u8, Ordering::Relaxed);
        if self.passively_ejected_at(at_unix_ms)
            || consecutive < u64::from(policy.consecutive_failure_threshold)
        {
            return None;
        }

        let ejection_count = increment_saturating(&self.passive_ejection_count);
        let backoff_step = increment_saturating(&self.passive_backoff_step);
        let duration = policy.ejection_duration_for(backoff_step);
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let ejection_until_unix_ms = at_unix_ms.saturating_add(duration_ms);
        self.passive_ejected.store(true, Ordering::Release);
        self.passive_consecutive_failures
            .store(0, Ordering::Relaxed);
        self.passive_ejected_at_unix_ms
            .store(at_unix_ms, Ordering::Relaxed);
        self.passive_ejection_until_unix_ms
            .store(ejection_until_unix_ms, Ordering::Release);
        if policy.marks_down() {
            let previous = EndpointHealthState::from_u8(
                self.state
                    .swap(EndpointHealthState::Unhealthy as u8, Ordering::AcqRel),
            );
            if previous != EndpointHealthState::Unhealthy {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
        }
        Some(PassiveEjection {
            reason: failure,
            failure_count,
            ejection_count,
            ejected_at_unix_ms: at_unix_ms,
            ejection_until_unix_ms,
        })
    }

    fn recover_passive(
        &self,
        at_unix_ms: u64,
        policy: PassiveFailurePolicy,
    ) -> Option<PassiveRecovery> {
        if !self.passive_ejected.swap(false, Ordering::AcqRel) {
            return None;
        }
        let recovery_count = increment_saturating(&self.passive_recovery_count);
        self.passive_consecutive_failures
            .store(0, Ordering::Relaxed);
        self.passive_backoff_step.store(0, Ordering::Relaxed);
        self.passive_last_recovery_at_unix_ms
            .store(at_unix_ms, Ordering::Relaxed);
        if policy.mark_up {
            let previous = EndpointHealthState::from_u8(
                self.state
                    .swap(EndpointHealthState::Healthy as u8, Ordering::AcqRel),
            );
            if previous != EndpointHealthState::Healthy {
                self.last_transition_at_unix_ms
                    .store(at_unix_ms, Ordering::Relaxed);
            }
        }
        Some(PassiveRecovery {
            reason: HealthFailure::from_u8(self.passive_last_failure.load(Ordering::Relaxed)),
            recovery_count,
            recovered_at_unix_ms: at_unix_ms,
        })
    }

    fn snapshot(&self, now_unix_ms: u64) -> EndpointHealthSnapshot {
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
            weight: self.weight,
            last_checked_at_unix_ms: nonzero(self.last_checked_at_unix_ms.load(Ordering::Relaxed)),
            last_transition_at_unix_ms: nonzero(
                self.last_transition_at_unix_ms.load(Ordering::Relaxed),
            ),
            successful_checks: self.successful_checks.load(Ordering::Relaxed),
            failed_checks: self.failed_checks.load(Ordering::Relaxed),
            consecutive_successes: self.consecutive_successes.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            last_failure: HealthFailure::from_u8(self.last_failure.load(Ordering::Relaxed)),
            passive_ejected: self.passively_ejected_at(now_unix_ms),
            passive_failure_count: self.passive_failure_count.load(Ordering::Relaxed),
            passive_consecutive_failures: self.passive_consecutive_failures.load(Ordering::Relaxed),
            passive_ejection_count: self.passive_ejection_count.load(Ordering::Relaxed),
            passive_ejection_reason: HealthFailure::from_u8(
                self.passive_last_failure.load(Ordering::Relaxed),
            ),
            passive_ejected_at_unix_ms: nonzero(
                self.passive_ejected_at_unix_ms.load(Ordering::Relaxed),
            ),
            passive_ejection_until_unix_ms: nonzero(
                self.passive_ejection_until_unix_ms.load(Ordering::Relaxed),
            ),
            passive_recovery_count: self.passive_recovery_count.load(Ordering::Relaxed),
            passive_last_recovery_at_unix_ms: nonzero(
                self.passive_last_recovery_at_unix_ms
                    .load(Ordering::Relaxed),
            ),
        }
    }
}

#[derive(Debug)]
struct HealthUpdate {
    transition: Option<(EndpointHealthState, EndpointHealthState)>,
    passive_recovery: Option<PassiveRecovery>,
}

#[derive(Clone, Copy, Debug)]
struct PassiveEjection {
    reason: HealthFailure,
    failure_count: u64,
    ejection_count: u64,
    ejected_at_unix_ms: u64,
    ejection_until_unix_ms: u64,
}

#[derive(Clone, Copy, Debug)]
struct PassiveRecovery {
    reason: Option<HealthFailure>,
    recovery_count: u64,
    recovered_at_unix_ms: u64,
}

const fn nonzero(value: u64) -> Option<u64> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

fn increment_saturating(counter: &AtomicU64) -> u64 {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        })
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
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
    pub weight: u16,
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
    pub passive_ejected: bool,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_failure_count: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_consecutive_failures: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_ejection_count: u64,
    pub passive_ejection_reason: Option<HealthFailure>,
    pub passive_ejected_at_unix_ms: Option<u64>,
    pub passive_ejection_until_unix_ms: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub passive_recovery_count: u64,
    pub passive_last_recovery_at_unix_ms: Option<u64>,
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
    waiters: Mutex<VecDeque<Arc<PoolWaiter>>>,
}

#[derive(Debug)]
struct PoolWaiter {
    notify: Notify,
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
            waiters: Mutex::new(VecDeque::new()),
        }
    }

    fn notify_capacity_waiters(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        #[cfg(test)]
        self.notifications.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_waiters();
        self.notify_front();
    }

    fn notify_front(&self) {
        let waiter = self
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .front()
            .cloned();
        if let Some(waiter) = waiter {
            waiter.notify.notify_one();
        }
    }
}

#[derive(Debug)]
struct PoolHealthState {
    health_version: AtomicU64,
    health_writer: Mutex<()>,
    passive_policy: PassiveFailurePolicy,
    pool_name: String,
    queue: Arc<PoolQueue>,
}

impl PoolHealthState {
    fn new(pool_name: String, queue: Arc<PoolQueue>, passive_policy: PassiveFailurePolicy) -> Self {
        Self {
            health_version: AtomicU64::new(0),
            health_writer: Mutex::new(()),
            passive_policy,
            pool_name,
            queue,
        }
    }

    fn record_passive_failure(&self, server: &PoolEndpoint, failure: HealthFailure) {
        self.record_passive_failure_at(server, failure, now_unix_ms());
    }

    fn record_passive_failure_at(
        &self,
        server: &PoolEndpoint,
        failure: HealthFailure,
        at_unix_ms: u64,
    ) {
        let ejection = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let ejection = server.record_passive_failure(failure, at_unix_ms, self.passive_policy);
            self.health_version.fetch_add(1, Ordering::Release);
            ejection
        };
        if let Some(ejection) = ejection {
            self.queue.notify_capacity_waiters();
            crate::operational_event::emit_upstream_endpoint_ejection(
                &self.pool_name,
                &server.name,
                ejection.reason,
                ejection.failure_count,
                ejection.ejection_count,
                ejection.ejected_at_unix_ms,
                ejection.ejection_until_unix_ms,
            );
        }
    }

    fn record_health(
        &self,
        server: &PoolEndpoint,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> HealthUpdate {
        let update = {
            let _writer = self
                .health_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.health_version.fetch_add(1, Ordering::AcqRel);
            let update = server.record(
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
                self.passive_policy,
            );
            self.health_version.fetch_add(1, Ordering::Release);
            update
        };
        if update.transition.is_some() || update.passive_recovery.is_some() {
            self.queue.notify_capacity_waiters();
        }
        if let Some(recovery) = update.passive_recovery {
            crate::operational_event::emit_upstream_endpoint_recovery(
                &self.pool_name,
                &server.name,
                recovery.reason,
                recovery.recovery_count,
                recovery.recovered_at_unix_ms,
            );
        }
        update
    }
}

/// A health-aware selector over a fixed, nonempty named-server list.
#[derive(Debug)]
pub struct EndpointPool {
    algorithm: UpstreamAlgorithm,
    name: String,
    endpoints: Box<[Arc<PoolEndpoint>]>,
    weighted_schedule: Box<[usize]>,
    health: Arc<PoolHealthState>,
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

#[derive(Clone)]
pub(crate) struct EndpointObservation {
    health: Arc<PoolHealthState>,
    server: Arc<PoolEndpoint>,
}

impl EndpointObservation {
    pub(crate) fn record_passive_failure(&self, failure: HealthFailure) {
        self.health.record_passive_failure(&self.server, failure);
    }
}

#[derive(Debug)]
struct EndpointLeaseInner {
    acquired: AtomicBool,
    deadline: Option<Instant>,
    health: Arc<PoolHealthState>,
    queue: Arc<PoolQueue>,
    server: Arc<PoolEndpoint>,
}

impl EndpointLease {
    fn acquired(
        server: Arc<PoolEndpoint>,
        queue: Arc<PoolQueue>,
        health: Arc<PoolHealthState>,
    ) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(true),
                deadline: None,
                health,
                queue,
                server,
            }),
        }
    }

    fn pending(
        server: Arc<PoolEndpoint>,
        queue: Arc<PoolQueue>,
        queue_timeout: Option<std::time::Duration>,
        health: Arc<PoolHealthState>,
    ) -> Self {
        Self {
            inner: Arc::new(EndpointLeaseInner {
                acquired: AtomicBool::new(false),
                deadline: queue_timeout.map(|timeout| Instant::now() + timeout),
                health,
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

    pub(crate) fn observation(&self) -> EndpointObservation {
        EndpointObservation {
            health: Arc::clone(&self.inner.health),
            server: Arc::clone(&self.inner.server),
        }
    }

    pub(crate) fn record_passive_failure(&self, failure: HealthFailure) {
        self.inner
            .health
            .record_passive_failure(&self.inner.server, failure);
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
        if self.server.try_acquire_capacity(now_unix_ms()).is_none() {
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
    waiter: Arc<PoolWaiter>,
    waiting: bool,
}

impl QueueWaitGuard {
    fn new(queue: Arc<PoolQueue>) -> Self {
        let waiter = Arc::new(PoolWaiter {
            notify: Notify::new(),
        });
        queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Arc::clone(&waiter));
        Self {
            completed: false,
            queue,
            waiter,
            waiting: false,
        }
    }

    fn is_front(&self) -> bool {
        self.queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .front()
            .is_some_and(|waiter| Arc::ptr_eq(waiter, &self.waiter))
    }

    fn mark_waiting(&mut self) {
        if self.waiting {
            return;
        }
        self.waiting = true;
        self.queue.queued.fetch_add(1, Ordering::Relaxed);
        self.queue.queued_total.fetch_add(1, Ordering::Relaxed);
    }

    fn complete(&mut self) {
        self.finish(false);
    }

    fn timeout(&mut self) {
        self.finish(true);
    }

    fn finish(&mut self, timed_out: bool) {
        let removed_front = self.remove();
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
        }
        if timed_out {
            self.queue.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        self.completed = true;
        if removed_front {
            self.queue.notify_front();
        }
    }

    fn remove(&self) -> bool {
        let mut waiters = self
            .queue
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(waiter, &self.waiter))
        else {
            return false;
        };
        let removed_front = index == 0;
        waiters.remove(index);
        removed_front
    }
}

impl Drop for QueueWaitGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let removed_front = self.remove();
        if self.waiting {
            decrement_queue_count(&self.queue.queued);
            self.queue.cancellations.fetch_add(1, Ordering::Relaxed);
        }
        if removed_front {
            self.queue.notify_front();
        }
    }
}

fn decrement_queue_count(counter: &AtomicU64) {
    let released = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
        queued.checked_sub(1)
    });
    debug_assert!(released.is_ok(), "pool queue counter underflow");
}

struct SelectionAttempt {
    lease: Option<EndpointLease>,
    pool_available: bool,
    saturated: bool,
}

fn effective_weights(
    algorithm: &UpstreamAlgorithm,
    endpoint_count: usize,
) -> Result<Vec<u16>, PoolError> {
    match algorithm {
        UpstreamAlgorithm::WeightedRoundRobin { weights } => {
            if weights.len() != endpoint_count {
                return Err(PoolError::InvalidWeights {
                    detail: "weighted round-robin requires one weight per endpoint",
                });
            }
            if weights.contains(&0) {
                return Err(PoolError::InvalidWeights {
                    detail: "weighted round-robin weights must be positive",
                });
            }
            Ok(weights.clone())
        }
        UpstreamAlgorithm::RoundRobin
        | UpstreamAlgorithm::LeastConnections
        | UpstreamAlgorithm::First => Ok(vec![1; endpoint_count]),
    }
}

fn build_weighted_schedule(
    algorithm: &UpstreamAlgorithm,
    weights: &[u16],
) -> Result<Box<[usize]>, PoolError> {
    if !matches!(algorithm, UpstreamAlgorithm::WeightedRoundRobin { .. }) {
        return Ok(Box::new([]));
    }
    let cycle_length = weights.iter().try_fold(0_usize, |total, weight| {
        total
            .checked_add(usize::from(*weight))
            .filter(|total| *total <= MAX_WEIGHTED_CYCLE)
            .ok_or(PoolError::InvalidWeights {
                detail: "weighted round-robin cycle exceeds its bounded size",
            })
    })?;
    let mut schedule = Vec::with_capacity(cycle_length);
    for (index, weight) in weights.iter().enumerate() {
        schedule.extend(std::iter::repeat_n(index, usize::from(*weight)));
    }
    Ok(schedule.into_boxed_slice())
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

    /// Creates an unchecked pool with an immutable passive failure policy.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints_with_policy(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_with_policy(
            String::new(),
            endpoints
                .into_iter()
                .enumerate()
                .map(|(index, endpoint)| RuntimeServer {
                    name: index.to_string(),
                    endpoint,
                    max_connections: None,
                    pinned_addresses: None,
                    protected_addresses: Arc::from([]),
                }),
            algorithm,
            None,
            None,
            passive_policy,
        )
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
        Self::new_named_servers_with_policy(
            name,
            servers,
            algorithm,
            startup,
            queue_timeout,
            PassiveFailurePolicy::default(),
        )
    }

    pub(crate) fn new_named_servers_with_policy(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        passive_policy.validate()?;
        let servers = servers.into_iter().collect::<Vec<_>>();
        let weights = effective_weights(&algorithm, servers.len())?;
        let weighted_schedule = build_weighted_schedule(&algorithm, &weights)?;
        let endpoints = servers
            .into_iter()
            .zip(weights)
            .map(|server| {
                let (server, weight) = server;
                server.endpoint.preflight()?;
                Ok(Arc::new(PoolEndpoint::new(server, startup, weight)))
            })
            .collect::<Result<Vec<_>, PoolError>>()?
            .into_boxed_slice();
        if endpoints.is_empty() {
            return Err(PoolError::Empty);
        }
        let queue = Arc::new(PoolQueue::new(queue_timeout.is_some()));
        let health = Arc::new(PoolHealthState::new(
            name.clone(),
            Arc::clone(&queue),
            passive_policy,
        ));
        Ok(Self {
            algorithm,
            name,
            endpoints,
            weighted_schedule,
            health,
            selection: Mutex::new(SelectionState::default()),
            queue,
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
        let now_unix_ms = now_unix_ms();
        let (candidates, pool_available) = self.read_health(|endpoints| {
            let pool_available = endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms));
            let candidates = if self.weighted_schedule.is_empty() {
                (0..endpoints.len())
                    .filter_map(|offset| {
                        let index = (start + offset) % endpoints.len();
                        let server = &endpoints[index];
                        (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name))
                            .then_some((index, index, server.active_work.load(Ordering::Acquire)))
                    })
                    .collect::<Vec<_>>()
            } else {
                (0..self.weighted_schedule.len())
                    .filter_map(|offset| {
                        let cursor = (start + offset) % self.weighted_schedule.len();
                        let index = self.weighted_schedule[cursor];
                        let server = &endpoints[index];
                        (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name))
                            .then_some((cursor, index, server.active_work.load(Ordering::Acquire)))
                    })
                    .collect::<Vec<_>>()
            };
            (candidates, pool_available)
        });
        let saturated = candidates
            .iter()
            .any(|(_, index, _)| !self.endpoints[*index].has_capacity());
        let mut ordered = candidates
            .iter()
            .filter(|(_, index, _)| self.endpoints[*index].has_capacity())
            .copied()
            .collect::<Vec<_>>();
        match &self.algorithm {
            UpstreamAlgorithm::LeastConnections => {
                ordered.sort_by_key(|(_, _, active)| *active);
            }
            UpstreamAlgorithm::First => ordered.sort_by_key(|(_, index, _)| *index),
            UpstreamAlgorithm::WeightedRoundRobin { .. } | UpstreamAlgorithm::RoundRobin => {}
        }
        let selected = ordered.into_iter().find_map(|(cursor, index, _)| {
            self.endpoints[index]
                .try_acquire(&self.queue, &self.health, now_unix_ms)
                .map(|lease| (lease, cursor, index))
        });
        let lease = selected.map(|(lease, cursor, index)| {
            if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
                selection.next = if self.weighted_schedule.is_empty() {
                    (index + 1) % self.endpoints.len()
                } else {
                    (cursor + 1) % self.weighted_schedule.len()
                };
            }
            lease
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
        let now_unix_ms = now_unix_ms();
        let candidates = self.read_health(|endpoints| {
            if self.weighted_schedule.is_empty() {
                (0..endpoints.len())
                    .filter_map(|offset| {
                        let index = (start + offset) % endpoints.len();
                        let server = &endpoints[index];
                        (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name))
                            .then_some((index, index, server.active_work.load(Ordering::Acquire)))
                    })
                    .collect::<Vec<_>>()
            } else {
                (0..self.weighted_schedule.len())
                    .filter_map(|offset| {
                        let cursor = (start + offset) % self.weighted_schedule.len();
                        let index = self.weighted_schedule[cursor];
                        let server = &endpoints[index];
                        (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name))
                            .then_some((cursor, index, server.active_work.load(Ordering::Acquire)))
                    })
                    .collect::<Vec<_>>()
            }
        });
        let available = candidates
            .iter()
            .copied()
            .filter(|(_, index, _)| self.endpoints[*index].has_capacity())
            .collect::<Vec<_>>();
        let candidates = if available.is_empty() {
            &candidates
        } else {
            &available
        };
        let selected = match &self.algorithm {
            UpstreamAlgorithm::WeightedRoundRobin { .. } | UpstreamAlgorithm::RoundRobin => {
                candidates
                    .first()
                    .map(|(cursor, index, _)| (*cursor, *index))
            }
            UpstreamAlgorithm::LeastConnections => candidates
                .iter()
                .min_by_key(|(_, _, active)| *active)
                .map(|(cursor, index, _)| (*cursor, *index)),
            UpstreamAlgorithm::First => candidates
                .iter()
                .min_by_key(|(_, index, _)| *index)
                .map(|(cursor, index, _)| (*cursor, *index)),
        };
        let Some((cursor, index)) = selected else {
            self.note_unavailable_selection();
            return None;
        };
        if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
            selection.next = if self.weighted_schedule.is_empty() {
                (index + 1) % self.endpoints.len()
            } else {
                (cursor + 1) % self.weighted_schedule.len()
            };
        }
        Some(EndpointLease::pending(
            Arc::clone(&self.endpoints[index]),
            Arc::clone(&self.queue),
            self.queue_timeout,
            Arc::clone(&self.health),
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
        self.select_wait_with(|| self.select_server_excluding(excluded))
            .await
    }

    pub(crate) async fn select_server_wait(&self, name: &str) -> Option<EndpointLease> {
        self.select_wait_with(|| self.select_named_server(name))
            .await
    }

    async fn select_wait_with(
        &self,
        mut select: impl FnMut() -> SelectionAttempt,
    ) -> Option<EndpointLease> {
        let Some(queue_timeout) = self.queue_timeout else {
            let attempt = select();
            if attempt.lease.is_none() && !attempt.pool_available {
                self.note_unavailable_selection();
            }
            return attempt.lease;
        };
        let deadline = Instant::now() + queue_timeout;
        let mut waiting = QueueWaitGuard::new(Arc::clone(&self.queue));
        loop {
            let waiter = Arc::clone(&waiting.waiter);
            let notified = waiter.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if waiting.is_front() {
                let attempt = select();
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
            }
            waiting.mark_waiting();
            if timeout_at(deadline, notified).await.is_err() {
                waiting.timeout();
                return None;
            }
        }
    }

    fn select_named_server(&self, name: &str) -> SelectionAttempt {
        let Some(server) = self.endpoints.iter().find(|server| server.name == name) else {
            return SelectionAttempt {
                lease: None,
                pool_available: false,
                saturated: false,
            };
        };
        let now_unix_ms = now_unix_ms();
        let pool_available = server.selectable_at(now_unix_ms);
        let saturated = pool_available && !server.has_capacity();
        SelectionAttempt {
            lease: server.try_acquire(&self.queue, &self.health, now_unix_ms),
            pool_available,
            saturated,
        }
    }

    pub(crate) fn select_server_connection_target(&self, name: &str) -> Option<EndpointLease> {
        let server = self.endpoints.iter().find(|server| server.name == name)?;
        server.selectable_at(now_unix_ms()).then(|| {
            EndpointLease::pending(
                Arc::clone(server),
                Arc::clone(&self.queue),
                self.queue_timeout,
                Arc::clone(&self.health),
            )
        })
    }

    #[must_use]
    pub fn algorithm(&self) -> UpstreamAlgorithm {
        self.algorithm.clone()
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|endpoints| {
            endpoints.iter().any(|server| {
                server.selectable_at(now_unix_ms) && !attempted.contains(&server.endpoint)
            })
        })
    }

    #[must_use]
    pub(crate) fn has_unattempted_servers(&self, attempted: &[String]) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|endpoints| {
            endpoints.iter().any(|server| {
                server.selectable_at(now_unix_ms) && !attempted.contains(&server.name)
            })
        })
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        let now_unix_ms = now_unix_ms();
        self.read_health(|endpoints| {
            endpoints
                .iter()
                .any(|server| server.selectable_at(now_unix_ms) && server.has_capacity())
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
        let update = self.endpoints.get(index).map(|server| {
            self.health.record_health(
                server,
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
        });
        update.and_then(|update| update.transition)
    }

    #[cfg(test)]
    pub(crate) fn record_passive_failure(&self, index: usize, failure: HealthFailure) {
        if let Some(server) = self.endpoints.get(index) {
            self.health.record_passive_failure(server, failure);
        }
    }

    #[cfg(test)]
    fn record_passive_failure_at(&self, index: usize, failure: HealthFailure, at_unix_ms: u64) {
        if let Some(server) = self.endpoints.get(index) {
            self.health
                .record_passive_failure_at(server, failure, at_unix_ms);
        }
    }

    pub(crate) fn health_state(&self, index: usize) -> Option<EndpointHealthState> {
        self.endpoints.get(index).map(|server| server.state())
    }

    pub(crate) fn health_checks_running(&self, index: usize) -> bool {
        self.endpoints
            .get(index)
            .is_some_and(|server| server.checks_running())
    }

    pub(crate) fn passive_ejected(&self, index: usize) -> bool {
        self.endpoints
            .get(index)
            .is_some_and(|server| server.passively_ejected_at(now_unix_ms()))
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
        let now_unix_ms = now_unix_ms();
        let endpoints = self.read_health(|endpoints| {
            endpoints
                .iter()
                .map(|server| server.snapshot(now_unix_ms))
                .collect::<Vec<_>>()
        });
        PoolHealthSnapshot {
            name: self.name.clone(),
            algorithm: algorithm_name(&self.algorithm),
            available_endpoints: endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.administrative_state == AdministrativeState::Ready
                        && !endpoint.passive_ejected
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
            let version = self.health.health_version.load(Ordering::Acquire);
            if version % 2 != 0 {
                std::thread::yield_now();
                continue;
            }
            let value = read(&self.endpoints);
            fence(Ordering::Acquire);
            if self.health.health_version.load(Ordering::Relaxed) == version {
                return value;
            }
        }
        let _writer = self
            .health
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

const fn algorithm_name(algorithm: &UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::WeightedRoundRobin { .. } => "weighted_round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
        UpstreamAlgorithm::First => "first",
    }
}

/// Errors produced while constructing an endpoint pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolError {
    Empty,
    InvalidWeights { detail: &'static str },
    InvalidPassivePolicy { detail: &'static str },
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
            Self::InvalidWeights { detail } => {
                write!(
                    formatter,
                    "invalid weighted round-robin configuration: {detail}"
                )
            }
            Self::InvalidPassivePolicy { detail } => {
                write!(formatter, "invalid passive failure policy: {detail}")
            }
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
        sync::{mpsc, Arc, Barrier},
        thread,
        time::Duration,
    };

    use pingora::{
        connectors::http::Connector as HttpConnector,
        http::RequestHeader,
        protocols::{http::client::HttpSession, ConnectionLifetime as _},
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
        assert!(error
            .to_string()
            .contains("returned more than 16 addresses"));
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

    async fn wait_for_queued(pool: &EndpointPool, expected: u64) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health_snapshot().queued != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued request count");
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
    async fn queue_disabled_selection_ignores_a_transient_fifo_registration() {
        let pool = RoundRobinPool::new_named_servers(
            "immediate".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
        )
        .expect("immediate pool");
        let _concurrent_front = QueueWaitGuard::new(Arc::clone(&pool.queue));

        let acquired = pool
            .select_wait()
            .await
            .expect("queue-disabled selection must attempt available capacity");

        drop(acquired);
        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 0);
    }

    #[tokio::test]
    async fn fifo_queue_sends_the_oldest_waiter_to_whichever_endpoint_frees_first() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "fifo".into(),
                [
                    runtime_server("primary", 3000, Some(1)),
                    runtime_server("secondary", 3001, Some(1)),
                ],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("FIFO pool"),
        );
        let primary = pool.select().expect("primary capacity");
        let secondary = pool.select().expect("secondary capacity");
        let old_pool = Arc::clone(&pool);
        let old = tokio::spawn(async move { old_pool.select_wait().await });
        wait_for_queued(&pool, 1).await;
        let new_pool = Arc::clone(&pool);
        let new = tokio::spawn(async move { new_pool.select_wait().await });
        wait_for_queued(&pool, 2).await;

        drop(secondary);
        let old = old
            .await
            .expect("old waiter task")
            .expect("old waiter lease");
        assert_eq!(old.server_name(), "secondary");
        wait_for_queued(&pool, 1).await;
        drop(primary);
        let new = new
            .await
            .expect("new waiter task")
            .expect("new waiter lease");
        assert_eq!(new.server_name(), "primary");
        drop((old, new));

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 2);
        assert_eq!(snapshot.queue_timeouts, 0);
        assert_eq!(snapshot.queue_cancellations, 0);
    }

    #[tokio::test]
    async fn cancelling_the_fifo_head_advances_the_next_waiter_once() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "fifo-cancel".into(),
                [runtime_server("only", 3000, Some(1))],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(1)),
            )
            .expect("FIFO cancellation pool"),
        );
        let held = pool.select().expect("held capacity");
        let old_pool = Arc::clone(&pool);
        let old = tokio::spawn(async move { old_pool.select_wait().await });
        wait_for_queued(&pool, 1).await;
        let new_pool = Arc::clone(&pool);
        let new = tokio::spawn(async move { new_pool.select_wait().await });
        wait_for_queued(&pool, 2).await;

        old.abort();
        assert!(old.await.expect_err("old waiter cancelled").is_cancelled());
        wait_for_queued(&pool, 1).await;
        drop(held);
        let acquired = new
            .await
            .expect("new waiter task")
            .expect("new waiter lease");
        drop(acquired);

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 2);
        assert_eq!(snapshot.queue_timeouts, 0);
        assert_eq!(snapshot.queue_cancellations, 1);
    }

    #[tokio::test]
    async fn timing_out_the_fifo_head_exposes_free_capacity_to_the_next_waiter() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "fifo-timeout".into(),
                [
                    runtime_server("primary", 3000, Some(1)),
                    runtime_server("secondary", 3001, Some(1)),
                ],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_millis(100)),
            )
            .expect("FIFO timeout pool"),
        );
        let primary = pool.select().expect("primary capacity");
        let old_pool = Arc::clone(&pool);
        let old = tokio::spawn(async move { old_pool.select_server_wait("primary").await });
        wait_for_queued(&pool, 1).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        let new_pool = Arc::clone(&pool);
        let new = tokio::spawn(async move { new_pool.select_wait().await });
        wait_for_queued(&pool, 2).await;

        assert!(old.await.expect("old waiter task").is_none());
        let acquired = new
            .await
            .expect("new waiter task")
            .expect("new waiter lease");
        assert_eq!(acquired.server_name(), "secondary");
        drop((primary, acquired));

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.queued_total, 2);
        assert_eq!(snapshot.queue_timeouts, 1);
        assert_eq!(snapshot.queue_cancellations, 0);
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
        assert!(server
            .await
            .expect_err("H2 server cancelled")
            .is_cancelled());
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
        assert!(hidden_waiter
            .await
            .expect_err("hidden waiter cancelled")
            .is_cancelled());
        wait_for_lifetime_waiters(&pool, 0).await;
        assert_eq!(pool.health_snapshot().queued, 1);
        assert_eq!(pool.health_snapshot().queue_cancellations, 0);

        selector_waiter.abort();
        assert!(selector_waiter
            .await
            .expect_err("selector waiter cancelled")
            .is_cancelled());
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
        let writer = pool.health.health_writer.lock().expect("health writer");
        pool.health.health_version.store(1, Ordering::Release);
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
        assert!(selection_rx
            .recv_timeout(Duration::from_millis(20))
            .is_err());

        pool.endpoints[0]
            .state
            .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
        pool.endpoints[1]
            .state
            .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
        pool.health.health_version.store(2, Ordering::Release);
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
    fn weighted_round_robin_is_deterministic_and_exposes_effective_weights() {
        let pool = RoundRobinPool::new_named_servers(
            "weighted".into(),
            [
                runtime_server("primary", 3000, None),
                runtime_server("backup", 3001, None),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![3, 1],
            },
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("weighted pool");

        let selected = (0..8)
            .map(|_| {
                pool.select()
                    .expect("weighted endpoint")
                    .server_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selected,
            [
                "primary", "primary", "primary", "backup", "primary", "primary", "primary",
                "backup",
            ]
        );

        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.algorithm, "weighted_round_robin");
        assert_eq!(
            snapshot
                .endpoints
                .iter()
                .map(|endpoint| endpoint.weight)
                .collect::<Vec<_>>(),
            vec![3, 1]
        );
    }

    #[test]
    fn weighted_round_robin_skips_unhealthy_endpoints_and_recovers_them() {
        let pool = RoundRobinPool::new_named_servers(
            "weighted-health".into(),
            [
                runtime_server("primary", 3000, None),
                runtime_server("backup", 3001, None),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![3, 1],
            },
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("weighted health pool");
        pool.record_health(
            1,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(100),
            1,
            1,
        );

        let unavailable = (0..4)
            .map(|_| {
                pool.select()
                    .expect("healthy endpoint")
                    .server_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(unavailable, ["primary", "primary", "primary", "primary"]);
        assert_eq!(
            pool.health_snapshot().endpoints[1].state,
            EndpointHealthState::Unhealthy
        );

        pool.record_health(1, true, None, Some(200), 1, 1);
        let recovered = (0..4)
            .map(|_| {
                pool.select()
                    .expect("recovered endpoint")
                    .server_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(recovered, ["primary", "primary", "backup", "primary"]);
        assert_eq!(
            pool.health_snapshot().endpoints[1].state,
            EndpointHealthState::Healthy
        );
    }

    #[test]
    fn passive_ejection_is_deterministic_and_active_health_reentry_preserves_weighting() {
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-weighted".into(),
            [
                runtime_server("primary", 3000, None),
                runtime_server("backup", 3001, None),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![3, 1],
            },
            Some(HealthStartup::Healthy),
            None,
            PassiveFailurePolicy::new(2, Duration::from_secs(60), Duration::from_secs(120)),
        )
        .expect("passive policy pool");

        let base = now_unix_ms();
        pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base);
        pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base + 1);
        let ejected = pool.health_snapshot();
        assert_eq!(ejected.available_endpoints, 1);
        assert!(ejected.endpoints[0].passive_ejected);
        assert_eq!(ejected.endpoints[0].passive_failure_count, 2);
        assert_eq!(ejected.endpoints[0].passive_ejection_count, 1);
        assert_eq!(
            ejected.endpoints[0].passive_ejection_reason,
            Some(HealthFailure::ConnectFailed)
        );
        assert_eq!(
            ejected.endpoints[0].passive_ejected_at_unix_ms,
            Some(base + 1)
        );
        assert_eq!(
            ejected.endpoints[0].passive_ejection_until_unix_ms,
            Some(base + 60_001)
        );

        let selected_while_ejected = (0..16)
            .map(|_| {
                pool.select()
                    .expect("backup remains eligible")
                    .server_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert!(selected_while_ejected.iter().all(|name| name == "backup"));

        pool.record_health(0, true, None, Some(base + 10), 1, 1);
        let recovered = pool.health_snapshot();
        assert!(!recovered.endpoints[0].passive_ejected);
        assert_eq!(recovered.endpoints[0].passive_recovery_count, 1);
        assert_eq!(
            recovered.endpoints[0].passive_last_recovery_at_unix_ms,
            Some(base + 10)
        );

        let selected_after_recovery = (0..4)
            .map(|_| {
                pool.select()
                    .expect("recovered endpoint")
                    .server_name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selected_after_recovery,
            ["primary", "primary", "primary", "backup"]
        );
    }

    #[test]
    fn passive_ejection_backoff_is_bounded_and_active_failures_are_not_double_counted() {
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-backoff".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
            PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(250)),
        )
        .expect("passive backoff pool");
        let base = now_unix_ms();

        pool.record_health(
            0,
            false,
            Some(HealthFailure::ConnectFailed),
            Some(base.saturating_sub(500)),
            1,
            1,
        );
        assert_eq!(pool.health_snapshot().endpoints[0].passive_failure_count, 0);

        pool.record_passive_failure_at(0, HealthFailure::Timeout, base);
        pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 101);
        pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 302);
        let snapshot = pool.health_snapshot();
        assert_eq!(snapshot.endpoints[0].passive_failure_count, 3);
        assert_eq!(snapshot.endpoints[0].passive_ejection_count, 3);
        assert_eq!(
            snapshot.endpoints[0].passive_ejection_until_unix_ms,
            Some(base + 552)
        );
        assert!(snapshot.endpoints[0].passive_ejected);
    }

    #[test]
    fn passive_ejection_fails_closed_without_revoking_concurrent_leases() {
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-leases".into(),
            [runtime_server("only", 3000, Some(2))],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
            PassiveFailurePolicy::new(1, Duration::from_secs(60), Duration::from_secs(60)),
        )
        .expect("passive lease pool");
        let base = now_unix_ms();
        let first = pool.select().expect("first lease");
        let second = pool.select().expect("second lease");

        pool.record_passive_failure_at(0, HealthFailure::ProtocolError, base);
        assert!(pool.select().is_none(), "ejected pool must fail closed");
        assert_eq!(pool.health_snapshot().unavailable_selections, 1);
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 2);

        drop((first, second));
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
    }

    #[test]
    fn invalid_passive_policy_is_rejected_at_pool_construction() {
        let error = RoundRobinPool::new_named_servers_with_policy(
            "invalid-passive".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
            PassiveFailurePolicy::new(0, Duration::from_secs(1), Duration::from_secs(1)),
        )
        .expect_err("zero threshold must be rejected");
        assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
    }

    #[test]
    fn passive_policy_bounds_are_rejected_at_pool_construction() {
        let policies = [
            PassiveFailurePolicy::new(101, Duration::from_secs(1), Duration::from_secs(1)),
            PassiveFailurePolicy::new(1, Duration::ZERO, Duration::from_secs(1)),
            PassiveFailurePolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
            PassiveFailurePolicy::new(
                1,
                Duration::from_secs(1),
                Duration::from_secs(24 * 60 * 60 + 1),
            ),
        ];

        for policy in policies {
            let error = RoundRobinPool::new_named_servers_with_policy(
                "invalid-passive".into(),
                [runtime_server("only", 3000, None)],
                UpstreamAlgorithm::First,
                None,
                None,
                policy,
            )
            .expect_err("invalid passive policy must be rejected");
            assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
        }
    }

    #[test]
    fn passive_ejection_expiry_allows_new_selection() {
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-expiry".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
            PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(100)),
        )
        .expect("passive expiry pool");
        let now = now_unix_ms();
        pool.record_passive_failure_at(0, HealthFailure::Timeout, now.saturating_sub(101));

        assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
        assert_eq!(
            pool.select().expect("expired endpoint").server_name(),
            "only"
        );
    }

    #[test]
    fn configured_passive_observation_filters_layer_seven_failures() {
        let mut policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
            observe: PassiveObserve::Layer4,
            on_error: PassiveOnError::Immediately,
            ..oxiroute_config::PassiveHealthPolicy::default()
        });
        policy.initial_ejection_duration = Duration::from_secs(60);
        policy.max_ejection_duration = Duration::from_secs(60);
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-observe".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
            policy,
        )
        .expect("passive observe pool");

        pool.record_passive_failure_at(0, HealthFailure::ProtocolError, now_unix_ms());
        assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
        pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
        assert!(pool.health_snapshot().endpoints[0].passive_ejected);
    }

    #[test]
    fn configured_mark_down_honors_the_error_limit_before_ejecting() {
        let policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
            on_error: PassiveOnError::MarkDown,
            error_limit: 2,
            ..oxiroute_config::PassiveHealthPolicy::default()
        });
        let pool = RoundRobinPool::new_named_servers_with_policy(
            "passive-mark-down".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Healthy),
            None,
            policy,
        )
        .expect("passive mark-down pool");

        pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
        assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
        pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
        let endpoint = &pool.health_snapshot().endpoints[0];
        assert!(endpoint.passive_ejected);
        assert_eq!(endpoint.state, EndpointHealthState::Unhealthy);
    }

    #[test]
    fn weighted_round_robin_leases_respect_capacity_and_release() {
        let pool = RoundRobinPool::new_named_servers(
            "weighted-capacity".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("backup", 3001, Some(1)),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![2, 1],
            },
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("weighted capacity pool");

        let primary = pool.select().expect("primary lease");
        assert_eq!(primary.server_name(), "primary");
        let backup = pool.select().expect("backup lease");
        assert_eq!(backup.server_name(), "backup");
        assert!(pool.select().is_none(), "both endpoints are at capacity");
        drop(primary);
        assert_eq!(
            pool.select().expect("released primary").server_name(),
            "primary"
        );
        drop(backup);
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
    fn concurrent_weighted_selection_preserves_cycle_fairness() {
        const THREADS: usize = 4;
        const SELECTIONS_PER_THREAD: usize = 100;

        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "weighted-concurrent".into(),
                [
                    runtime_server("primary", 3000, None),
                    runtime_server("backup", 3001, None),
                ],
                UpstreamAlgorithm::WeightedRoundRobin {
                    weights: vec![3, 1],
                },
                Some(HealthStartup::Healthy),
                None,
            )
            .expect("weighted concurrent pool"),
        );
        let selected = (0..THREADS)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    (0..SELECTIONS_PER_THREAD)
                        .map(|_| {
                            pool.select()
                                .expect("weighted endpoint")
                                .server_name()
                                .to_owned()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|task| task.join().expect("weighted selection thread"))
            .collect::<Vec<_>>();

        assert_eq!(
            selected
                .iter()
                .filter(|name| name.as_str() == "primary")
                .count(),
            THREADS * SELECTIONS_PER_THREAD * 3 / 4
        );
        assert_eq!(
            selected
                .iter()
                .filter(|name| name.as_str() == "backup")
                .count(),
            THREADS * SELECTIONS_PER_THREAD / 4
        );
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
        let writer = pool.health.health_writer.lock().expect("health writer");
        pool.health.health_version.store(1, Ordering::Release);
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
        pool.health.health_version.store(2, Ordering::Release);
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
