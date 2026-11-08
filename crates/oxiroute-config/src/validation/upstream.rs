#[allow(clippy::wildcard_imports)]
use super::*;

mod health_udp;
mod passive;

#[allow(clippy::wildcard_imports)]
pub use health_udp::*;
#[allow(clippy::wildcard_imports)]
use passive::*;

const MAX_EJECTION_JITTER_PERCENT: u8 = 50;
const MAX_UNAVAILABLE_BODY_BYTES: usize = 1024;
const MAX_READMISSION_INTERVAL_MS: u64 = 3_600_000;
const MAX_BREAKER_MS: u64 = 3_600_000;
const MAX_QUEUE_CLASSES: usize = 8;
const MAX_CANARY_PATH_BYTES: usize = 1024;

/// Validates upstream-pool identities, resources, endpoints, and health policies.
///
/// # Errors
///
/// Returns an error when pool names, cardinality, endpoints, management isolation, or health
/// policies are invalid.
pub fn validate_upstream_pools(
    upstream_pools: &[UpstreamPool],
    management_bind: Option<SocketAddr>,
) -> Result<(), ConfigError> {
    validate_upstream_pool_definitions(upstream_pools, management_bind)?;
    for pool in upstream_pools {
        if let Some(health_check) = &pool.health_check {
            validate_health_check_config(&pool.name, health_check)?;
        }
        if let Some(passive_health) = &pool.passive_health {
            validate_passive_health_config(&pool.name, passive_health)?;
        }
    }

    Ok(())
}

/// Validates upstream-pool identities, resources, endpoints, and management isolation.
///
/// This lower-level helper excludes health-policy validation for callers that need to inspect only
/// pool identity, resource, endpoint, and management-isolation rules.
///
/// # Errors
///
/// Returns an error when pool names, cardinality, endpoints, or management isolation are invalid.
pub fn validate_upstream_pool_definitions(
    upstream_pools: &[UpstreamPool],
    management_bind: Option<SocketAddr>,
) -> Result<(), ConfigError> {
    validate_names(
        "upstream pool",
        upstream_pools.iter().map(|pool| pool.name.as_str()),
    )?;
    validate_upstream_pool_cardinality(upstream_pools)?;
    for pool in upstream_pools {
        validate_upstream_pool_definition(pool, management_bind)?;
    }

    Ok(())
}

fn validate_upstream_pool_definition(
    pool: &UpstreamPool,
    management_bind: Option<SocketAddr>,
) -> Result<(), ConfigError> {
    validate_upstream_servers(pool, management_bind)?;
    validate_upstream_algorithm(pool)?;
    let has_unix_endpoint = pool
        .servers
        .iter()
        .any(|server| matches!(server.endpoint, UpstreamEndpoint::Unix { .. }));
    if has_unix_endpoint && pool.tls.is_some() {
        return Err(ConfigError::UnsupportedUnixUpstreamTls {
            pool: pool.name.clone(),
        });
    }
    if has_unix_endpoint && pool.health_check.is_some() {
        return Err(ConfigError::UnsupportedUnixHealthCheck {
            pool: pool.name.clone(),
        });
    }
    if pool.health_check.is_some() && pool.tls.is_some() {
        return Err(ConfigError::UnsupportedTlsHealthCheck {
            pool: pool.name.clone(),
        });
    }
    if let Some(tls) = &pool.tls {
        if !is_valid_dns_name(&tls.server_name) {
            return Err(ConfigError::InvalidUpstreamTlsServerName {
                pool: pool.name.clone(),
                server_name: tls.server_name.clone(),
            });
        }
        if let Some(ca_certificate_path) = &tls.ca_certificate_path {
            validate_file_path(
                "upstream pool",
                &pool.name,
                "tls.ca_certificate_path",
                ca_certificate_path,
            )?;
        }
    }

    if matches!(
        (pool.http_versions.min, pool.http_versions.max),
        (HttpVersion::Http2, HttpVersion::Http11)
    ) {
        return Err(ConfigError::InvalidHttpVersionRange {
            pool: pool.name.clone(),
            min: pool.http_versions.min.as_str(),
            max: pool.http_versions.max.as_str(),
        });
    }
    if pool.http_versions.min == HttpVersion::Http3 || pool.http_versions.max == HttpVersion::Http3
    {
        if pool.http_versions.min != HttpVersion::Http3
            || pool.http_versions.max != HttpVersion::Http3
        {
            return Err(ConfigError::InvalidHttpVersionRange {
                pool: pool.name.clone(),
                min: pool.http_versions.min.as_str(),
                max: pool.http_versions.max.as_str(),
            });
        }
        if pool.tls.is_none() {
            return Err(ConfigError::H3RequiresUpstreamTls {
                pool: pool.name.clone(),
            });
        }
    }
    if pool.http_versions.max == HttpVersion::Http2 && pool.tls.is_none() {
        return Err(ConfigError::H2RequiresUpstreamTls {
            pool: pool.name.clone(),
        });
    }
    validate_upstream_pool_timeouts(pool)?;
    validate_pool_resilience(pool)?;
    validate_pool_queue(pool)?;
    validate_pool_canary(pool, has_unix_endpoint)?;
    validate_pool_registration(pool)
}

