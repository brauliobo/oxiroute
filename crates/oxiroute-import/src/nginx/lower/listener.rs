use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use http::header::{HeaderName, HeaderValue};
use oxiroute_config::{
    AccessLogPolicy, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpAccessPolicy,
    HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpGzipMinimumVersion, HttpGzipPolicy,
    HttpHostSelector, HttpLiteralHeader, HttpMimeType, HttpPathSelector, HttpProxyPathRewrite,
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpSameSite, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
    HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost, HttpVersionPolicy, Listener,
    ListenerBind, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint,
    UpstreamPool, UpstreamServer, UpstreamTls, canonical_dns_name as canonical_endpoint_dns_name,
    canonicalize_http_path,
};

use crate::canonical::absolute_file_path;
use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveLocation, EffectiveServer,
    ListenEndpoint, LocationKind, OccurrenceId, ProxyPassScheme, ServerNameKind, StaticEndpoint,
    semantic::certbot_host_condition,
};

use super::{
    BindBlock, BindCandidate, GzipOrigins, LowerIssue, Lowerer, PoolCandidate,
    provenance::{PolicyValue, collect_result, issue, utf8},
    upstream::canonical_endpoint,
};

const NGINX_DEFAULT_BODY_BYTES: u64 = 1024 * 1024;
const NGINX_DEFAULT_PROXY_TIMEOUT_MS: u64 = 60_000;
const NGINX_DEFAULT_GZIP_LEVEL: u8 = 1;
const NGINX_DEFAULT_GZIP_MIN_LENGTH_BYTES: u64 = 20;
const NGINX_MAX_HEADER_NAME_BYTES: usize = 64;
const NGINX_MAX_HEADER_VALUE_BYTES: usize = 8192;
const NGINX_MAX_LITERAL_HEADERS: usize = 32;
const NGINX_MAX_STATIC_ERROR_RESPONSES: usize = 16;
const NGINX_MAX_STATIC_ERROR_STATUSES: usize = 16;
const NGINX_MAX_STATIC_ERROR_TARGET_BYTES: usize = 1024;
const NGINX_IMPLICIT_GZIP_TYPE: &str = "text/html";
const NGINX_GZIP_PROXIED_MODES: [&[u8]; 9] = [
    b"off",
    b"expired",
    b"no-cache",
    b"no-store",
    b"private",
    b"no_last_modified",
    b"no_etag",
    b"auth",
    b"any",
];
const NGINX_DEFAULT_HIDDEN_RESPONSE_HEADERS: [&str; 8] = [
    "Date",
    "Server",
    "X-Pad",
    "X-Accel-Expires",
    "X-Accel-Redirect",
    "X-Accel-Limit-Rate",
    "X-Accel-Buffering",
    "X-Accel-Charset",
];
const NGINX_RESPONSE_CONTROL_HEADERS: [&[u8]; 5] = [
    b"X-Accel-Redirect",
    b"X-Accel-Expires",
    b"X-Accel-Limit-Rate",
    b"X-Accel-Buffering",
    b"X-Accel-Charset",
];

fn socket_addr(endpoint: &ListenEndpoint) -> Option<SocketAddr> {
    let ListenEndpoint::Socket { address, port } = endpoint else {
        return None;
    };
    let address = address
        .strip_prefix(b"[")
        .and_then(|address| address.strip_suffix(b"]"))
        .unwrap_or(address);
    let ip = utf8(address)?.parse::<IpAddr>().ok()?;
    Some(SocketAddr::new(ip, *port))
}

fn listener_bind(bind: &EffectiveBind, _servers: &[&EffectiveServer]) -> Option<ListenerBind> {
    match &bind.endpoint {
        ListenEndpoint::Unix { path } => Some(ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        }),
        ListenEndpoint::Socket { address, port } => socket_addr(&bind.endpoint)
            .or_else(|| {
                (address == b"*").then(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), *port))
            })
            .map(|address| ListenerBind::Socket { address }),
    }
}

pub(super) fn matching_listen<'a>(
    server: &'a EffectiveServer,
    endpoint: &ListenEndpoint,
) -> &'a crate::nginx::EffectiveListen {
    server
        .listens
        .iter()
        .find(|listen| listen.endpoint.as_ref() == Some(endpoint))
        .expect("effective bind retains each contributing listen")
}

pub(super) fn canonical_exact_host(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    canonical_endpoint_dns_name(host).ok()
}

pub(super) fn canonical_wildcard_host(host: &str) -> bool {
    host.strip_prefix("*.").is_some_and(canonical_dns_name)
}

