use std::collections::{HashMap, HashSet};

use http::{
    HeaderValue,
    header::HeaderName,
    uri::{Authority, PathAndQuery},
};

use crate::{
    defaults::{
        MAX_HTTP_ACCESS_REALM_BYTES, MAX_HTTP_AUTHORITY_BYTES, MAX_HTTP_COOKIE_PATH_BYTES,
        MAX_HTTP_COOKIE_PATH_REWRITES, MAX_HTTP_FIXED_RESPONSE_BODY_BYTES,
        MAX_HTTP_HEADER_MUTATIONS, MAX_HTTP_HEADER_NAME_BYTES, MAX_HTTP_HEADER_VALUE_BYTES,
        MAX_HTTP_LITERAL_HEADERS, MAX_HTTP_METHOD_BYTES, MAX_HTTP_METHODS_PER_ROUTE,
        MAX_HTTP_REDIRECT_LOCATION_BYTES, MAX_HTTP_RETRIES, MAX_HTTP_STATIC_FALLBACK_BYTES,
        MAX_HTTP_STATIC_INDEX_BYTES, MAX_HTTP_STATIC_INDEX_FILES, MAX_SAFE_JSON_INTEGER,
    },
    lexical::{
        authority_has_invalid_port, canonicalize_http_path, is_uppercase_http_token,
        normalize_absolute_directory, normalize_host, validate_file_path, validate_relative_path,
    },
    model::{
        ConfigError, HttpAccessPolicy, HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader,
        HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
        HttpService, HttpUpstreamHost, UpstreamEndpoint, UpstreamPool,
    },
};

pub(crate) fn validate_http_services(
    services: &mut [HttpService],
    upstream_pools: &[UpstreamPool],
    cache_stores: &HashMap<String, crate::cache_validation::CacheStoreBounds>,
) -> Result<(), ConfigError> {
    let pools = upstream_pools
        .iter()
        .map(|pool| (pool.name.as_str(), pool))
        .collect::<HashMap<_, _>>();
    for service in services {
        if service.routes.is_empty() {
            return Err(ConfigError::EmptyHttpRoutes {
                service: service.name.clone(),
            });
        }
        validate_service_limits(service)?;

        let mut matchers = HashMap::with_capacity(service.routes.len());
        for (route_index, route) in service.routes.iter_mut().enumerate() {
            validate_matcher(&service.name, route_index, route)?;
            let matcher = (
                route.host.clone(),
                route.path.clone(),
                route.methods.clone(),
            );
            if let Some(first_route) = matchers.insert(matcher, route_index) {
                return Err(ConfigError::DuplicateHttpRoute {
                    service: service.name.clone(),
                    first_route,
                    duplicate_route: route_index,
                });
            }
            validate_access_policy(&service.name, route_index, &mut route.access_policy)?;
            validate_action(
                &service.name,
                route_index,
                &mut route.action,
                &pools,
                cache_stores,
            )?;
        }
    }
    Ok(())
}

fn validate_service_limits(service: &HttpService) -> Result<(), ConfigError> {
    if service.upstream_io_timeout_ms == 0 {
        return Err(ConfigError::ZeroLimit {
            kind: "HTTP service",
            name: service.name.clone(),
            field: "upstream_io_timeout_ms",
        });
    }
    if service.max_request_body_bytes == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind: "HTTP service",
            name: service.name.clone(),
            field: "max_request_body_bytes",
        });
    }
    if service.upstream_io_timeout_ms > MAX_SAFE_JSON_INTEGER {
        return Err(ConfigError::LimitTooLarge {
            kind: "HTTP service",
            name: service.name.clone(),
            field: "upstream_io_timeout_ms",
        });
    }
    if service
        .max_request_body_bytes
        .is_some_and(|value| value > MAX_SAFE_JSON_INTEGER)
    {
        return Err(ConfigError::LimitTooLarge {
            kind: "HTTP service",
            name: service.name.clone(),
            field: "max_request_body_bytes",
        });
    }
    Ok(())
}

