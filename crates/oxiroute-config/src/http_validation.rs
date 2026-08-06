use std::collections::{HashMap, HashSet};

use http::{
    HeaderValue,
    header::HeaderName,
    uri::{Authority, PathAndQuery},
};

use crate::{
    defaults::{
        MAX_HTTP_ACCESS_REALM_BYTES, MAX_HTTP_AUTHORITY_BYTES, MAX_HTTP_COOKIE_ATTRIBUTE_RULES,
        MAX_HTTP_COOKIE_PATH_BYTES, MAX_HTTP_COOKIE_PATH_REWRITES, MAX_HTTP_FILE_EXTENSION_BYTES,
        MAX_HTTP_FIXED_RESPONSE_BODY_BYTES, MAX_HTTP_GZIP_TYPES, MAX_HTTP_HEADER_MUTATIONS,
        MAX_HTTP_HEADER_NAME_BYTES, MAX_HTTP_HEADER_VALUE_BYTES, MAX_HTTP_LITERAL_HEADERS,
        MAX_HTTP_METHOD_BYTES, MAX_HTTP_METHODS_PER_ROUTE, MAX_HTTP_MIME_TYPE_BYTES,
        MAX_HTTP_PROXY_PATH_BYTES, MAX_HTTP_REDIRECT_LOCATION_BYTES, MAX_HTTP_RETRIES,
        MAX_HTTP_STATIC_ERROR_RESPONSES, MAX_HTTP_STATIC_ERROR_STATUSES,
        MAX_HTTP_STATIC_FALLBACK_BYTES, MAX_HTTP_STATIC_INDEX_BYTES, MAX_HTTP_STATIC_INDEX_FILES,
        MAX_HTTP_STATIC_MIME_TYPES, MAX_HTTP_STATIC_TRY_FILES, MAX_HTTP_TIMEOUT_MS,
        MAX_SAFE_JSON_INTEGER,
    },
    lexical::{
        authority_has_invalid_port, canonicalize_http_path, is_uppercase_http_token,
        is_valid_dns_name, normalize_absolute_directory, normalize_host, validate_file_path,
        validate_relative_path,
    },
    model::{
        AccessLogPolicy, ConfigError, HttpAccessPolicy, HttpCookieAttributePolicy,
        HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader, HttpPathSelector,
        HttpProxyPathRewrite, HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation,
        HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger,
        HttpRoute, HttpRouteAction, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
        HttpStaticTryFile, HttpUpstreamHost, UpstreamEndpoint, UpstreamPool,
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
            if route.policy == crate::model::HttpRoutePolicy::default() {
                route.policy.max_request_body_bytes = service.max_request_body_bytes;
                route.policy.connect_timeout_ms = service.upstream_io_timeout_ms;
                route.policy.read_timeout_ms = service.upstream_io_timeout_ms;
                route.policy.write_timeout_ms = service.upstream_io_timeout_ms;
            }
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
            validate_route_policy(&service.name, route_index, &route.policy)?;
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
    if let Some(gzip) = &service.gzip {
        if !(1..=9).contains(&gzip.level) {
            return Err(ConfigError::InvalidHttpRoute {
                service: service.name.clone(),
                route: 0,
                field: "gzip.level",
                detail: "must be between 1 and 9".into(),
            });
        }
        if gzip.min_length_bytes > MAX_SAFE_JSON_INTEGER {
            return Err(ConfigError::InvalidHttpRoute {
                service: service.name.clone(),
                route: 0,
                field: "gzip.min_length_bytes",
                detail: "must not exceed the maximum safe JSON integer".into(),
            });
        }
        if gzip.content_types.len() > MAX_HTTP_GZIP_TYPES {
            return Err(ConfigError::InvalidHttpRoute {
                service: service.name.clone(),
                route: 0,
                field: "gzip.content_types",
                detail: "must contain at most 64 values".into(),
            });
        }
        let mut types = HashSet::with_capacity(gzip.content_types.len());
        for content_type in &gzip.content_types {
            validate_content_type(content_type).map_err(|detail| {
                ConfigError::InvalidHttpRoute {
                    service: service.name.clone(),
                    route: 0,
                    field: "gzip.content_types",
                    detail: detail.into(),
                }
            })?;
            if !types.insert(content_type) {
                return Err(ConfigError::InvalidHttpRoute {
                    service: service.name.clone(),
                    route: 0,
                    field: "gzip.content_types",
                    detail: "must not contain duplicates".into(),
                });
            }
        }
    }
    if let Some(AccessLogPolicy::File { path }) = &service.access_log {
        validate_file_path("HTTP service", &service.name, "access_log.path", path)?;
    }
    Ok(())
}

fn validate_route_policy(
    service: &str,
    route_index: usize,
    policy: &crate::model::HttpRoutePolicy,
) -> Result<(), ConfigError> {
    if policy.max_request_body_bytes == Some(0) {
        return Err(invalid_route(
            service,
            route_index,
            "policy.max_request_body_bytes",
            "must be null or a positive exact JSON integer",
        ));
    }
    if policy.response_buffering && policy.max_request_body_bytes.is_none() {
        return Err(invalid_route(
            service,
            route_index,
            "policy.response_buffering",
            "requires a positive policy.max_request_body_bytes limit",
        ));
    }
    if policy
        .max_request_body_bytes
        .is_some_and(|value| value > MAX_SAFE_JSON_INTEGER)
    {
        return Err(invalid_route(
            service,
            route_index,
            "policy.max_request_body_bytes",
            "exceeds the exact JSON integer limit",
        ));
    }
    for (field, timeout) in [
        ("policy.connect_timeout_ms", policy.connect_timeout_ms),
        ("policy.read_timeout_ms", policy.read_timeout_ms),
        ("policy.write_timeout_ms", policy.write_timeout_ms),
    ] {
        if timeout == 0 || timeout > MAX_HTTP_TIMEOUT_MS {
            return Err(invalid_route(
                service,
                route_index,
                field,
                "must be between 1 and 86400000 milliseconds",
            ));
        }
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
    let requires_ascii = matches!(
        route.path,
        HttpPathSelector::AsciiCaseInsensitiveExact { .. }
    );
    let path = route.path.value_mut();
    if requires_ascii && !path.is_ascii() {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "ASCII case-insensitive exact matching requires an ASCII path",
        ));
    }
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
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            if !value.is_ascii() || value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('*') {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "ascii_case_insensitive_exact_authority must be a non-wildcard ASCII authority of at most 255 bytes",
                ));
            }
            parse_authority(value)
                .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
            value.make_ascii_lowercase();
        }
        HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => {
            value.make_ascii_lowercase();
            if !is_valid_dns_name(value) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "nginx wildcard suffix must be a canonical DNS name",
                ));
            }
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
    let Some(policy) = policy.as_mut() else {
        return Ok(());
    };
    let identity = format!("{service} route {route_index}");
    match policy {
        HttpAccessPolicy::BearerTokenFile {
            token_file_path,
            header_name,
            realm,
        } => {
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
                validate_realm(realm).map_err(|detail| {
                    invalid_route(service, route_index, "access_policy.realm", detail)
                })?;
            }
        }
        HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path,
            realm,
        } => {
            validate_file_path(
                "HTTP access policy",
                &identity,
                "htpasswd_file_path",
                htpasswd_file_path,
            )?;
            validate_realm(realm).map_err(|detail| {
                invalid_route(service, route_index, "access_policy.realm", detail)
            })?;
        }
    }
    Ok(())
}

