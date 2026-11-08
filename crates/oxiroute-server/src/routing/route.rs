#[allow(clippy::wildcard_imports)]
use super::*;

use std::borrow::Cow;

mod index;
mod paths;
#[cfg(test)]
mod reference_tests;

use index::RouteIndex;
use paths::PathIndex;

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

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathMatcher {
    RawPrefix(String),
    SegmentPrefix(String),
    Exact(String),
    AsciiCaseInsensitiveExact(String),
}

impl PathMatcher {
    /// Path rank (exact, segment prefix, raw prefix) and the length of the matched value.
    fn precedence(&self) -> (u8, usize) {
        let rank = match self {
            Self::RawPrefix(_) => 0,
            Self::SegmentPrefix(_) => 1,
            Self::Exact(_) | Self::AsciiCaseInsensitiveExact(_) => 2,
        };
        (rank, self.value().len())
    }

    fn matches(&self, path: &str) -> bool {
        match self {
            Self::RawPrefix(value) => path.starts_with(value),
            Self::SegmentPrefix(value) => {
                value == "/"
                    || path == value
                    || path
                        .strip_prefix(value.as_str())
                        .is_some_and(|remainder| value.ends_with('/') || remainder.starts_with('/'))
            }
            Self::Exact(value) => path == value,
            Self::AsciiCaseInsensitiveExact(value) => path.eq_ignore_ascii_case(value),
        }
    }

    fn rank_if_matches(&self, path: &str) -> Option<(u8, usize)> {
        self.matches(path).then(|| self.precedence())
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

/// An immutable table that selects from routes by precedence.
#[derive(Clone, Debug, Default)]
pub struct RouteTable {
    routes: Box<[Route]>,
    index: RouteIndex,
}

impl PartialEq for RouteTable {
    fn eq(&self, other: &Self) -> bool {
        self.routes == other.routes
    }
}

impl Eq for RouteTable {}

impl RouteTable {
    #[must_use]
    pub fn new(routes: Vec<Route>) -> Self {
        let index = RouteIndex::new(&routes);
        Self {
            routes: routes.into_boxed_slice(),
            index,
        }
    }

    /// Selects the highest-priority route for the request.
    ///
    /// Host precedence is exact authority, normalized exact/IP, wildcard, then catch-all. Path
    /// precedence is exact, segment prefix, then raw prefix, with longer prefixes winning within a
    /// kind. A method-specific route precedes an any-method route and source order resolves ties.
    /// The cost does not grow with the number of routes: the request authority is normalized once
    /// and only routes whose host selector can match are visited.
    #[must_use]
    pub fn select(
        &self,
        authority: Option<&Authority>,
        uri: &Uri,
        method: &Method,
    ) -> Option<&Route> {
        let authority = authority.or_else(|| uri.authority());
        let path = canonicalize_http_path(uri.path())?;
        let best = self.index.best(&self.routes, authority, &path, method)?;
        Some(&self.routes[best.order()])
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

pub(super) fn is_dns_name(host: &str) -> bool {
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

pub(super) fn normalized_authority_host(authority: &Authority) -> Option<Cow<'_, str>> {
    let authority_host = authority.host();
    let host = authority_host.strip_suffix('.').unwrap_or(authority_host);
    if let Some(ip) = parse_host_ip(host) {
        Some(ip.to_string().into())
    } else if is_dns_name(host) {
        Some(if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
            host.to_ascii_lowercase().into()
        } else {
            host.into()
        })
    } else {
        None
    }
}
