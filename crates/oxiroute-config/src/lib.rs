use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use http::uri::PathAndQuery;
use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};
use serde::Deserialize;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_LUA_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_LUA_INSTRUCTIONS: u32 = 1_000_000;
const INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

const DEFAULT_MAX_CONNECTIONS: u64 = 10_000;
const DEFAULT_UPSTREAM_IO_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_REQUEST_BODY_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;
const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub management: Option<Management>,
    pub listeners: Vec<Listener>,
    #[serde(default)]
    pub upstream_pools: Vec<UpstreamPool>,
    #[serde(default)]
    pub http_services: Vec<HttpService>,
    #[serde(default)]
    pub l4_services: Vec<L4Service>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Management {
    pub bind: SocketAddr,
    #[serde(default)]
    pub ui_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    pub name: String,
    pub bind: SocketAddr,
    pub protocol: Protocol,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default = "default_max_connections")]
    pub max_connections: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Rtmp,
    Tcp,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamPool {
    pub name: String,
    pub endpoints: Vec<SocketAddr>,
    #[serde(default)]
    pub algorithm: UpstreamAlgorithm,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamAlgorithm {
    #[default]
    RoundRobin,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpService {
    pub name: String,
    pub routes: Vec<HttpRoute>,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub upstream_io_timeout_ms: u64,
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRoute {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default = "default_path_prefix")]
    pub path_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub upstream_pool: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct L4Service {
    pub name: String,
    pub upstream_pool: String,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    #[serde(default)]
    pub lifetime_timeout_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Lua configuration failed: {0}")]
    Lua(#[from] mlua::Error),
    #[error("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")]
    SourceTooLarge,
    #[error("unsupported configuration version {0}; expected version 1")]
    UnsupportedVersion(u32),
    #[error("{namespace} at index {index} has a blank name")]
    BlankName {
        namespace: &'static str,
        index: usize,
    },
    #[error("{namespace} at index {index} has noncanonical name {name:?}")]
    InvalidName {
        namespace: &'static str,
        index: usize,
        name: String,
    },
    #[error("duplicate {namespace} name `{name}`")]
    DuplicateName {
        namespace: &'static str,
        name: String,
    },
    #[error("binds `{first_name}` ({first_bind}) and `{second_name}` ({second_bind}) overlap")]
    OverlappingBind {
        first_name: String,
        first_bind: SocketAddr,
        second_name: String,
        second_bind: SocketAddr,
    },
    #[error("{kind} `{name}` has an invalid zero port in `{field}`")]
    ZeroPort {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` must have a nonzero `{field}`")]
    ZeroLimit {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` exceeds the exact JSON integer limit in `{field}`")]
    LimitTooLarge {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{protocol:?} listener `{listener}` requires a service")]
    MissingListenerService {
        listener: String,
        protocol: Protocol,
    },
    #[error("{protocol:?} listener `{listener}` references unknown same-kind service `{service}`")]
    UnknownListenerService {
        listener: String,
        protocol: Protocol,
        service: String,
    },
    #[error("RTMP listener `{listener}` must not reference service `{service}`")]
    UnexpectedRtmpService { listener: String, service: String },
    #[error("upstream pool `{pool}` must contain at least one endpoint")]
    EmptyUpstreamEndpoints { pool: String },
    #[error("upstream pool `{pool}` contains duplicate endpoint `{endpoint}`")]
    DuplicateUpstreamEndpoint { pool: String, endpoint: SocketAddr },
    #[error("upstream pool `{pool}` exposes the loopback management endpoint `{endpoint}`")]
    ManagementUpstreamEndpoint { pool: String, endpoint: SocketAddr },
    #[error("HTTP service `{service}` must contain at least one route")]
    EmptyHttpRoutes { service: String },
    #[error("HTTP service `{service}` route {route} has invalid host `{host}`")]
    InvalidRouteHost {
        service: String,
        route: usize,
        host: String,
    },
    #[error("HTTP service `{service}` route {route} has invalid path prefix `{path_prefix}`")]
    InvalidRoutePathPrefix {
        service: String,
        route: usize,
        path_prefix: String,
    },
    #[error(
        "HTTP service `{service}` route {route} method `{method}` must be an uppercase HTTP token"
    )]
    InvalidRouteMethod {
        service: String,
        route: usize,
        method: String,
    },
    #[error("HTTP service `{service}` route {route} contains duplicate method `{method}`")]
    DuplicateRouteMethod {
        service: String,
        route: usize,
        method: String,
    },
    #[error(
        "HTTP service `{service}` routes {first_route} and {duplicate_route} have equivalent matchers"
    )]
    DuplicateHttpRoute {
        service: String,
        first_route: usize,
        duplicate_route: usize,
    },
    #[error("HTTP service `{service}` route {route} references unknown upstream pool `{pool}`")]
    UnknownRouteUpstreamPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error("L4 service `{service}` references unknown upstream pool `{pool}`")]
    UnknownL4UpstreamPool { service: String, pool: String },
    #[error("management listener must use loopback, got `{0}`")]
    ManagementMustUseLoopback(SocketAddr),
}

