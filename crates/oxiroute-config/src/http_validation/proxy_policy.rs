#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_proxy_policy(
    service: &str,
    route_index: usize,
    policy: &mut HttpProxyPolicy,
    route_policy: crate::model::HttpRoutePolicy,
    pool: &UpstreamPool,
    pools: &HashMap<&str, &UpstreamPool>,
    cache_stores: &HashMap<String, crate::cache_validation::CacheStoreBounds>,
) -> Result<(), ConfigError> {
    match &mut policy.upstream_host {
        HttpUpstreamHost::PreserveIncoming => {}
        HttpUpstreamHost::NginxHost { fallback } => {
            validate_nginx_host_fallback(fallback).map_err(|detail| {
                invalid_route(service, route_index, "action.policy.upstream_host", detail)
            })?;
        }
        HttpUpstreamHost::Endpoint { unix_fallback } => {
            if let Some(fallback) = unix_fallback {
                parse_authority(fallback).map_err(|detail| {
                    invalid_route(service, route_index, "action.policy.upstream_host", detail)
                })?;
            }
            if unix_fallback.is_none()
                && pool
                    .servers
                    .iter()
                    .any(|server| matches!(server.endpoint, UpstreamEndpoint::Unix { .. }))
            {
                return Err(ConfigError::HttpEndpointHostRequiresUnixFallback {
                    service: service.into(),
                    route: route_index,
                });
            }
        }
        HttpUpstreamHost::Literal { value } => {
            parse_authority(value).map_err(|detail| {
                invalid_route(service, route_index, "action.policy.upstream_host", detail)
            })?;
        }
    }
    validate_request_mutations(service, route_index, &mut policy.request_headers)?;
    validate_proxy_path_rewrite(service, route_index, &mut policy.upstream_path_rewrite)?;
    validate_response_mutations(service, route_index, &mut policy.response_headers)?;
    validate_cookie_rewrites(service, route_index, &policy.response_cookie_path_rewrites)?;
    validate_cookie_attributes(service, route_index, &mut policy.response_cookie_attributes)?;
    validate_retry(service, route_index, &mut policy.retry, route_policy)?;
    if policy.retry.budget.is_some() && pool.http_versions.max == crate::model::HttpVersion::Http3 {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.budget",
            "is not supported for HTTP/3 upstream pools",
        ));
    }
    validate_proxy_timeouts(service, route_index, policy.timeouts)?;
    validate_rescue(service, route_index, policy, pool, pools)?;
    validate_select(service, route_index, policy, pool, pools)?;
    validate_affinity(service, route_index, policy, pool)?;
    if let Some(cache) = &mut policy.cache {
        crate::cache_validation::validate_cache_policy(service, route_index, cache, cache_stores)?;
    }
    Ok(())
}

fn validate_proxy_path_rewrite(
    service: &str,
    route_index: usize,
    rewrite: &mut Option<HttpProxyPathRewrite>,
) -> Result<(), ConfigError> {
    let Some(rewrite) = rewrite else {
        return Ok(());
    };
    for path in [&mut rewrite.from, &mut rewrite.to] {
        if path.len() > MAX_HTTP_PROXY_PATH_BYTES || !path.starts_with('/') {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.upstream_path_rewrite",
                "must be an absolute path of at most 1024 bytes",
            ));
        }
        let valid = path
            .parse::<PathAndQuery>()
            .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == path);
        if !valid {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.upstream_path_rewrite",
                "must be an absolute path without query or fragment",
            ));
        }
        let Some(canonical) = canonicalize_http_path(path) else {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.upstream_path_rewrite",
                "must have one unambiguous path interpretation",
            ));
        };
        *path = canonical.into_owned();
    }
    Ok(())
}

fn validate_rescue(
    service: &str,
    route_index: usize,
    policy: &mut HttpProxyPolicy,
    pool: &UpstreamPool,
    pools: &HashMap<&str, &UpstreamPool>,
) -> Result<(), ConfigError> {
    let Some(rescue) = &mut policy.rescue else {
        return Ok(());
    };
    let invalid =
        |detail: &'static str| invalid_route(service, route_index, "action.policy.rescue", detail);
    if rescue.statuses.is_empty()
        || rescue.statuses.len() > crate::defaults::MAX_HTTP_RETRY_RESPONSE_STATUSES
    {
        return Err(invalid("statuses must contain between 1 and 100 statuses"));
    }
    if rescue
        .statuses
        .iter()
        .any(|status| !(100..=599).contains(status))
    {
        return Err(invalid("statuses must be between 100 and 599"));
    }
    rescue.statuses.sort_unstable();
    if rescue.statuses.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid("statuses must not contain duplicates"));
    }
    if policy.retry.max_retries == 0 {
        return Err(invalid("requires retry.max_retries of at least 1"));
    }
    let target_pool = match (rescue.target, &rescue.pool) {
        (HttpRescueTarget::NextServer, None) => None,
        (HttpRescueTarget::Pool, Some(name)) => Some(pools.get(name.as_str()).ok_or_else(
            || ConfigError::UnknownRouteUpstreamPool {
                service: service.into(),
                route: route_index,
                pool: name.clone(),
            },
        )?),
        _ => return Err(invalid("pool is required exactly when target is `pool`")),
    };
    if target_pool.is_some_and(|target| target.name == pool.name) {
        return Err(invalid("pool must differ from the route pool"));
    }
    let http3 = |pool: &UpstreamPool| pool.http_versions.max == crate::model::HttpVersion::Http3;
    if http3(pool) || target_pool.is_some_and(|target| http3(target)) {
        return Err(invalid("is not supported for HTTP/3 upstream pools"));
    }
    Ok(())
}

