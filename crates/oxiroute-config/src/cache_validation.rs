use std::collections::{HashMap, HashSet};

use crate::{
    defaults::{
        MAX_CACHE_ENTRIES, MAX_CACHE_FOLLOWERS_PER_FILL, MAX_CACHE_HEADER_BYTES,
        MAX_CACHE_IN_FLIGHT_FILLS, MAX_CACHE_KEY_BYTES, MAX_CACHE_KEY_COMPONENTS,
        MAX_CACHE_METHODS, MAX_CACHE_OBJECT_BYTES, MAX_CACHE_PREDICATES, MAX_CACHE_RETENTION_MS,
        MAX_CACHE_STALE_TRIGGERS, MAX_CACHE_STATUS_TTLS, MAX_CACHE_STORE_BYTES, MAX_CACHE_STORES,
        MAX_CACHE_TAG_BYTES, MAX_CACHE_TAGS_PER_OBJECT,
    },
    http_validation::normalize_header_name,
    lexical::{normalize_absolute_directory, normalize_http_token, validate_file_path},
    model::{
        CacheKeyComponent, CachePredicate, CachePurgeAuthorization, CacheStore, ConfigError,
        HttpCachePolicy,
    },
};

#[derive(Clone, Copy)]
pub(crate) struct CacheStoreBounds {
    max_tags_per_object: u64,
    max_tag_bytes: u64,
}

#[derive(Clone, Copy)]
struct CacheStoreLimits {
    bytes: u64,
    entries: u64,
    object_bytes: u64,
    header_bytes: u64,
    key_bytes: u64,
    tag_bytes: u64,
    tags_per_object: u64,
    in_flight_fills: u64,
    followers_per_fill: u64,
}

pub(crate) fn validate_cache_stores(
    stores: &mut [CacheStore],
) -> Result<HashMap<String, CacheStoreBounds>, ConfigError> {
    if stores.len() > MAX_CACHE_STORES {
        return Err(invalid_store(
            "<configuration>",
            "cache_stores",
            format!("must contain at most {MAX_CACHE_STORES} stores"),
        ));
    }

    let mut names = HashSet::with_capacity(stores.len());
    let mut disk_roots = HashSet::new();
    let mut bounds = HashMap::with_capacity(stores.len());
    for store in stores {
        let name = store.name().to_owned();
        validate_store_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(ConfigError::DuplicateName {
                namespace: "cache store",
                name,
            });
        }
        let limits = cache_store_limits(store, &name)?;
        if let CacheStore::Disk { root_directory, .. } = store
            && !disk_roots.insert(root_directory.clone())
        {
            return Err(invalid_store(
                &name,
                "root_directory",
                "must be unique across disk stores",
            ));
        }
        validate_cache_store_limits(&name, limits)?;
        bounds.insert(
            name,
            CacheStoreBounds {
                max_tags_per_object: limits.tags_per_object,
                max_tag_bytes: limits.tag_bytes,
            },
        );
    }
    Ok(bounds)
}

fn cache_store_limits(store: &mut CacheStore, name: &str) -> Result<CacheStoreLimits, ConfigError> {
    let (
        max_bytes,
        max_entries,
        max_object_bytes,
        max_header_bytes,
        max_key_bytes,
        max_tag_bytes,
        max_tags_per_object,
        max_in_flight_fills,
        max_followers_per_fill,
    ) = match store {
        CacheStore::Memory {
            max_bytes,
            max_entries,
            max_object_bytes,
            max_header_bytes,
            max_key_bytes,
            max_tag_bytes,
            max_tags_per_object,
            max_in_flight_fills,
            max_followers_per_fill,
            ..
        } => (
            *max_bytes,
            *max_entries,
            *max_object_bytes,
            *max_header_bytes,
            *max_key_bytes,
            *max_tag_bytes,
            *max_tags_per_object,
            *max_in_flight_fills,
            *max_followers_per_fill,
        ),
        CacheStore::Disk {
            root_directory,
            max_bytes,
            max_files,
            max_object_bytes,
            max_header_bytes,
            max_key_bytes,
            max_tag_bytes,
            max_tags_per_object,
            max_in_flight_fills,
            max_followers_per_fill,
            ..
        } => {
            normalize_absolute_directory(root_directory)
                .map_err(|detail| invalid_store(name, "root_directory", detail))?;
            (
                *max_bytes,
                *max_files,
                *max_object_bytes,
                *max_header_bytes,
                *max_key_bytes,
                *max_tag_bytes,
                *max_tags_per_object,
                *max_in_flight_fills,
                *max_followers_per_fill,
            )
        }
    };
    Ok(CacheStoreLimits {
        bytes: max_bytes,
        entries: max_entries,
        object_bytes: max_object_bytes,
        header_bytes: max_header_bytes,
        key_bytes: max_key_bytes,
        tag_bytes: max_tag_bytes,
        tags_per_object: max_tags_per_object,
        in_flight_fills: max_in_flight_fills,
        followers_per_fill: max_followers_per_fill,
    })
}

