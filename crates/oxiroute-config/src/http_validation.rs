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
        MAX_HTTP_METHODS_PER_ROUTE, MAX_HTTP_MIME_TYPE_BYTES, MAX_HTTP_PROXY_PATH_BYTES,
        MAX_HTTP_REDIRECT_LOCATION_BYTES, MAX_HTTP_RETRIES, MAX_HTTP_STATIC_ERROR_RESPONSES,
        MAX_HTTP_STATIC_ERROR_STATUSES, MAX_HTTP_STATIC_FALLBACK_BYTES,
        MAX_HTTP_STATIC_INDEX_BYTES, MAX_HTTP_STATIC_INDEX_FILES, MAX_HTTP_STATIC_MIME_TYPES,
        MAX_HTTP_STATIC_TRY_FILES, MAX_HTTP_TIMEOUT_MS, MAX_SAFE_JSON_INTEGER,
    },
    lexical::{
        authority_has_invalid_port, canonicalize_http_path, is_valid_dns_name,
        normalize_absolute_directory, normalize_host, normalize_http_token, validate_file_path,
        validate_relative_path,
    },
    model::{
        AccessLogPolicy, ConfigError, HttpAccessPolicy, HttpAffinity, HttpAffinityKey,
        HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader,
        HttpPathSelector, HttpProxyPathRewrite, HttpProxyPolicy, HttpProxyTimeouts,
        HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpRescueTarget,
        HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
        HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy, HttpStaticTryFile,
        HttpUpstreamHost, UpstreamEndpoint, UpstreamPool,
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
                route.policy,
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

mod action;
mod headers;
mod matcher;
mod proxy_policy;
mod retry;
mod static_files;

#[allow(clippy::wildcard_imports)]
use action::*;
pub(crate) use headers::normalize_header_name;
#[allow(clippy::wildcard_imports)]
use headers::*;
pub(crate) use matcher::validate_realm;
#[allow(clippy::wildcard_imports)]
use matcher::*;
#[allow(clippy::wildcard_imports)]
use proxy_policy::*;
#[allow(clippy::wildcard_imports)]
use retry::*;
#[allow(clippy::wildcard_imports)]
use static_files::*;

#[cfg(test)]
mod retry_tests;