fn validate_matcher(
    service: &str,
    route_index: usize,
    route: &mut HttpRoute,
) -> Result<(), ConfigError> {
    if let Some(host) = &mut route.host {
        validate_host_selector(service, route_index, host)?;
    }
    let path = route.path.value_mut();
    let valid = path.starts_with('/')
        && path
            .parse::<PathAndQuery>()
            .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == path);
    if !valid {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "must be an absolute path without query or fragment",
        ));
    }
    let Some(canonical) = canonicalize_http_path(path) else {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "must have one unambiguous path interpretation",
        ));
    };
    *path = canonical.into_owned();

    if route.methods.len() > MAX_HTTP_METHODS_PER_ROUTE {
        return Err(invalid_route(
            service,
            route_index,
            "methods",
            "must contain at most 16 methods",
        ));
    }
    let mut methods = HashSet::with_capacity(route.methods.len());
    for method in &mut route.methods {
        method.make_ascii_uppercase();
        if method.len() > MAX_HTTP_METHOD_BYTES || !is_uppercase_http_token(method) {
            return Err(invalid_route(
                service,
                route_index,
                "methods",
                "each method must be an HTTP token of at most 32 bytes",
            ));
        }
        if !methods.insert(method.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                "methods",
                format!("contains duplicate method `{method}`"),
            ));
        }
    }
    route.methods.sort_unstable();
    Ok(())
}

fn validate_host_selector(
    service: &str,
    route_index: usize,
    selector: &mut HttpHostSelector,
) -> Result<(), ConfigError> {
    match selector {
        HttpHostSelector::NormalizedHost { value } => {
            let original = value.clone();
            let mut host = if original.parse::<std::net::IpAddr>().is_ok() {
                original
            } else {
                let authority = parse_authority(&original)
                    .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
                authority
                    .host()
                    .strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or_else(|| authority.host())
                    .to_owned()
            };
            if !normalize_host(&mut host) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "normalized_host must be an exact DNS name, one-label wildcard, or IP address",
                ));
            }
            *value = host;
        }
        HttpHostSelector::ExactAuthority { value } => {
            if value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('*') {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "exact_authority must be a non-wildcard authority of at most 255 bytes",
                ));
            }
            parse_authority(value)
                .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
        }
    }
    Ok(())
}

fn parse_authority(value: &str) -> Result<Authority, &'static str> {
    if value.is_empty() || value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('@') {
        return Err("must be a bounded authority without userinfo");
    }
    let authority = value
        .parse::<Authority>()
        .map_err(|_| "must be a valid authority")?;
    if authority.host().is_empty() || authority_has_invalid_port(&authority) {
        return Err("must contain a host and an optional numeric port");
    }
    Ok(authority)
}