fn validate_select(
    service: &str,
    route_index: usize,
    policy: &mut HttpProxyPolicy,
    pool: &UpstreamPool,
    pools: &HashMap<&str, &UpstreamPool>,
) -> Result<(), ConfigError> {
    let cached = policy.cache.is_some();
    let Some(select) = &mut policy.select else {
        return Ok(());
    };
    let invalid =
        |detail: &'static str| invalid_route(service, route_index, "action.policy.select", detail);
    normalize_header_name(&mut select.header).map_err(invalid)?;
    if select.rules.is_empty() || select.rules.len() > crate::defaults::MAX_HTTP_SELECT_RULES {
        return Err(invalid("rules must contain between 1 and 16 entries"));
    }
    if select
        .rules
        .windows(2)
        .any(|pair| pair[0].at_most >= pair[1].at_most)
    {
        return Err(invalid("rules must have strictly ascending at_most bounds"));
    }
    if cached {
        return Err(invalid(
            "cannot be combined with cache: entries are not keyed by pool",
        ));
    }
    let http3 = |pool: &UpstreamPool| pool.http_versions.max == crate::model::HttpVersion::Http3;
    for rule in &select.rules {
        let target =
            pools
                .get(rule.pool.as_str())
                .ok_or_else(|| ConfigError::UnknownRouteUpstreamPool {
                    service: service.into(),
                    route: route_index,
                    pool: rule.pool.clone(),
                })?;
        if http3(target) {
            return Err(invalid("is not supported for HTTP/3 upstream pools"));
        }
    }
    if http3(pool) {
        return Err(invalid("is not supported for HTTP/3 upstream pools"));
    }
    Ok(())
}

fn validate_affinity(
    service: &str,
    route_index: usize,
    policy: &mut HttpProxyPolicy,
    pool: &UpstreamPool,
) -> Result<(), ConfigError> {
    let cached = policy.cache.is_some();
    let Some(affinity) = &mut policy.affinity else {
        return Ok(());
    };
    let invalid = |detail: &'static str| {
        invalid_route(service, route_index, "action.policy.affinity", detail)
    };
    if pool.http_versions.max == crate::model::HttpVersion::Http3 {
        return Err(invalid("is not supported for HTTP/3 upstream pools"));
    }
    match affinity {
        HttpAffinity::Cookie {
            name,
            ttl_s,
            same_site,
            secure,
            ..
        } => {
            let token =
                |byte: u8| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte);
            if name.is_empty() || name.len() > 64 || !name.bytes().all(token) {
                return Err(invalid("cookie name must be a 1..=64 byte token"));
            }
            if !(1..=crate::defaults::MAX_AFFINITY_TTL_S).contains(ttl_s) {
                return Err(invalid("ttl_s must be between 1 and 31536000"));
            }
            if *same_site == crate::model::HttpSameSite::None && !*secure {
                return Err(invalid("same_site none requires secure"));
            }
            if cached {
                return Err(invalid("a cookie cannot be combined with cache"));
            }
        }
        HttpAffinity::Hash { key } => {
            let mut parsed = HttpAffinityKey::parse(key).map_err(invalid)?;
            if let HttpAffinityKey::Header(name) = &mut parsed {
                normalize_header_name(name).map_err(invalid)?;
            }
            *key = parsed.to_string();
        }
        HttpAffinity::Header { name } => normalize_header_name(name).map_err(invalid)?,
    }
    Ok(())
}

fn validate_proxy_timeouts(
    service: &str,
    route_index: usize,
    timeouts: HttpProxyTimeouts,
) -> Result<(), ConfigError> {
    for (field, timeout) in [
        ("action.policy.timeouts.connect_ms", timeouts.connect_ms),
        ("action.policy.timeouts.header_ms", timeouts.header_ms),
        ("action.policy.timeouts.idle_ms", timeouts.idle_ms),
        ("action.policy.timeouts.total_ms", timeouts.total_ms),
    ] {
        if timeout.is_some_and(|value| value == 0 || value > MAX_HTTP_TIMEOUT_MS) {
            return Err(invalid_route(
                service,
                route_index,
                field,
                "must be null or between 1 and 86400000 milliseconds",
            ));
        }
    }
    let phase_over_total = timeouts.total_ms.is_some_and(|total| {
        [timeouts.connect_ms, timeouts.header_ms, timeouts.idle_ms]
            .into_iter()
            .flatten()
            .any(|phase| phase > total)
    });
    if phase_over_total {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.timeouts.total_ms",
            "must not be shorter than the connect, header or idle timeout",
        ));
    }
    Ok(())
}