pub(crate) fn validate_realm(realm: &str) -> Result<(), &'static str> {
    let valid = !realm.is_empty()
        && realm.len() <= MAX_HTTP_ACCESS_REALM_BYTES
        && realm.is_ascii()
        && realm
            .bytes()
            .all(|byte| byte == b' ' || (byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\')));
    if valid {
        Ok(())
    } else {
        Err("must be 1..=128 safe ASCII bytes without quotes or backslashes")
    }
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
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => validate_redirect(service, route_index, *status, location, headers),
        HttpRouteAction::StaticFiles {
            root_directory,
            index_files,
            spa_fallback,
            try_files,
            mime,
            headers,
            error_responses,
            ..
        } => validate_static_files(
            service,
            route_index,
            &mut StaticFilesValidation {
                root_directory,
                index_files,
                spa_fallback: spa_fallback.as_deref(),
                try_files,
                mime,
                headers,
                error_responses,
            },
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
    validate_retry(service, route_index, &mut policy.retry)?;
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
        validate_request_mutation(service, route_index, mutation)?;
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
    }
    Ok(())
}

fn validate_request_mutation(
    service: &str,
    route_index: usize,
    mutation: &mut HttpRequestHeaderMutation,
) -> Result<(), ConfigError> {
    normalize_header_name_syntax(mutation.name_mut()).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            detail,
        )
    })?;
    let is_x_forwarded_for = mutation.name() == "x-forwarded-for";
    if let HttpRequestHeaderMutation::Set { value, .. } = mutation {
        match value {
            HttpRequestHeaderValue::Literal { value } => {
                validate_header_value(value).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => validate_forwarded_for_value(
                service,
                route_index,
                is_x_forwarded_for,
                *max_bytes,
                except_source_cidrs,
            )?,
            HttpRequestHeaderValue::IncomingHeader { name, max_bytes } => {
                normalize_header_name_syntax(name).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
                validate_dynamic_header_bound(*max_bytes).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
            HttpRequestHeaderValue::IncomingAuthority
            | HttpRequestHeaderValue::NormalizedHost
            | HttpRequestHeaderValue::ClientIp
            | HttpRequestHeaderValue::DownstreamScheme
            | HttpRequestHeaderValue::SelectedUpstreamHost => {}
            HttpRequestHeaderValue::NginxHost { fallback } => {
                validate_nginx_host_fallback(fallback).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
        }
    }
    if is_forbidden_header(mutation.name()) && !is_pingora_managed_upgrade_mutation(mutation) {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "header is hop-by-hop, framing, or managed by upstream Host policy",
        ));
    }
    let has_forbidden_incoming_source = match mutation {
        HttpRequestHeaderMutation::Set {
            value: HttpRequestHeaderValue::IncomingHeader { name, .. },
            ..
        } => is_forbidden_header(name),
        _ => false,
    };
    if has_forbidden_incoming_source && !is_pingora_managed_upgrade_mutation(mutation) {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "incoming header is hop-by-hop or framing",
        ));
    }
    Ok(())
}