fn validate_access_policy(
    service: &str,
    route_index: usize,
    policy: &mut Option<HttpAccessPolicy>,
) -> Result<(), ConfigError> {
    let Some(HttpAccessPolicy::BearerTokenFile {
        token_file_path,
        header_name,
        realm,
    }) = policy.as_mut()
    else {
        return Ok(());
    };
    if token_file_path
        .to_str()
        .is_some_and(|path| path.bytes().any(|byte| byte.is_ascii_control()))
    {
        return Err(invalid_route(
            service,
            route_index,
            "access_policy.token_file_path",
            "must not contain control bytes",
        ));
    }
    let identity = format!("{service} route {route_index}");
    validate_file_path(
        "HTTP access policy",
        &identity,
        "token_file_path",
        token_file_path,
    )?;
    normalize_header_name(header_name).map_err(|detail| {
        invalid_route(service, route_index, "access_policy.header_name", detail)
    })?;
    if let Some(realm) = realm {
        let valid = !realm.is_empty()
            && realm.len() <= MAX_HTTP_ACCESS_REALM_BYTES
            && realm.is_ascii()
            && realm.bytes().all(|byte| {
                byte == b' ' || (byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
            });
        if !valid {
            return Err(invalid_route(
                service,
                route_index,
                "access_policy.realm",
                "must be 1..=128 safe ASCII bytes without quotes or backslashes",
            ));
        }
    }
    Ok(())
}

fn validate_action(
    service: &str,
    route_index: usize,
    action: &mut HttpRouteAction,
    pools: &HashMap<&str, &UpstreamPool>,
    cache_stores: &HashMap<String, crate::cache_validation::CacheStoreBounds>,
) -> Result<(), ConfigError> {
    match action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => {
            let pool = pools.get(upstream_pool.as_str()).ok_or_else(|| {
                ConfigError::UnknownRouteUpstreamPool {
                    service: service.into(),
                    route: route_index,
                    pool: upstream_pool.clone(),
                }
            })?;
            validate_proxy_policy(service, route_index, policy, pool, cache_stores)
        }
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => validate_fixed_response(service, route_index, *status, body, headers),
        HttpRouteAction::Redirect { status, location } => {
            validate_redirect(service, route_index, *status, location)
        }
        HttpRouteAction::StaticFiles {
            root_directory,
            index_files,
            spa_fallback,
        } => validate_static_files(
            service,
            route_index,
            root_directory,
            index_files,
            spa_fallback.as_deref(),
        ),
    }
}

fn validate_proxy_policy(
    service: &str,
    route_index: usize,
    policy: &mut HttpProxyPolicy,
    pool: &UpstreamPool,
    cache_stores: &HashMap<String, crate::cache_validation::CacheStoreBounds>,
) -> Result<(), ConfigError> {
    match &mut policy.upstream_host {
        HttpUpstreamHost::PreserveIncoming => {}
        HttpUpstreamHost::Endpoint { unix_fallback } => {
            if let Some(fallback) = unix_fallback {
                parse_authority(fallback).map_err(|detail| {
                    invalid_route(service, route_index, "action.policy.upstream_host", detail)
                })?;
            }
            if unix_fallback.is_none()
                && pool
                    .endpoints
                    .iter()
                    .any(|endpoint| matches!(endpoint, UpstreamEndpoint::Unix { .. }))
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
    validate_response_mutations(service, route_index, &mut policy.response_headers)?;
    validate_cookie_rewrites(service, route_index, &policy.response_cookie_path_rewrites)?;
    validate_retry(service, route_index, &mut policy.retry)?;
    if let Some(cache) = &mut policy.cache {
        crate::cache_validation::validate_cache_policy(service, route_index, cache, cache_stores)?;
    }
    Ok(())
}

fn validate_request_mutations(
    service: &str,
    route_index: usize,
    mutations: &mut [HttpRequestHeaderMutation],
) -> Result<(), ConfigError> {
    if mutations.len() > MAX_HTTP_HEADER_MUTATIONS {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "must contain at most 32 header mutations",
        ));
    }
    let mut names = HashSet::with_capacity(mutations.len());
    for mutation in mutations {
        normalize_header_name(mutation.name_mut()).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.request_headers",
                detail,
            )
        })?;
        if !names.insert(mutation.name().to_owned()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.request_headers",
                format!(
                    "contains duplicate or conflicting mutation for `{}`",
                    mutation.name()
                ),
            ));
        }
        if let HttpRequestHeaderMutation::Set {
            value: HttpRequestHeaderValue::Literal { value },
            ..
        } = mutation
        {
            validate_header_value(value).map_err(|detail| {
                invalid_route(
                    service,
                    route_index,
                    "action.policy.request_headers",
                    detail,
                )
            })?;
        }
    }
    Ok(())
}