fn validate_pool_canary(pool: &UpstreamPool, has_unix_endpoint: bool) -> Result<(), ConfigError> {
    let Some(canary) = &pool.canary else {
        return Ok(());
    };
    let invalid = |field, detail| ConfigError::InvalidUpstreamServer {
        pool: pool.name.clone(),
        server: "<pool>".into(),
        field,
        detail,
    };
    if has_unix_endpoint {
        return Err(invalid("canary", "does not support unix endpoints"));
    }
    if pool.tls.is_some() {
        return Err(invalid("canary", "does not support upstream tls"));
    }
    if !canary.path.starts_with('/')
        || canary.path.len() > MAX_CANARY_PATH_BYTES
        || canary.path.bytes().any(|byte| !byte.is_ascii_graphic())
    {
        return Err(invalid(
            "canary.path",
            "must start with / and have at most 1024 visible ASCII bytes",
        ));
    }
    if canary
        .host
        .as_ref()
        .is_some_and(|host| host.is_empty() || !host.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Err(invalid(
            "canary.host",
            "must be visible ASCII without spaces",
        ));
    }
    if !(100..=599).contains(&canary.expected_status) {
        return Err(invalid(
            "canary.expected_status",
            "must be between 100 and 599",
        ));
    }
    if !(1_000..=MAX_HTTP_TIMEOUT_MS).contains(&canary.interval_ms) {
        return Err(invalid(
            "canary.interval_ms",
            "must be between 1000 and 86400000",
        ));
    }
    if !(100..=canary.interval_ms).contains(&canary.timeout_ms) {
        return Err(invalid(
            "canary.timeout_ms",
            "must be between 100 and interval_ms",
        ));
    }
    if canary
        .max_latency_ms
        .is_some_and(|latency| !(1..=canary.timeout_ms).contains(&latency))
    {
        return Err(invalid(
            "canary.max_latency_ms",
            "must be between 1 and timeout_ms",
        ));
    }
    Ok(())
}

fn validate_pool_queue(pool: &UpstreamPool) -> Result<(), ConfigError> {
    let Some(queue) = &pool.queue else {
        return Ok(());
    };
    let invalid = |field, detail| ConfigError::InvalidUpstreamServer {
        pool: pool.name.clone(),
        server: "<pool>".into(),
        field,
        detail,
    };
    if pool.queue_timeout_ms.is_none() {
        return Err(invalid("queue", "requires queue_timeout_ms"));
    }
    if queue.classes.len() > MAX_QUEUE_CLASSES {
        return Err(invalid("queue.classes", "must contain at most 8 classes"));
    }
    let name_ok = |name: &str| {
        (1..=32).contains(&name.len())
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
    };
    for (index, class) in queue.classes.iter().enumerate() {
        if !name_ok(&class.name)
            || queue.classes[..index]
                .iter()
                .any(|other| other.name == class.name)
        {
            return Err(invalid(
                "queue.classes",
                "class names must be unique, 1 through 32 bytes of a-z, 0-9, `_` or `-`",
            ));
        }
        if !(1..=100).contains(&class.weight) {
            return Err(invalid(
                "queue.classes",
                "class weight must be between 1 and 100",
            ));
        }
    }
    if let Some(default) = &queue.default_class
        && !queue.classes.iter().any(|class| &class.name == default)
    {
        return Err(invalid(
            "queue.default_class",
            "must name one of the classes",
        ));
    }
    for (field, header) in [
        ("queue.class_header", &queue.class_header),
        ("queue.tenant_header", &queue.tenant_header),
    ] {
        if header.as_ref().is_some_and(|name| {
            name.len() > 64 || http::HeaderName::from_bytes(name.as_bytes()).is_err()
        }) {
            return Err(invalid(field, "must be a header name of at most 64 bytes"));
        }
    }
    if queue.class_header.is_some() && queue.classes.is_empty() {
        return Err(invalid("queue.class_header", "requires classes"));
    }
    if !(1..=1_024).contains(&queue.max_tenants) {
        return Err(invalid("queue.max_tenants", "must be between 1 and 1024"));
    }
    match queue.aging_ms {
        Some(_) if queue.discipline != crate::model::QueueDiscipline::Strict => Err(invalid(
            "queue.aging_ms",
            "applies only to the strict discipline",
        )),
        Some(aging) if !(100..=MAX_BREAKER_MS).contains(&aging) => Err(invalid(
            "queue.aging_ms",
            "must be between 100 and 3600000 milliseconds",
        )),
        _ => Ok(()),
    }
}