/// Loads a complete immutable configuration snapshot from restricted Lua.
///
/// # Errors
///
/// Returns an error when evaluation, deserialization, or validation fails.
pub fn load_lua(source: &str) -> Result<Config, ConfigError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ConfigError::SourceTooLarge);
    }

    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())?;
    lua.set_memory_limit(lua.used_memory().saturating_add(MAX_LUA_MEMORY_BYTES))?;

    let instructions = Arc::new(AtomicU32::new(MAX_LUA_INSTRUCTIONS));
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(INSTRUCTION_HOOK_INTERVAL),
        move |_lua, _debug| {
            if instructions.fetch_sub(INSTRUCTION_HOOK_INTERVAL, Ordering::Relaxed)
                <= INSTRUCTION_HOOK_INTERVAL
            {
                return Err(mlua::Error::runtime("Lua instruction limit exceeded"));
            }

            Ok(VmState::Continue)
        },
    )?;

    let value = lua
        .load(source)
        .set_name("oxiroute.lua")
        .set_mode(ChunkMode::Text)
        .eval()?;
    let mut config = lua.from_value(value)?;

    validate(&mut config)?;
    Ok(config)
}

fn validate(config: &mut Config) -> Result<(), ConfigError> {
    if config.version != 1 {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }

    validate_management(config.management.as_ref())?;
    validate_names(
        "listener",
        config
            .listeners
            .iter()
            .map(|listener| listener.name.as_str()),
    )?;
    validate_names(
        "upstream pool",
        config.upstream_pools.iter().map(|pool| pool.name.as_str()),
    )?;
    validate_names(
        "HTTP service",
        config
            .http_services
            .iter()
            .map(|service| service.name.as_str()),
    )?;
    validate_names(
        "L4 service",
        config
            .l4_services
            .iter()
            .map(|service| service.name.as_str()),
    )?;

    let upstream_pool_names = config
        .upstream_pools
        .iter()
        .map(|pool| pool.name.clone())
        .collect::<HashSet<_>>();
    let http_service_names = config
        .http_services
        .iter()
        .map(|service| service.name.clone())
        .collect::<HashSet<_>>();
    let l4_service_names = config
        .l4_services
        .iter()
        .map(|service| service.name.clone())
        .collect::<HashSet<_>>();

    validate_listeners(&config.listeners, &http_service_names, &l4_service_names)?;
    validate_bind_conflicts(config.management.as_ref(), &config.listeners)?;
    validate_upstream_pools(
        &config.upstream_pools,
        config.management.as_ref().map(|management| management.bind),
    )?;
    validate_http_services(&mut config.http_services, &upstream_pool_names)?;
    validate_l4_services(&config.l4_services, &upstream_pool_names)?;

    Ok(())
}