fn validate_response_mutations(
    service: &str,
    route_index: usize,
    mutations: &mut [HttpResponseHeaderMutation],
) -> Result<(), ConfigError> {
    if mutations.len() > MAX_HTTP_HEADER_MUTATIONS {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_headers",
            "must contain at most 32 header mutations",
        ));
    }
    let mut names = HashSet::with_capacity(mutations.len());
    for mutation in mutations {
        normalize_header_name(mutation.name_mut()).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_headers",
                detail,
            )
        })?;
        if !names.insert(mutation.name().to_owned()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_headers",
                format!(
                    "contains duplicate or conflicting mutation for `{}`",
                    mutation.name()
                ),
            ));
        }
        if let HttpResponseHeaderMutation::Set { value, .. } = mutation {
            validate_header_value(value).map_err(|detail| {
                invalid_route(
                    service,
                    route_index,
                    "action.policy.response_headers",
                    detail,
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn normalize_header_name(name: &mut String) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > MAX_HTTP_HEADER_NAME_BYTES {
        return Err("header name must be 1..=64 bytes");
    }
    let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "header name is invalid")?;
    let normalized = header.as_str();
    if is_forbidden_header(normalized) {
        return Err("header is hop-by-hop, framing, or managed by upstream Host policy");
    }
    *name = normalized.into();
    Ok(())
}

fn is_forbidden_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn validate_header_value(value: &str) -> Result<(), &'static str> {
    if value.len() > MAX_HTTP_HEADER_VALUE_BYTES {
        return Err("header value exceeds 8192 bytes");
    }
    HeaderValue::from_str(value).map_err(|_| "header value contains invalid bytes")?;
    Ok(())
}

fn validate_cookie_rewrites(
    service: &str,
    route_index: usize,
    rewrites: &[HttpCookiePathRewrite],
) -> Result<(), ConfigError> {
    if rewrites.len() > MAX_HTTP_COOKIE_PATH_REWRITES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_cookie_path_rewrites",
            "must contain at most 16 rewrites",
        ));
    }
    let mut from_paths = HashSet::with_capacity(rewrites.len());
    for rewrite in rewrites {
        validate_cookie_path(&rewrite.from).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                detail,
            )
        })?;
        validate_cookie_path(&rewrite.to).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                detail,
            )
        })?;
        if !from_paths.insert(rewrite.from.as_str()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                format!("contains duplicate source path `{}`", rewrite.from),
            ));
        }
    }
    Ok(())
}

fn validate_cookie_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.len() > MAX_HTTP_COOKIE_PATH_BYTES || !path.starts_with('/') {
        return Err("cookie path must be an absolute value of at most 1024 bytes");
    }
    if path
        .bytes()
        .any(|byte| byte == 0 || byte == b';' || byte.is_ascii_control())
    {
        return Err("cookie path contains an invalid byte");
    }
    Ok(())
}

fn validate_retry(
    service: &str,
    route_index: usize,
    retry: &mut HttpRetryPolicy,
) -> Result<(), ConfigError> {
    if retry.max_retries > MAX_HTTP_RETRIES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.max_retries",
            "must be between 0 and 2",
        ));
    }
    if retry.triggers.is_empty() {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.triggers",
            "must not be empty",
        ));
    }
    let mut triggers = HashSet::with_capacity(retry.triggers.len());
    for trigger in &retry.triggers {
        if !triggers.insert(*trigger) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.triggers",
                "must not contain duplicates",
            ));
        }
    }
    retry
        .triggers
        .sort_unstable_by_key(|trigger| match trigger {
            HttpRetryTrigger::ConnectFailure => 0,
            HttpRetryTrigger::ConnectTimeout => 1,
            HttpRetryTrigger::RefusedStream => 2,
        });
    Ok(())
}