fn validate_cache_store_limits(name: &str, limits: CacheStoreLimits) -> Result<(), ConfigError> {
    validate_bound(name, "max_bytes", limits.bytes, MAX_CACHE_STORE_BYTES)?;
    validate_bound(
        name,
        "max_entries/max_files",
        limits.entries,
        MAX_CACHE_ENTRIES,
    )?;
    validate_bound(
        name,
        "max_object_bytes",
        limits.object_bytes,
        MAX_CACHE_OBJECT_BYTES,
    )?;
    validate_bound(
        name,
        "max_header_bytes",
        limits.header_bytes,
        MAX_CACHE_HEADER_BYTES,
    )?;
    validate_bound(name, "max_key_bytes", limits.key_bytes, MAX_CACHE_KEY_BYTES)?;
    validate_bound(name, "max_tag_bytes", limits.tag_bytes, MAX_CACHE_TAG_BYTES)?;
    validate_bound(
        name,
        "max_tags_per_object",
        limits.tags_per_object,
        MAX_CACHE_TAGS_PER_OBJECT,
    )?;
    validate_bound(
        name,
        "max_in_flight_fills",
        limits.in_flight_fills,
        MAX_CACHE_IN_FLIGHT_FILLS,
    )?;
    validate_bound(
        name,
        "max_followers_per_fill",
        limits.followers_per_fill,
        MAX_CACHE_FOLLOWERS_PER_FILL,
    )?;
    if limits.object_bytes > limits.bytes {
        return Err(invalid_store(
            name,
            "max_object_bytes",
            "must not exceed max_bytes",
        ));
    }
    if limits.header_bytes > limits.object_bytes {
        return Err(invalid_store(
            name,
            "max_header_bytes",
            "must not exceed max_object_bytes",
        ));
    }
    Ok(())
}

pub(crate) fn validate_cache_policy(
    service: &str,
    route: usize,
    policy: &mut HttpCachePolicy,
    stores: &HashMap<String, CacheStoreBounds>,
) -> Result<(), ConfigError> {
    let store = stores
        .get(&policy.store)
        .ok_or_else(|| ConfigError::UnknownCacheStore {
            service: service.into(),
            route,
            store: policy.store.clone(),
        })?;
    let invalid = |field, detail: String| ConfigError::InvalidCachePolicy {
        service: service.into(),
        route,
        field,
        detail,
    };

    validate_cache_key(policy, &invalid)?;
    validate_cache_retention(policy, &invalid)?;
    validate_predicates("bypass_request", &mut policy.bypass_request, &invalid)?;
    validate_predicates("no_store_request", &mut policy.no_store_request, &invalid)?;
    validate_predicates("no_store_response", &mut policy.no_store_response, &invalid)?;

    if let Some(tags) = &mut policy.surrogate_tags {
        normalize_header_name(&mut tags.response_header)
            .map_err(|detail| invalid("surrogate_tags.response_header", detail.into()))?;
        if tags.max_tags == 0 || tags.max_tags > store.max_tags_per_object {
            return Err(invalid(
                "surrogate_tags.max_tags",
                "must fit the referenced store's max_tags_per_object".into(),
            ));
        }
        if tags.max_tag_bytes == 0 || tags.max_tag_bytes > store.max_tag_bytes {
            return Err(invalid(
                "surrogate_tags.max_tag_bytes",
                "must fit the referenced store's max_tag_bytes".into(),
            ));
        }
    }
    if let Some(CachePurgeAuthorization::BearerTokenFile { token_file_path }) =
        &policy.purge_authorization
    {
        let identity = format!("{service} route {route}");
        validate_file_path(
            "HTTP cache purge policy",
            &identity,
            "token_file_path",
            token_file_path,
        )?;
    }
    Ok(())
}