fn validate_management(management: Option<&Management>) -> Result<(), ConfigError> {
    let Some(management) = management else {
        return Ok(());
    };

    if !management.bind.ip().is_loopback() {
        return Err(ConfigError::ManagementMustUseLoopback(management.bind));
    }
    if management.bind.port() == 0 {
        return Err(ConfigError::ZeroPort {
            kind: "management listener",
            name: "management".into(),
            field: "bind",
        });
    }

    Ok(())
}

fn validate_bind_conflicts(
    management: Option<&Management>,
    listeners: &[Listener],
) -> Result<(), ConfigError> {
    let mut binds = Vec::with_capacity(listeners.len() + usize::from(management.is_some()));
    if let Some(management) = management {
        binds.push(("management".to_owned(), management.bind));
    }

    for listener in listeners {
        for (first_name, first_bind) in &binds {
            if binds_overlap(*first_bind, listener.bind) {
                return Err(ConfigError::OverlappingBind {
                    first_name: first_name.clone(),
                    first_bind: *first_bind,
                    second_name: listener.name.clone(),
                    second_bind: listener.bind,
                });
            }
        }
        binds.push((listener.name.clone(), listener.bind));
    }

    Ok(())
}

fn binds_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    let first_ip = canonical_ip(first.ip());
    let second_ip = canonical_ip(second.ip());
    first.port() == second.port()
        && (first_ip == second_ip || first_ip.is_unspecified() || second_ip.is_unspecified())
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

fn endpoint_exposes_management(endpoint: SocketAddr, management: SocketAddr) -> bool {
    let endpoint_ip = canonical_ip(endpoint.ip());
    endpoint.port() == management.port()
        && (endpoint_ip == canonical_ip(management.ip()) || endpoint_ip.is_unspecified())
}