fn validate_forwarded_for_value(
    service: &str,
    route_index: usize,
    is_x_forwarded_for: bool,
    max_bytes: u64,
    except_source_cidrs: &mut [String],
) -> Result<(), ConfigError> {
    let field = "action.policy.request_headers";
    validate_dynamic_header_bound(max_bytes)
        .map_err(|detail| invalid_route(service, route_index, field, detail))?;
    if !is_x_forwarded_for {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "appended X-Forwarded-For values require the x-forwarded-for header name",
        ));
    }
    if except_source_cidrs.len() > 16 {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "X-Forwarded-For source exceptions must contain at most 16 CIDRs",
        ));
    }
    let mut unique = HashSet::with_capacity(except_source_cidrs.len());
    for cidr in except_source_cidrs {
        *cidr = crate::forward_validation::normalize_cidr(cidr).ok_or_else(|| {
            invalid_route(
                service,
                route_index,
                field,
                format!("invalid canonical source exception CIDR `{cidr}`"),
            )
        })?;
        if !unique.insert(cidr.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                field,
                format!("duplicate source exception CIDR `{cidr}`"),
            ));
        }
    }
    Ok(())
}

fn is_pingora_managed_upgrade_mutation(mutation: &HttpRequestHeaderMutation) -> bool {
    matches!(
        mutation,
        HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::IncomingHeader { name: source, .. },
        } if name == "upgrade" && source == "upgrade"
    ) || matches!(
        mutation,
        HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::Literal { value },
        } if name == "connection" && value.eq_ignore_ascii_case("upgrade")
    )
}