fn validate_cache_key(
    policy: &mut HttpCachePolicy,
    invalid: &impl Fn(&'static str, String) -> ConfigError,
) -> Result<(), ConfigError> {
    if policy.methods.is_empty() || policy.methods.len() > MAX_CACHE_METHODS {
        return Err(invalid("methods", "must contain 1..=8 methods".into()));
    }
    let mut methods = HashSet::with_capacity(policy.methods.len());
    for method in &mut policy.methods {
        if normalize_http_token(method).is_err() {
            return Err(invalid(
                "methods",
                "each method must be an HTTP token of at most 32 bytes".into(),
            ));
        }
        if !matches!(method.as_str(), "GET" | "HEAD") {
            return Err(invalid("methods", "only GET and HEAD are cacheable".into()));
        }
        if !methods.insert(method.clone()) {
            return Err(invalid(
                "methods",
                format!("contains duplicate method `{method}`"),
            ));
        }
    }
    policy.methods.sort_unstable();

    if policy.key_components.is_empty() || policy.key_components.len() > MAX_CACHE_KEY_COMPONENTS {
        return Err(invalid(
            "key_components",
            "must contain 1..=32 components".into(),
        ));
    }
    let mut components = HashSet::with_capacity(policy.key_components.len());
    for component in &mut policy.key_components {
        match component {
            CacheKeyComponent::Header { name } => normalize_header_name(name)
                .map_err(|detail| invalid("key_components", detail.into()))?,
            CacheKeyComponent::Cookie { name } => validate_cookie_name(name)
                .map_err(|detail| invalid("key_components", detail.into()))?,
            CacheKeyComponent::Scheme
            | CacheKeyComponent::NormalizedHost
            | CacheKeyComponent::PathAndQuery => {}
        }
        if !components.insert(component.clone()) {
            return Err(invalid(
                "key_components",
                "contains a duplicate component".into(),
            ));
        }
    }
    Ok(())
}

fn validate_cache_retention(
    policy: &HttpCachePolicy,
    invalid: &impl Fn(&'static str, String) -> ConfigError,
) -> Result<(), ConfigError> {
    validate_retention("default_ttl_ms", policy.default_ttl_ms, invalid)?;
    validate_retention("grace_ms", policy.grace_ms, invalid)?;
    validate_retention("keep_ms", policy.keep_ms, invalid)?;
    if policy.grace_ms > policy.keep_ms {
        return Err(invalid("grace_ms", "must not exceed keep_ms".into()));
    }
    if policy.status_ttls.len() > MAX_CACHE_STATUS_TTLS {
        return Err(invalid(
            "status_ttls",
            "must contain at most 64 entries".into(),
        ));
    }
    let mut statuses = HashSet::with_capacity(policy.status_ttls.len());
    for status_ttl in &policy.status_ttls {
        if !(200..=599).contains(&status_ttl.status) || matches!(status_ttl.status, 206 | 304) {
            return Err(invalid(
                "status_ttls",
                "status must be between 200 and 599 and cannot be 206 or 304".into(),
            ));
        }
        validate_retention("status_ttls", status_ttl.ttl_ms, invalid)?;
        if !statuses.insert(status_ttl.status) {
            return Err(invalid(
                "status_ttls",
                format!("contains duplicate status `{}`", status_ttl.status),
            ));
        }
    }
    if policy.stale_on.len() > MAX_CACHE_STALE_TRIGGERS
        || policy.stale_on.iter().collect::<HashSet<_>>().len() != policy.stale_on.len()
    {
        return Err(invalid(
            "stale_on",
            "must contain at most 8 unique triggers".into(),
        ));
    }
    Ok(())
}

fn validate_store_name(name: &str) -> Result<(), ConfigError> {
    if name.trim().is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        return Err(invalid_store(
            name,
            "name",
            "must be a nonblank canonical name",
        ));
    }
    Ok(())
}

fn validate_bound(
    store: &str,
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return Err(invalid_store(
            store,
            field,
            format!("must be between 1 and {maximum}"),
        ));
    }
    Ok(())
}

fn validate_retention(
    field: &'static str,
    value: u64,
    invalid: &impl Fn(&'static str, String) -> ConfigError,
) -> Result<(), ConfigError> {
    if value > MAX_CACHE_RETENTION_MS {
        return Err(invalid(
            field,
            format!("must not exceed {MAX_CACHE_RETENTION_MS}"),
        ));
    }
    Ok(())
}

fn validate_predicates(
    field: &'static str,
    predicates: &mut [CachePredicate],
    invalid: &impl Fn(&'static str, String) -> ConfigError,
) -> Result<(), ConfigError> {
    if predicates.len() > MAX_CACHE_PREDICATES {
        return Err(invalid(field, "must contain at most 32 predicates".into()));
    }
    let mut unique = HashSet::with_capacity(predicates.len());
    for predicate in predicates {
        match predicate {
            CachePredicate::HeaderPresent { name } => {
                normalize_header_name(name).map_err(|detail| invalid(field, detail.into()))?;
            }
            CachePredicate::CookiePresent { name } => {
                validate_cookie_name(name).map_err(|detail| invalid(field, detail.into()))?;
            }
        }
        if !unique.insert(predicate.clone()) {
            return Err(invalid(field, "contains a duplicate predicate".into()));
        }
    }
    Ok(())
}

fn validate_cookie_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > 256
        || !name.bytes().all(|byte| {
            byte.is_ascii_graphic()
                && !matches!(
                    byte,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                        | b'{'
                        | b'}'
                )
        })
    {
        return Err("cookie name must be a token of at most 256 bytes");
    }
    Ok(())
}

fn invalid_store(store: &str, field: &'static str, detail: impl Into<String>) -> ConfigError {
    ConfigError::InvalidCacheStore {
        store: store.into(),
        field,
        detail: detail.into(),
    }
}