fn validate_pool_registration(pool: &UpstreamPool) -> Result<(), ConfigError> {
    let Some(registration) = &pool.registration else {
        return Ok(());
    };
    let invalid = |field, detail| ConfigError::InvalidUpstreamServer {
        pool: pool.name.clone(),
        server: "<pool>".into(),
        field,
        detail,
    };
    if !(1..=1024).contains(&registration.max_servers) {
        return Err(invalid(
            "registration.max_servers",
            "must be between 1 and 1024",
        ));
    }
    if registration.min_ttl_ms == 0
        || registration.min_ttl_ms > registration.default_ttl_ms
        || registration.default_ttl_ms > registration.max_ttl_ms
        || registration.max_ttl_ms > MAX_HTTP_TIMEOUT_MS * 7
    {
        return Err(invalid(
            "registration.default_ttl_ms",
            "must satisfy 0 < min_ttl_ms <= default_ttl_ms <= max_ttl_ms <= 604800000",
        ));
    }
    if registration
        .allowed_cidrs
        .iter()
        .any(|cidr| cidr_contains(cidr, IpAddr::from([0, 0, 0, 0])).is_none())
    {
        return Err(invalid(
            "registration.allowed_cidrs",
            "entries must be `address/prefix`",
        ));
    }
    Ok(())
}

fn validate_pool_resilience(pool: &UpstreamPool) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidUpstreamServer {
        pool: pool.name.clone(),
        server: "<pool>".into(),
        field,
        detail,
    };
    let resilience = &pool.resilience;
    if resilience
        .min_members
        .is_some_and(|members| usize::from(members) > pool.servers.len())
    {
        return Err(invalid(
            "resilience.min_members",
            "must not exceed the number of servers",
        ));
    }
    if resilience.ejection_jitter_percent > MAX_EJECTION_JITTER_PERCENT {
        return Err(invalid(
            "resilience.ejection_jitter_percent",
            "must not exceed 50",
        ));
    }
    validate_optional_safe_limit(
        "upstream pool",
        &pool.name,
        "resilience.queue_while_unavailable_ms",
        resilience.queue_while_unavailable_ms,
    )?;
    if resilience
        .queue_while_unavailable_ms
        .is_some_and(|wait| wait > MAX_HTTP_TIMEOUT_MS)
    {
        return Err(invalid(
            "resilience.queue_while_unavailable_ms",
            "must not exceed 86400000 milliseconds",
        ));
    }
    if resilience
        .unavailable_retry_after_seconds
        .is_some_and(|seconds| !(1..=86_400).contains(&seconds))
    {
        return Err(invalid(
            "resilience.unavailable_retry_after_seconds",
            "must be between 1 and 86400",
        ));
    }
    if resilience.unavailable_body.as_ref().is_some_and(|body| {
        body.is_empty() || body.len() > MAX_UNAVAILABLE_BODY_BYTES || body.contains('\0')
    }) {
        return Err(invalid(
            "resilience.unavailable_body",
            "must be 1 to 1024 bytes without NUL",
        ));
    }
    Ok(())
}

fn validate_upstream_algorithm(pool: &UpstreamPool) -> Result<(), ConfigError> {
    let UpstreamAlgorithm::WeightedRoundRobin { weights } = &pool.algorithm else {
        return Ok(());
    };
    if weights.len() != pool.servers.len() {
        return Err(ConfigError::InvalidUpstreamWeights {
            pool: pool.name.clone(),
            detail: "must contain exactly one weight per server",
        });
    }
    if weights
        .iter()
        .any(|weight| !(1..=MAX_UPSTREAM_WEIGHT).contains(weight))
    {
        return Err(ConfigError::InvalidUpstreamWeights {
            pool: pool.name.clone(),
            detail: "each weight must be between 1 and 100",
        });
    }
    Ok(())
}