fn validate_dynamic_header_bound(max_bytes: u64) -> Result<(), &'static str> {
    if max_bytes == 0 || max_bytes > MAX_HTTP_HEADER_VALUE_BYTES as u64 {
        return Err("dynamic header max_bytes must be between 1 and 8192");
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
    let mut operations = HashMap::with_capacity(mutations.len());
    for mutation in mutations {
        normalize_header_name(mutation.name_mut()).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_headers",
                detail,
            )
        })?;
        let operation = response_mutation_kind(mutation);
        if let Some(previous) = operations.get(mutation.name()) {
            let compatible = matches!(
                (*previous, operation),
                (
                    ResponseMutationKind::Add | ResponseMutationKind::Remove,
                    ResponseMutationKind::Add,
                )
            );
            if !compatible {
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
        } else {
            operations.insert(mutation.name().to_owned(), operation);
        }
        if let HttpResponseHeaderMutation::Set { value, .. }
        | HttpResponseHeaderMutation::Add { value, .. } = mutation
        {
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

#[derive(Clone, Copy)]
enum ResponseMutationKind {
    Set,
    Add,
    Remove,
}

fn response_mutation_kind(mutation: &HttpResponseHeaderMutation) -> ResponseMutationKind {
    match mutation {
        HttpResponseHeaderMutation::Set { .. } => ResponseMutationKind::Set,
        HttpResponseHeaderMutation::Add { .. } => ResponseMutationKind::Add,
        HttpResponseHeaderMutation::Remove { .. } => ResponseMutationKind::Remove,
    }
}

pub(crate) fn normalize_header_name(name: &mut String) -> Result<(), &'static str> {
    normalize_header_name_syntax(name)?;
    if is_forbidden_header(name) {
        return Err("header is hop-by-hop, framing, or managed by upstream Host policy");
    }
    Ok(())
}

fn normalize_header_name_syntax(name: &mut String) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > MAX_HTTP_HEADER_NAME_BYTES {
        return Err("header name must be 1..=64 bytes");
    }
    let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "header name is invalid")?;
    let normalized = header.as_str();
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

fn validate_cookie_attributes(
    service: &str,
    route_index: usize,
    policies: &mut [HttpCookieAttributePolicy],
) -> Result<(), ConfigError> {
    if policies.len() > MAX_HTTP_COOKIE_ATTRIBUTE_RULES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_cookie_attributes",
            "must contain at most 16 policies",
        ));
    }
    let mut names = HashSet::with_capacity(policies.len());
    for policy in policies {
        if policy.name.is_empty()
            || policy.name.len() > 256
            || policy
                .name
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'=' | b';' | b','))
        {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "cookie name must be 1..=256 bytes without separators or controls",
            ));
        }
        if policy.secure.is_none() && policy.http_only.is_none() && policy.same_site.is_none() {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "each policy must set at least one attribute",
            ));
        }
        if !names.insert(policy.name.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "must not contain duplicate cookie names",
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
            "must be between 0 and 3",
        ));
    }
    if retry.delay_ms > 60_000 {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.delay_ms",
            "retry delay must not exceed 60000 milliseconds",
        ));
    }
    if retry.final_redispatch
        && (retry.max_retries == 0 || retry.target != crate::model::HttpRetryTarget::SameServer)
    {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.final_redispatch",
            "final redispatch requires at least one same-server retry",
        ));
    }
    if retry.triggers.is_empty() && retry.max_retries != 0 && retry.response_statuses.is_empty() {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.triggers",
            "must not be empty when retries are enabled without response statuses",
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
            HttpRetryTrigger::EmptyResponse => 3,
            HttpRetryTrigger::ResponseTimeout => 4,
            HttpRetryTrigger::JunkResponse => 5,
        });
    if retry.response_statuses.len() > crate::defaults::MAX_HTTP_RETRY_RESPONSE_STATUSES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.response_statuses",
            "must contain at most 100 statuses",
        ));
    }
    let mut response_statuses = HashSet::with_capacity(retry.response_statuses.len());
    for status in &retry.response_statuses {
        if !(500..=599).contains(status) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.response_statuses",
                "must contain only 5xx statuses",
            ));
        }
        if !response_statuses.insert(*status) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.response_statuses",
                "must not contain duplicates",
            ));
        }
    }
    retry.response_statuses.sort_unstable();
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
    for header in headers {
        normalize_header_name(&mut header.name)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
        validate_header_value(&header.value)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
    }
    Ok(())
}

