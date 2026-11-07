#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(super) fn normalized_authority_host(authority: &Authority) -> Option<String> {
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