fn validate_upstream_servers(
    pool: &UpstreamPool,
    management_bind: Option<SocketAddr>,
) -> Result<(), ConfigError> {
    if pool.servers.is_empty() && pool.registration.is_none() {
        return Err(ConfigError::EmptyUpstreamEndpoints {
            pool: pool.name.clone(),
        });
    }
    validate_names(
        "upstream server",
        pool.servers.iter().map(|server| server.name.as_str()),
    )?;
    let mut endpoints = HashSet::with_capacity(pool.servers.len());
    for server in &pool.servers {
        validate_upstream_endpoint(
            &pool.name,
            &server.endpoint,
            management_bind,
            &mut endpoints,
        )?;
        validate_optional_safe_limit(
            "upstream server",
            &server.name,
            "max_connections",
            server.max_connections,
        )?;
        if server.dns_resolution == DnsResolutionPolicy::Startup
            && !matches!(server.endpoint, UpstreamEndpoint::Dns { .. })
        {
            return Err(ConfigError::InvalidUpstreamServer {
                pool: pool.name.clone(),
                server: server.name.clone(),
                field: "dns_resolution",
                detail: "startup resolution applies only to DNS endpoints",
            });
        }
    }
    Ok(())
}

fn validate_upstream_pool_timeouts(pool: &UpstreamPool) -> Result<(), ConfigError> {
    for (field, timeout) in [
        ("queue_timeout_ms", pool.queue_timeout_ms),
        ("connect_timeout_ms", pool.connect_timeout_ms),
        ("server_timeout_ms", pool.server_timeout_ms),
    ] {
        validate_optional_safe_limit("upstream pool", &pool.name, field, timeout)?;
        if timeout.is_some_and(|value| value > MAX_HTTP_TIMEOUT_MS) {
            return Err(ConfigError::InvalidUpstreamServer {
                pool: pool.name.clone(),
                server: "<pool>".into(),
                field,
                detail: "timeout must not exceed 86400000 milliseconds",
            });
        }
    }
    Ok(())
}

fn validate_upstream_endpoint(
    pool: &str,
    endpoint: &UpstreamEndpoint,
    management_bind: Option<SocketAddr>,
    endpoints: &mut HashSet<UpstreamEndpoint>,
) -> Result<(), ConfigError> {
    let mut endpoint = endpoint.clone();
    normalize_upstream_endpoint(pool, &mut endpoint)?;
    match &endpoint {
        UpstreamEndpoint::Socket { address } if address.port() == 0 => {
            return Err(ConfigError::ZeroPort {
                kind: "upstream pool",
                name: pool.into(),
                field: "endpoints",
            });
        }
        UpstreamEndpoint::Dns { port: 0, .. } => {
            return Err(ConfigError::ZeroPort {
                kind: "upstream pool",
                name: pool.into(),
                field: "endpoints",
            });
        }
        UpstreamEndpoint::Dns { host, .. } if !is_valid_dns_name(host) => {
            return Err(ConfigError::InvalidDnsEndpoint {
                pool: pool.into(),
                host: host.clone(),
            });
        }
        UpstreamEndpoint::Socket { .. }
        | UpstreamEndpoint::Dns { .. }
        | UpstreamEndpoint::Unix { .. } => {}
    }
    if !endpoints.insert(endpoint.clone()) {
        return Err(ConfigError::DuplicateUpstreamEndpoint {
            pool: pool.into(),
            endpoint,
        });
    }
    if let UpstreamEndpoint::Socket { address } = endpoint
        && management_bind
            .is_some_and(|management| endpoint_exposes_management(address, management))
    {
        return Err(ConfigError::ManagementUpstreamEndpoint {
            pool: pool.into(),
            endpoint: address,
        });
    }
    Ok(())
}

fn validate_upstream_pool_cardinality(upstream_pools: &[UpstreamPool]) -> Result<(), ConfigError> {
    let total_endpoints = upstream_pools
        .iter()
        .try_fold(0_usize, |total, pool| total.checked_add(pool.servers.len()));
    if total_endpoints.is_none_or(|total| total > MAX_TOTAL_ENDPOINTS) {
        return Err(ConfigError::TooManyTotalUpstreamEndpoints);
    }
    for pool in upstream_pools {
        if pool.servers.len() > MAX_ENDPOINTS_PER_POOL {
            return Err(ConfigError::TooManyUpstreamEndpoints {
                pool: pool.name.clone(),
            });
        }
    }

    Ok(())
}