fn validate_fixed_response(
    service: &str,
    route_index: usize,
    status: u16,
    body: &str,
    headers: &mut [HttpLiteralHeader],
) -> Result<(), ConfigError> {
    if !(200..=599).contains(&status) {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.status",
            "must be between 200 and 599",
        ));
    }
    if body.len() > MAX_HTTP_FIXED_RESPONSE_BODY_BYTES {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.body",
            "must not exceed 65536 UTF-8 bytes",
        ));
    }
    if matches!(status, 204 | 205 | 304) && !body.is_empty() {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.body",
            format!("must be empty for body-forbidden status {status}"),
        ));
    }
    validate_literal_headers(
        service,
        route_index,
        "action.fixed_response.headers",
        headers,
    )
}

fn validate_literal_headers(
    service: &str,
    route_index: usize,
    field: &'static str,
    headers: &mut [HttpLiteralHeader],
) -> Result<(), ConfigError> {
    if headers.len() > MAX_HTTP_LITERAL_HEADERS {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "must contain at most 32 headers",
        ));
    }
    let mut names = HashSet::with_capacity(headers.len());
    for header in headers {
        normalize_header_name(&mut header.name)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
        validate_header_value(&header.value)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
        if !names.insert(header.name.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                field,
                format!("contains duplicate header `{}`", header.name),
            ));
        }
    }
    Ok(())
}

fn validate_redirect(
    service: &str,
    route_index: usize,
    status: u16,
    location: &HttpRedirectLocation,
) -> Result<(), ConfigError> {
    if !matches!(status, 301 | 302 | 307 | 308) {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.status",
            "must be 301, 302, 307, or 308",
        ));
    }
    let (value, request_template) = match location {
        HttpRedirectLocation::Literal { value } => (value, false),
        HttpRedirectLocation::RequestTemplate { value } => (value, true),
    };
    if value.is_empty() || value.len() > MAX_HTTP_REDIRECT_LOCATION_BYTES {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.location",
            "must be 1..=2048 bytes",
        ));
    }
    validate_header_value(value).map_err(|detail| {
        invalid_route(service, route_index, "action.redirect.location", detail)
    })?;
    if request_template && !is_valid_location_template(value) {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.location",
            "supports only $scheme, $host, and $request_uri variables",
        ));
    }
    Ok(())
}

fn is_valid_location_template(template: &str) -> bool {
    let mut remainder = template;
    while let Some((_, after_dollar)) = remainder.split_once('$') {
        let Some(variable) = ["scheme", "host", "request_uri"]
            .into_iter()
            .find(|variable| after_dollar.starts_with(variable))
        else {
            return false;
        };
        remainder = &after_dollar[variable.len()..];
    }
    true
}

fn validate_static_files(
    service: &str,
    route_index: usize,
    root_directory: &mut std::path::PathBuf,
    index_files: &[String],
    spa_fallback: Option<&std::path::Path>,
) -> Result<(), ConfigError> {
    normalize_absolute_directory(root_directory).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.static_files.root_directory",
            detail,
        )
    })?;
    if index_files.len() > MAX_HTTP_STATIC_INDEX_FILES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.index_files",
            "must contain at most 8 filenames",
        ));
    }
    for index in index_files {
        let valid = !index.is_empty()
            && index.len() <= MAX_HTTP_STATIC_INDEX_BYTES
            && index.bytes().enumerate().all(|(position, byte)| {
                byte.is_ascii_alphanumeric() || (position > 0 && matches!(byte, b'.' | b'-' | b'_'))
            });
        if !valid {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.index_files",
                "each index must be a safe relative filename of at most 255 bytes",
            ));
        }
    }
    if let Some(spa_fallback) = spa_fallback {
        validate_relative_path(spa_fallback, MAX_HTTP_STATIC_FALLBACK_BYTES).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.spa_fallback",
                detail,
            )
        })?;
    }
    Ok(())
}

fn invalid_route(
    service: &str,
    route: usize,
    field: &'static str,
    detail: impl Into<String>,
) -> ConfigError {
    ConfigError::InvalidHttpRoute {
        service: service.into(),
        route,
        field,
        detail: detail.into(),
    }
}