fn validate_listeners(
    listeners: &[Listener],
    http_service_names: &HashSet<String>,
    l4_service_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    for listener in listeners {
        if listener.bind.port() == 0 {
            return Err(ConfigError::ZeroPort {
                kind: "listener",
                name: listener.name.clone(),
                field: "bind",
            });
        }
        if listener.max_connections == 0 {
            return Err(ConfigError::ZeroLimit {
                kind: "listener",
                name: listener.name.clone(),
                field: "max_connections",
            });
        }
        if listener.max_connections > MAX_SAFE_JSON_INTEGER {
            return Err(ConfigError::LimitTooLarge {
                kind: "listener",
                name: listener.name.clone(),
                field: "max_connections",
            });
        }

        match (listener.protocol, listener.service.as_deref()) {
            (Protocol::Http | Protocol::Tcp, None) => {
                return Err(ConfigError::MissingListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                });
            }
            (Protocol::Http, Some(service)) if !http_service_names.contains(service) => {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (Protocol::Tcp, Some(service)) if !l4_service_names.contains(service) => {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (Protocol::Rtmp, Some(service)) => {
                return Err(ConfigError::UnexpectedRtmpService {
                    listener: listener.name.clone(),
                    service: service.into(),
                });
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_upstream_pools(
    upstream_pools: &[UpstreamPool],
    management_bind: Option<SocketAddr>,
) -> Result<(), ConfigError> {
    for pool in upstream_pools {
        if pool.endpoints.is_empty() {
            return Err(ConfigError::EmptyUpstreamEndpoints {
                pool: pool.name.clone(),
            });
        }

        let mut endpoints = HashSet::with_capacity(pool.endpoints.len());
        for endpoint in &pool.endpoints {
            if endpoint.port() == 0 {
                return Err(ConfigError::ZeroPort {
                    kind: "upstream pool",
                    name: pool.name.clone(),
                    field: "endpoints",
                });
            }
            if !endpoints.insert(*endpoint) {
                return Err(ConfigError::DuplicateUpstreamEndpoint {
                    pool: pool.name.clone(),
                    endpoint: *endpoint,
                });
            }
            if management_bind
                .is_some_and(|management| endpoint_exposes_management(*endpoint, management))
            {
                return Err(ConfigError::ManagementUpstreamEndpoint {
                    pool: pool.name.clone(),
                    endpoint: *endpoint,
                });
            }
        }
    }

    Ok(())
}

fn validate_http_services(
    http_services: &mut [HttpService],
    upstream_pool_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    for service in http_services {
        if service.routes.is_empty() {
            return Err(ConfigError::EmptyHttpRoutes {
                service: service.name.clone(),
            });
        }
        if service.upstream_io_timeout_ms == 0 {
            return Err(ConfigError::ZeroLimit {
                kind: "HTTP service",
                name: service.name.clone(),
                field: "upstream_io_timeout_ms",
            });
        }
        if service.max_request_body_bytes == 0 {
            return Err(ConfigError::ZeroLimit {
                kind: "HTTP service",
                name: service.name.clone(),
                field: "max_request_body_bytes",
            });
        }

        let mut matchers = HashMap::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter_mut().enumerate() {
            if let Some(host) = &mut route.host {
                if !normalize_host(host) {
                    return Err(ConfigError::InvalidRouteHost {
                        service: service.name.clone(),
                        route: route_index,
                        host: host.clone(),
                    });
                }
            }
            if !normalize_path_prefix(&mut route.path_prefix) {
                return Err(ConfigError::InvalidRoutePathPrefix {
                    service: service.name.clone(),
                    route: route_index,
                    path_prefix: route.path_prefix.clone(),
                });
            }

            let mut methods = HashSet::with_capacity(route.methods.len());
            for method in &route.methods {
                if !is_uppercase_http_token(method) {
                    return Err(ConfigError::InvalidRouteMethod {
                        service: service.name.clone(),
                        route: route_index,
                        method: method.clone(),
                    });
                }
                if !methods.insert(method.as_str()) {
                    return Err(ConfigError::DuplicateRouteMethod {
                        service: service.name.clone(),
                        route: route_index,
                        method: method.clone(),
                    });
                }
            }

            let mut canonical_methods = route.methods.clone();
            canonical_methods.sort_unstable();
            let matcher = (
                route.host.clone(),
                route.path_prefix.clone(),
                canonical_methods,
            );
            if let Some(first_route) = matchers.insert(matcher, route_index) {
                return Err(ConfigError::DuplicateHttpRoute {
                    service: service.name.clone(),
                    first_route,
                    duplicate_route: route_index,
                });
            }

            if !upstream_pool_names.contains(&route.upstream_pool) {
                return Err(ConfigError::UnknownRouteUpstreamPool {
                    service: service.name.clone(),
                    route: route_index,
                    pool: route.upstream_pool.clone(),
                });
            }
        }
    }

    Ok(())
}

fn validate_l4_services(
    l4_services: &[L4Service],
    upstream_pool_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    for service in l4_services {
        if !upstream_pool_names.contains(&service.upstream_pool) {
            return Err(ConfigError::UnknownL4UpstreamPool {
                service: service.name.clone(),
                pool: service.upstream_pool.clone(),
            });
        }
        if service.connect_timeout_ms == 0 {
            return Err(ConfigError::ZeroLimit {
                kind: "L4 service",
                name: service.name.clone(),
                field: "connect_timeout_ms",
            });
        }
        if service.idle_timeout_ms == 0 {
            return Err(ConfigError::ZeroLimit {
                kind: "L4 service",
                name: service.name.clone(),
                field: "idle_timeout_ms",
            });
        }
        if service.lifetime_timeout_ms == Some(0) {
            return Err(ConfigError::ZeroLimit {
                kind: "L4 service",
                name: service.name.clone(),
                field: "lifetime_timeout_ms",
            });
        }
    }

    Ok(())
}

fn validate_names<'a>(
    namespace: &'static str,
    names: impl Iterator<Item = &'a str>,
) -> Result<(), ConfigError> {
    let mut unique = HashSet::new();
    for (index, name) in names.enumerate() {
        if name.trim().is_empty() {
            return Err(ConfigError::BlankName { namespace, index });
        }
        if name.trim() != name || name.chars().any(char::is_control) {
            return Err(ConfigError::InvalidName {
                namespace,
                index,
                name: name.into(),
            });
        }
        if !unique.insert(name) {
            return Err(ConfigError::DuplicateName {
                namespace,
                name: name.into(),
            });
        }
    }
    Ok(())
}

fn normalize_host(host: &mut String) -> bool {
    host.make_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>() {
        *host = ip.to_string();
        return true;
    }

    if host.len() > 253 {
        return false;
    }

    let dns_name = if let Some(dns_name) = host.strip_prefix("*.") {
        if dns_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        dns_name
    } else {
        if host.starts_with('*') {
            return false;
        }
        host.as_str()
    };

    !dns_name.is_empty()
        && dns_name.split('.').all(|label| {
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

fn normalize_path_prefix(path_prefix: &mut String) -> bool {
    let valid = path_prefix.starts_with('/')
        && path_prefix
            .parse::<PathAndQuery>()
            .is_ok_and(|path| path.query().is_none() && path.path() == path_prefix);
    if !valid {
        return false;
    }

    let normalized = path_prefix.trim_end_matches('/');
    let normalized = if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized.to_owned()
    };
    let Some(canonical) = canonicalize_http_path(&normalized) else {
        return false;
    };
    *path_prefix = canonical.into_owned();
    true
}

/// Returns whether an HTTP path has one stable interpretation for routing and forwarding.
///
/// Dot segments, backslashes, repeated separators, malformed escapes, percent-encoded
/// unreserved characters, and encoded path separators are rejected.
#[must_use]
pub fn is_unambiguous_http_path(path: &str) -> bool {
    canonicalize_http_path(path).is_some()
}

/// Validates an HTTP path and uppercases accepted percent-triplet hex digits.
#[must_use]
pub fn canonicalize_http_path(path: &str) -> Option<Cow<'_, str>> {
    let bytes = path.as_bytes();
    if bytes.contains(&b'\\')
        || bytes.windows(2).any(|window| window == b"//")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return None;
    }

    let mut canonical = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| decode_hex_byte(digits[0], digits[1]))?;
        if encoded.is_ascii_alphanumeric()
            || matches!(encoded, b'-' | b'.' | b'_' | b'~' | b'/' | b'\\')
        {
            return None;
        }
        if bytes[index + 1] != bytes[index + 1].to_ascii_uppercase()
            || bytes[index + 2] != bytes[index + 2].to_ascii_uppercase()
        {
            let canonical = canonical.get_or_insert_with(|| bytes.to_vec());
            canonical[index + 1] = canonical[index + 1].to_ascii_uppercase();
            canonical[index + 2] = canonical[index + 2].to_ascii_uppercase();
        }
        index += 3;
    }
    canonical.map_or_else(
        || Some(Cow::Borrowed(path)),
        |canonical| String::from_utf8(canonical).ok().map(Cow::Owned),
    )
}

fn decode_hex_byte(high: u8, low: u8) -> Option<u8> {
    Some(hex_nibble(high)? << 4 | hex_nibble(low)?)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn is_uppercase_http_token(method: &str) -> bool {
    !method.is_empty()
        && method.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&byte)
        })
}

const fn default_max_connections() -> u64 {
    DEFAULT_MAX_CONNECTIONS
}

const fn default_upstream_io_timeout_ms() -> u64 {
    DEFAULT_UPSTREAM_IO_TIMEOUT_MS
}

const fn default_max_request_body_bytes() -> u64 {
    DEFAULT_MAX_REQUEST_BODY_BYTES
}

const fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}

const fn default_idle_timeout_ms() -> u64 {
    DEFAULT_IDLE_TIMEOUT_MS
}

fn default_path_prefix() -> String {
    "/".into()
}