fn validate_redirect(
    service: &str,
    route_index: usize,
    status: u16,
    location: &HttpRedirectLocation,
    headers: &mut [HttpLiteralHeader],
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
        HttpRedirectLocation::RequestTemplate {
            value,
            nginx_host_fallback,
        } => {
            if let Some(fallback) = nginx_host_fallback {
                validate_nginx_host_fallback(fallback).map_err(|detail| {
                    invalid_route(service, route_index, "action.redirect.location", detail)
                })?;
            }
            (value, true)
        }
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
    validate_literal_headers(service, route_index, "action.redirect.headers", headers)
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

fn validate_nginx_host_fallback(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('@') {
        return Err("nginx host fallback must be a bounded host without a port or userinfo");
    }
    if let Some(ip) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .and_then(|value| value.parse::<std::net::Ipv6Addr>().ok())
    {
        return (value == format!("[{ip}]"))
            .then_some(())
            .ok_or("nginx host fallback must be a normalized exact DNS name or IP address");
    }
    if value.contains(':') {
        return Err("nginx host fallback must be a bounded host without a port or userinfo");
    }
    let mut normalized = value.to_owned();
    if !normalize_host(&mut normalized) || normalized != value.to_ascii_lowercase() {
        return Err("nginx host fallback must be a normalized exact DNS name or IP address");
    }
    Ok(())
}

struct StaticFilesValidation<'a> {
    root_directory: &'a mut std::path::PathBuf,
    index_files: &'a [String],
    spa_fallback: Option<&'a std::path::Path>,
    try_files: &'a [HttpStaticTryFile],
    mime: &'a HttpStaticMimePolicy,
    headers: &'a mut [HttpLiteralHeader],
    error_responses: &'a mut [HttpStaticErrorResponse],
}

fn validate_static_files(
    service: &str,
    route_index: usize,
    fields: &mut StaticFilesValidation<'_>,
) -> Result<(), ConfigError> {
    normalize_absolute_directory(fields.root_directory).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.static_files.root_directory",
            detail,
        )
    })?;
    validate_static_indexes(
        service,
        route_index,
        fields.index_files,
        fields.spa_fallback,
    )?;
    validate_static_try_files(service, route_index, fields.try_files)?;
    validate_static_mime(service, route_index, fields.mime)?;
    validate_literal_headers(
        service,
        route_index,
        "action.static_files.headers",
        fields.headers,
    )?;
    validate_static_error_responses(service, route_index, fields.error_responses)
}

fn validate_static_indexes(
    service: &str,
    route_index: usize,
    index_files: &[String],
    spa_fallback: Option<&std::path::Path>,
) -> Result<(), ConfigError> {
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

fn validate_static_try_files(
    service: &str,
    route_index: usize,
    try_files: &[HttpStaticTryFile],
) -> Result<(), ConfigError> {
    if try_files.len() > MAX_HTTP_STATIC_TRY_FILES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.try_files",
            "must contain at most 16 candidates",
        ));
    }
    for (candidate_index, candidate) in try_files.iter().enumerate() {
        match candidate {
            HttpStaticTryFile::Relative { path } => {
                validate_relative_path(path, MAX_HTTP_STATIC_FALLBACK_BYTES).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.static_files.try_files",
                        detail,
                    )
                })?;
            }
            HttpStaticTryFile::Status { status }
                if !(400..=599).contains(status) || candidate_index + 1 != try_files.len() =>
            {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.try_files",
                    "terminal status must be the final candidate and between 400 and 599",
                ));
            }
            HttpStaticTryFile::RequestPath
            | HttpStaticTryFile::RequestPathDirectory
            | HttpStaticTryFile::Status { .. } => {}
        }
    }
    Ok(())
}