pub(super) fn canonical_dns_name(name: &str) -> bool {
    name.is_ascii()
        && !name.is_empty()
        && name.len() <= 253
        && !name.ends_with('.')
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

pub(super) fn parse_size(value: &[u8]) -> Option<u64> {
    parse_size_inner(value, false)
}

fn parse_nonnegative_size(value: &[u8]) -> Option<u64> {
    parse_size_inner(value, true)
}

fn parse_size_inner(value: &[u8], allow_zero: bool) -> Option<u64> {
    let (digits, multiplier) = match value.last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return None,
    };
    let amount = utf8(digits)?.parse::<u64>().ok()?;
    let bytes = amount.checked_mul(multiplier)?;
    ((allow_zero || bytes > 0) && bytes <= 9_007_199_254_740_991).then_some(bytes)
}

pub(super) fn parse_duration_ms(value: &[u8]) -> Option<u64> {
    const MAX_CANONICAL_INTEGER: u64 = 9_007_199_254_740_991;

    if value.iter().all(u8::is_ascii_digit) {
        let amount = utf8(value)?.parse::<u64>().ok()?;
        let milliseconds = amount.checked_mul(1_000)?;
        return (milliseconds > 0 && milliseconds <= MAX_CANONICAL_INTEGER).then_some(milliseconds);
    }

    let mut remaining = value;
    let mut previous_rank = None;
    let mut only_bare_value_remains = false;
    let mut total = 0_u64;
    while !remaining.is_empty() {
        let digit_count = remaining
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digit_count == 0 {
            return None;
        }
        let amount = utf8(&remaining[..digit_count])?.parse::<u64>().ok()?;
        remaining = &remaining[digit_count..];
        if remaining.is_empty() {
            total = total.checked_add(amount.checked_mul(1_000)?)?;
            break;
        }
        if only_bare_value_remains {
            return None;
        }
        let (rank, multiplier, suffix_bytes) = if remaining.starts_with(b"ms") {
            (7_u8, 1_u64, 2_usize)
        } else {
            match remaining[0] {
                b'w' => (2, 7 * 86_400_000, 1),
                b'd' => (3, 86_400_000, 1),
                b'h' => (4, 3_600_000, 1),
                b'm' => (5, 60_000, 1),
                b's' => (6, 1_000, 1),
                _ => return None,
            }
        };
        if previous_rank.is_some_and(|previous| rank <= previous) {
            return None;
        }
        previous_rank = Some(rank);
        remaining = &remaining[suffix_bytes..];
        total = total.checked_add(amount.checked_mul(multiplier)?)?;
        if remaining.first() == Some(&b' ') {
            if rank >= 6 {
                return None;
            }
            remaining = remaining.trim_ascii_start();
            only_bare_value_remains = true;
        }
    }
    (total > 0 && total <= MAX_CANONICAL_INTEGER).then_some(total)
}

fn valid_gzip_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && value.len() <= 128
        && kind.bytes().chain(subtype.bytes()).all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

enum NginxProxyValue {
    IncomingAuthority,
    NginxHost(String),
    ClientIp,
    AppendedXForwardedFor,
    DownstreamScheme,
    IncomingUpgrade,
    Literal(String),
}

impl NginxProxyValue {
    fn into_upstream_host(self) -> Option<HttpUpstreamHost> {
        match self {
            Self::IncomingAuthority => Some(HttpUpstreamHost::PreserveIncoming),
            Self::NginxHost(fallback) => Some(HttpUpstreamHost::NginxHost { fallback }),
            Self::Literal(value) => Some(HttpUpstreamHost::Literal { value }),
            Self::ClientIp
            | Self::AppendedXForwardedFor
            | Self::DownstreamScheme
            | Self::IncomingUpgrade => None,
        }
    }

    fn into_request_header_value(self) -> HttpRequestHeaderValue {
        match self {
            Self::IncomingAuthority => HttpRequestHeaderValue::IncomingAuthority,
            Self::NginxHost(fallback) => HttpRequestHeaderValue::NginxHost { fallback },
            Self::ClientIp => HttpRequestHeaderValue::ClientIp,
            Self::AppendedXForwardedFor => HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 8_192,
                except_source_cidrs: Vec::new(),
            },
            Self::DownstreamScheme => HttpRequestHeaderValue::DownstreamScheme,
            Self::IncomingUpgrade => HttpRequestHeaderValue::IncomingHeader {
                name: "upgrade".into(),
                max_bytes: 8_192,
            },
            Self::Literal(value) => HttpRequestHeaderValue::Literal { value },
        }
    }
}

include!("listener/bind.rs");
include!("listener/routes.rs");
include!("listener/proxy.rs");
include!("listener/static_aux.rs");
