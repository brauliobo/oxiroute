use std::{
    borrow::Cow,
    error::Error,
    fmt,
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicUsize, Ordering},
};

use http::{
    Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::canonicalize_http_path;

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostMatcher {
    Any,
    Exact(String),
    Wildcard(String),
}

impl HostMatcher {
    fn rank_if_matches(&self, host: Option<&str>) -> Option<u8> {
        match self {
            Self::Any => Some(0),
            Self::Exact(expected) => host
                .is_some_and(|host| exact_host_matches(host, expected))
                .then_some(2),
            Self::Wildcard(suffix) => host
                .is_some_and(|host| wildcard_matches(host, suffix))
                .then_some(1),
        }
    }
}

/// A normalized HTTP route associated with a pool identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    host: HostMatcher,
    path_prefix: String,
    methods: Option<Box<[Method]>>,
    pool_id: String,
}

impl Route {
    /// Creates a route from a host pattern, path prefix, optional method set, and pool identity.
    ///
    /// A host of `None` matches every host. Wildcards must have the form `*.example.com` and
    /// match exactly one label. Trailing slashes are removed from path prefixes other than `/`.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError`] when the host pattern or path prefix is invalid, the method set is
    /// empty, or the pool identity is empty.
    pub fn new(
        host: Option<&str>,
        path_prefix: &str,
        methods: Option<Vec<Method>>,
        pool_id: impl Into<String>,
    ) -> Result<Self, RouteError> {
        let host = normalize_host(host)?;
        let path_prefix = normalize_path_prefix(path_prefix)?;
        let methods = match methods {
            Some(methods) if methods.is_empty() => return Err(RouteError::EmptyMethodSet),
            Some(methods) => Some(methods.into_boxed_slice()),
            None => None,
        };
        let pool_id = pool_id.into();
        if pool_id.is_empty() {
            return Err(RouteError::EmptyPoolIdentity);
        }

        Ok(Self {
            host,
            path_prefix,
            methods,
            pool_id,
        })
    }

    #[must_use]
    pub fn path_prefix(&self) -> &str {
        &self.path_prefix
    }

    #[must_use]
    pub fn pool_id(&self) -> &str {
        &self.pool_id
    }

    fn matches_method(&self, method: &Method) -> bool {
        self.methods
            .as_ref()
            .is_none_or(|methods| methods.contains(method))
    }

    fn matches_path(&self, path: &str) -> bool {
        self.path_prefix == "/"
            || path == self.path_prefix
            || path
                .strip_prefix(&self.path_prefix)
                .is_some_and(|remainder| remainder.starts_with('/'))
    }
}

/// Errors produced while normalizing a route definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteError {
    InvalidHost(String),
    InvalidPathPrefix(String),
    EmptyMethodSet,
    EmptyPoolIdentity,
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHost(host) => write!(formatter, "invalid route host pattern `{host}`"),
            Self::InvalidPathPrefix(path) => {
                write!(formatter, "invalid route path prefix `{path}`")
            }
            Self::EmptyMethodSet => formatter.write_str("route method set cannot be empty"),
            Self::EmptyPoolIdentity => formatter.write_str("route pool identity cannot be empty"),
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
    /// Host precedence is exact, wildcard, then catch-all. Within a host class, the longest path
    /// prefix wins, and source order resolves remaining ties. The explicit authority is preferred;
    /// when absent, an authority from an absolute URI is used.
    #[must_use]
    pub fn select(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<&Route> {
        let host = authority.or_else(|| uri.authority()).map(Authority::host);
        let path = canonicalize_http_path(uri.path())?;
        let mut best = None;

        for route in &self.routes {
            if !route.matches_method(method) || !route.matches_path(path.as_ref()) {
                continue;
            }
            let Some(host_rank) = route.host.rank_if_matches(host) else {
                continue;
            };
            let score = (host_rank, route.path_prefix.len());

            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((route, score)),
            }
        }

        best.map(|(route, _)| route)
    }
}

fn normalize_host(host: Option<&str>) -> Result<HostMatcher, RouteError> {
    let Some(host) = host else {
        return Ok(HostMatcher::Any);
    };
    if host.len() > 253 {
        return Err(RouteError::InvalidHost(host.to_owned()));
    }

    if let Some(suffix) = host.strip_prefix("*.") {
        if suffix.parse::<IpAddr>().is_ok() || !is_dns_name(suffix) {
            return Err(RouteError::InvalidHost(host.to_owned()));
        }
        return Ok(HostMatcher::Wildcard(suffix.to_ascii_lowercase()));
    }

    if let Some(ip) = parse_host_ip(host) {
        return Ok(HostMatcher::Exact(ip.to_string()));
    }
    if !is_dns_name(host) {
        return Err(RouteError::InvalidHost(host.to_owned()));
    }

    Ok(HostMatcher::Exact(host.to_ascii_lowercase()))
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

fn normalize_path_prefix(path_prefix: &str) -> Result<String, RouteError> {
    let valid_path = path_prefix.starts_with('/')
        && path_prefix
            .parse::<PathAndQuery>()
            .is_ok_and(|path| path.query().is_none() && path.path() == path_prefix);
    if !valid_path {
        return Err(RouteError::InvalidPathPrefix(path_prefix.to_owned()));
    }

    let normalized = path_prefix.trim_end_matches('/');
    let normalized = if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized.to_owned()
    };
    canonicalize_http_path(&normalized)
        .map(Cow::into_owned)
        .ok_or_else(|| RouteError::InvalidPathPrefix(path_prefix.to_owned()))
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

fn exact_host_matches(host: &str, expected: &str) -> bool {
    host.eq_ignore_ascii_case(expected)
        || host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .is_some_and(|host| host.eq_ignore_ascii_case(expected))
}

/// A lock-free round-robin selector over a fixed, nonempty endpoint list.
#[derive(Debug)]
pub struct RoundRobinPool {
    endpoints: Box<[SocketAddr]>,
    next: AtomicUsize,
}

impl RoundRobinPool {
    /// Creates a pool whose first selection returns the first endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided.
    pub fn new(endpoints: impl IntoIterator<Item = SocketAddr>) -> Result<Self, PoolError> {
        let endpoints = endpoints.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if endpoints.is_empty() {
            return Err(PoolError::Empty);
        }

        Ok(Self {
            endpoints,
            next: AtomicUsize::new(0),
        })
    }

    #[must_use]
    pub fn select(&self) -> SocketAddr {
        self.endpoints[self.next_index()]
    }

    /// Selects the next endpoint not present in a request's attempted-endpoint set.
    #[must_use]
    pub fn select_excluding(&self, excluded: &[SocketAddr]) -> Option<SocketAddr> {
        let start = self.next_index();
        for offset in 0..self.endpoints.len() {
            let index = (start + offset) % self.endpoints.len();
            let endpoint = self.endpoints[index];
            if !excluded.contains(&endpoint) {
                return Some(endpoint);
            }
        }
        None
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[SocketAddr]) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| !attempted.contains(endpoint))
    }

    fn next_index(&self) -> usize {
        let endpoint_count = self.endpoints.len();
        let selected = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(if current == endpoint_count - 1 {
                    0
                } else {
                    current + 1
                })
            });
        match selected {
            Ok(index) | Err(index) => index,
        }
    }
}

/// Errors produced while constructing an endpoint pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    Empty,
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("round-robin pool cannot be empty"),
        }
    }
}

impl Error for PoolError {}