fn validate_static_error_responses(
    service: &str,
    route_index: usize,
    error_responses: &mut [HttpStaticErrorResponse],
) -> Result<(), ConfigError> {
    if error_responses.len() > MAX_HTTP_STATIC_ERROR_RESPONSES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.error_responses",
            "must contain at most 16 responses",
        ));
    }
    let mut statuses = HashSet::new();
    for response in error_responses {
        if response.statuses.is_empty() || response.statuses.len() > MAX_HTTP_STATIC_ERROR_STATUSES
        {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.error_responses",
                "each response must contain 1..=16 statuses",
            ));
        }
        match (&response.file, &response.body) {
            (Some(file), None) => validate_relative_path(file, MAX_HTTP_STATIC_FALLBACK_BYTES)
                .map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.static_files.error_responses",
                        detail,
                    )
                })?,
            (None, Some(body)) if body.len() <= MAX_HTTP_FIXED_RESPONSE_BODY_BYTES => {}
            _ => {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses",
                    "must configure exactly one bounded file or inline body",
                ));
            }
        }
        if response.body.is_some() && response.internal_redirect.is_some() {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.error_responses.internal_redirect",
                "is valid only for a file response",
            ));
        }
        validate_literal_headers(
            service,
            route_index,
            "action.static_files.error_responses.headers",
            &mut response.headers,
        )?;
        if let Some(path) = &response.internal_redirect {
            let canonical = canonicalize_http_path(path).ok_or_else(|| {
                invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses.internal_redirect",
                    "must be an absolute canonical HTTP path",
                )
            })?;
            if canonical.as_ref() != path || path.len() > MAX_HTTP_STATIC_FALLBACK_BYTES {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses.internal_redirect",
                    "must be an absolute canonical HTTP path within the static fallback bound",
                ));
            }
        }
        for status in &response.statuses {
            if !(400..=599).contains(status) || !statuses.insert(*status) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses",
                    "statuses must be unique values between 400 and 599",
                ));
            }
        }
    }
    Ok(())
}

fn validate_static_mime(
    service: &str,
    route_index: usize,
    mime: &HttpStaticMimePolicy,
) -> Result<(), ConfigError> {
    if mime.types.len() > MAX_HTTP_STATIC_MIME_TYPES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.mime.types",
            "must contain at most 2048 mappings",
        ));
    }
    if let Some(default_type) = &mime.default_type {
        validate_content_type(default_type).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.mime.default_type",
                detail,
            )
        })?;
    }
    let mut extensions = HashSet::with_capacity(mime.types.len());
    for mapping in &mime.types {
        let valid_extension = !mapping.extension.is_empty()
            && mapping.extension.len() <= MAX_HTTP_FILE_EXTENSION_BYTES
            && !mapping.extension.starts_with('.')
            && !mapping.extension.ends_with('.')
            && !mapping.extension.contains("..")
            && mapping.extension.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, '+' | '-' | '_' | '.')
            });
        if !valid_extension || !extensions.insert(mapping.extension.as_str()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.mime.types",
                "extensions must be unique safe suffixes of at most 32 bytes",
            ));
        }
        validate_content_type(&mapping.content_type).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.mime.types",
                detail,
            )
        })?;
    }
    Ok(())
}

fn validate_content_type(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_HTTP_MIME_TYPE_BYTES || !value.contains('/') {
        return Err("content type must be 1..=128 bytes and contain `/`");
    }
    validate_header_value(value)
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
