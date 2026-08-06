use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use crate::cache_validation::CacheStoreBounds;
use crate::{
    defaults::{
        MAX_FORWARD_ACCESS_CONDITIONS, MAX_FORWARD_ACCESS_MATCHERS, MAX_FORWARD_ACCESS_RULES,
        MAX_FORWARD_BODY_BYTES, MAX_FORWARD_CIDRS, MAX_FORWARD_CONNECT_PORTS,
        MAX_FORWARD_CONNECTIONS, MAX_FORWARD_DOMAINS, MAX_FORWARD_HEADER_BYTES,
        MAX_FORWARD_NAMESERVERS, MAX_FORWARD_PEER_RETRIES, MAX_FORWARD_PEERS,
        MAX_FORWARD_PROXY_SERVICES, MAX_FORWARD_RESOLVER_ADDRESSES,
        MAX_FORWARD_RESOLVER_CACHE_ENTRIES, MAX_FORWARD_RESOLVER_CONCURRENT_QUERIES,
        MAX_FORWARD_TIME_RANGES, MAX_FORWARD_TIMEOUT_MS,
    },
    lexical::{is_valid_certificate_dns_name, is_valid_dns_name, validate_file_path},
    model::{
        CacheAuthorizationPolicy, CacheKeyComponent, CacheSetCookiePolicy, CacheVaryPolicy,
        ConfigError, ForwardAccessMatcher, ForwardDirectFallback, ForwardHttpVersion,
        ForwardProxyAuth, ForwardProxyService, ForwardTimeRange, ForwardWeekday,
    },
};

pub(crate) fn validate_forward_proxy_services(
    services: &mut [ForwardProxyService],
    cache_stores: &HashMap<String, CacheStoreBounds>,
) -> Result<(), ConfigError> {
    if services.len() > MAX_FORWARD_PROXY_SERVICES {
        return Err(invalid(
            "<configuration>",
            "forward_proxy_services",
            format!("must contain at most {MAX_FORWARD_PROXY_SERVICES} services"),
        ));
    }

    let mut names = HashSet::with_capacity(services.len());
    for service in services {
        if service.name.trim().is_empty()
            || service.name.trim() != service.name
            || service.name.chars().any(char::is_control)
        {
            return Err(invalid(
                &service.name,
                "name",
                "must be a nonblank canonical name",
            ));
        }
        if !names.insert(service.name.clone()) {
            return Err(ConfigError::DuplicateName {
                namespace: "forward proxy service",
                name: service.name.clone(),
            });
        }
        validate_service(service, cache_stores)?;
    }
    Ok(())
}

fn validate_service(
    service: &mut ForwardProxyService,
    cache_stores: &HashMap<String, CacheStoreBounds>,
) -> Result<(), ConfigError> {
    validate_versions_and_connect(service)?;
    validate_peer_policy(service)?;
    if let Some(auth) = &service.auth {
        match auth {
            ForwardProxyAuth::BearerTokenFile { token_file_path } => validate_file_path(
                "forward proxy service",
                &service.name,
                "auth.token_file_path",
                token_file_path,
            )?,
            ForwardProxyAuth::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
                credential_ttl_ms,
                ..
            } => {
                validate_file_path(
                    "forward proxy service",
                    &service.name,
                    "auth.htpasswd_file_path",
                    htpasswd_file_path,
                )?;
                crate::http_validation::validate_realm(realm)
                    .map_err(|detail| invalid(&service.name, "auth.realm", detail))?;
                if credential_ttl_ms
                    .is_some_and(|value| value == 0 || value > MAX_FORWARD_TIMEOUT_MS)
                {
                    return Err(invalid(
                        &service.name,
                        "auth.credential_ttl_ms",
                        format!("must be null or between 1 and {MAX_FORWARD_TIMEOUT_MS}"),
                    ));
                }
            }
            ForwardProxyAuth::MutualTls {
                client_ca_file_path,
            } => {
                validate_file_path(
                    "forward proxy service",
                    &service.name,
                    "auth.client_ca_file_path",
                    client_ca_file_path,
                )?;
                return Err(invalid(
                    &service.name,
                    "auth.type",
                    "mutual_tls requires a listener TLS client-certificate verifier and is not available in the current runtime",
                ));
            }
        }
    }
    validate_access_policy(service)?;
    validate_destinations(service)?;
    validate_cache_policy(service, cache_stores)?;
    validate_resolver(service)?;
    validate_service_limits(service)
}

fn validate_peer_policy(service: &mut ForwardProxyService) -> Result<(), ConfigError> {
    let policy = &mut service.peer_policy;
    if policy.peers.len() > MAX_FORWARD_PEERS {
        return Err(invalid(
            &service.name,
            "peer_policy.peers",
            format!("must contain at most {MAX_FORWARD_PEERS} peers"),
        ));
    }
    if policy.max_retries > MAX_FORWARD_PEER_RETRIES {
        return Err(invalid(
            &service.name,
            "peer_policy.max_retries",
            format!("must be at most {MAX_FORWARD_PEER_RETRIES}"),
        ));
    }
    if policy.peers.is_empty() && policy.direct_fallback == ForwardDirectFallback::Denied {
        return Err(invalid(
            &service.name,
            "peer_policy.direct_fallback",
            "denied requires at least one static peer",
        ));
    }
    if !policy.peers.is_empty()
        && service
            .enabled_versions
            .iter()
            .any(|version| *version != ForwardHttpVersion::H1)
    {
        return Err(invalid(
            &service.name,
            "peer_policy.peers",
            "static peer selection is supported only with forward HTTP/1",
        ));
    }

    let mut identities = HashSet::with_capacity(policy.peers.len());
    for peer in &mut policy.peers {
        if peer.port == 0 {
            return Err(invalid(
                &service.name,
                "peer_policy.peers.port",
                "must be nonzero",
            ));
        }
        let identity = if let Ok(address) = peer.host.parse::<IpAddr>() {
            if address.is_unspecified() || address.is_multicast() {
                return Err(invalid(
                    &service.name,
                    "peer_policy.peers.host",
                    "must be a specific unicast address or DNS name",
                ));
            }
            peer.host = address.to_string();
            peer.host.clone()
        } else {
            peer.host.make_ascii_lowercase();
            if !is_valid_dns_name(&peer.host) {
                return Err(invalid(
                    &service.name,
                    "peer_policy.peers.host",
                    "must be a canonical DNS name or IP address",
                ));
            }
            peer.host.clone()
        };
        if !identities.insert((identity, peer.port)) {
            return Err(invalid(
                &service.name,
                "peer_policy.peers",
                "must not contain duplicate host and port pairs",
            ));
        }
    }
    Ok(())
}

fn validate_cache_policy(
    service: &mut ForwardProxyService,
    cache_stores: &HashMap<String, CacheStoreBounds>,
) -> Result<(), ConfigError> {
    let Some(policy) = service.header_policy.cache.as_mut() else {
        return Ok(());
    };
    if !service.allow_absolute_form {
        return Err(invalid(
            &service.name,
            "cache",
            "requires allow_absolute_form = true",
        ));
    }
    if !service.enabled_versions.contains(&ForwardHttpVersion::H1) {
        return Err(invalid(
            &service.name,
            "cache",
            "requires forward HTTP/1 to be enabled",
        ));
    }
    crate::cache_validation::validate_cache_policy(&service.name, 0, policy, cache_stores)?;

    let expected_key = [
        CacheKeyComponent::Scheme,
        CacheKeyComponent::NormalizedHost,
        CacheKeyComponent::PathAndQuery,
    ];
    if policy.key_components != expected_key {
        return Err(invalid(
            &service.name,
            "cache.key_components",
            "forward cache requires scheme, normalized_host, and path_and_query",
        ));
    }
    if !policy.bypass_request.is_empty()
        || !policy.no_store_request.is_empty()
        || !policy.no_store_response.is_empty()
    {
        return Err(invalid(
            &service.name,
            "cache",
            "request and response predicates are not supported by the forward cache",
        ));
    }
    if policy.set_cookie_policy != CacheSetCookiePolicy::Bypass {
        return Err(invalid(
            &service.name,
            "cache.set_cookie_policy",
            "must be bypass for the forward cache",
        ));
    }
    if policy.authorization_policy != CacheAuthorizationPolicy::Bypass {
        return Err(invalid(
            &service.name,
            "cache.authorization_policy",
            "must be bypass so authenticated responses require explicit shared permission",
        ));
    }
    if policy.vary_policy != CacheVaryPolicy::Respect {
        return Err(invalid(
            &service.name,
            "cache.vary_policy",
            "must respect origin Vary fields",
        ));
    }
    if !policy.stale_on.is_empty() {
        return Err(invalid(
            &service.name,
            "cache.stale_on",
            "forward cache uses canonical freshness and revalidation windows",
        ));
    }
    if !policy.collapsed_forwarding {
        return Err(invalid(
            &service.name,
            "cache.collapsed_forwarding",
            "must remain enabled for bounded forward fills",
        ));
    }
    Ok(())
}

fn validate_access_policy(service: &mut ForwardProxyService) -> Result<(), ConfigError> {
    let Some(policy) = &mut service.access_policy else {
        return Ok(());
    };
    if policy.rules.len() > MAX_FORWARD_ACCESS_RULES {
        return Err(invalid(
            &service.name,
            "access_policy.rules",
            format!("must contain at most {MAX_FORWARD_ACCESS_RULES} rules"),
        ));
    }
    for rule in &mut policy.rules {
        if rule.conditions.len() > MAX_FORWARD_ACCESS_CONDITIONS {
            return Err(invalid(
                &service.name,
                "access_policy.rules.conditions",
                format!("must contain at most {MAX_FORWARD_ACCESS_CONDITIONS} conditions"),
            ));
        }
        for condition in &mut rule.conditions {
            match &mut condition.matcher {
                ForwardAccessMatcher::All
                | ForwardAccessMatcher::DestinationLocal
                | ForwardAccessMatcher::DestinationLinkLocal
                | ForwardAccessMatcher::Manager => {}
                ForwardAccessMatcher::Authenticated => {
                    if service.auth.is_none() {
                        return Err(invalid(
                            &service.name,
                            "access_policy.rules.conditions",
                            "authenticated conditions require an authentication provider",
                        ));
                    }
                }
                ForwardAccessMatcher::Methods { methods } => {
                    if methods.is_empty() || methods.len() > MAX_FORWARD_ACCESS_MATCHERS {
                        return Err(invalid(
                            &service.name,
                            "access_policy.rules.conditions.methods",
                            "must contain 1..=256 methods",
                        ));
                    }
                    let mut unique = HashSet::with_capacity(methods.len());
                    for method in methods {
                        if method.is_empty()
                            || method.len() > 32
                            || !method
                                .bytes()
                                .all(|byte| byte.is_ascii_uppercase() || byte == b'-')
                            || !unique.insert(method.clone())
                        {
                            return Err(invalid(
                                &service.name,
                                "access_policy.rules.conditions.methods",
                                "must contain unique canonical uppercase HTTP methods",
                            ));
                        }
                    }
                }
                ForwardAccessMatcher::SourceCidrs { cidrs } => {
                    validate_cidrs(&service.name, "access_policy.rules.conditions.cidrs", cidrs)?;
                    if cidrs.is_empty() {
                        return Err(invalid(
                            &service.name,
                            "access_policy.rules.conditions.cidrs",
                            "must not be empty",
                        ));
                    }
                }
                ForwardAccessMatcher::DestinationPorts { ranges } => {
                    if ranges.is_empty() || ranges.len() > MAX_FORWARD_ACCESS_MATCHERS {
                        return Err(invalid(
                            &service.name,
                            "access_policy.rules.conditions.ranges",
                            "must contain 1..=256 ranges",
                        ));
                    }
                    for range in ranges {
                        if range.start == 0 || range.start > range.end {
                            return Err(invalid(
                                &service.name,
                                "access_policy.rules.conditions.ranges",
                                "must contain ordered nonzero port ranges",
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_resolver(service: &ForwardProxyService) -> Result<(), ConfigError> {
    let nameservers = &service.resolver.nameservers;
    if nameservers.len() > MAX_FORWARD_NAMESERVERS {
        return Err(invalid(
            &service.name,
            "resolver.nameservers",
            format!("must contain at most {MAX_FORWARD_NAMESERVERS} addresses"),
        ));
    }
    let mut unique = HashSet::with_capacity(nameservers.len());
    if nameservers.iter().any(|address| {
        address.is_unspecified() || address.is_multicast() || !unique.insert(*address)
    }) {
        return Err(invalid(
            &service.name,
            "resolver.nameservers",
            "must contain unique unicast addresses",
        ));
    }
    Ok(())
}

fn validate_versions_and_connect(service: &mut ForwardProxyService) -> Result<(), ConfigError> {
    if service.enabled_versions.is_empty() {
        return Err(invalid(
            &service.name,
            "enabled_versions",
            "must contain at least one version",
        ));
    }
    let versions = service
        .enabled_versions
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if versions.len() != service.enabled_versions.len() {
        return Err(invalid(
            &service.name,
            "enabled_versions",
            "must not contain duplicates",
        ));
    }
    service
        .enabled_versions
        .sort_by_key(|version| match version {
            ForwardHttpVersion::H1 => 1,
            ForwardHttpVersion::H2 => 2,
            ForwardHttpVersion::H3 => 3,
        });

    validate_connect_policy(&service.name, "connect", &mut service.connect)?;
    validate_connect_policy(&service.name, "connect_udp", &mut service.connect_udp)?;

    Ok(())
}

fn validate_connect_policy(
    service_name: &str,
    field: &str,
    policy: &mut crate::ForwardConnectPolicy,
) -> Result<(), ConfigError> {
    let ports_field = match field {
        "connect" => "connect.allowed_ports",
        "connect_udp" => "connect_udp.allowed_ports",
        _ => unreachable!("validated forward tunnel policy field"),
    };
    if policy.allowed_ports.len() > MAX_FORWARD_CONNECT_PORTS
        || (policy.enabled && policy.allowed_ports.is_empty())
    {
        return Err(invalid(
            service_name,
            ports_field,
            "must contain 1..=64 ports when the tunnel policy is enabled",
        ));
    }
    if policy.allowed_ports.contains(&0) {
        return Err(invalid(
            service_name,
            ports_field,
            "must contain only nonzero ports",
        ));
    }
    policy.allowed_ports.sort_unstable();
    if policy
        .allowed_ports
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(invalid(
            service_name,
            ports_field,
            "must not contain duplicates",
        ));
    }
    Ok(())
}

fn validate_service_limits(service: &ForwardProxyService) -> Result<(), ConfigError> {
    validate_limit(
        service,
        "connect_timeout_ms",
        service.connect_timeout_ms,
        MAX_FORWARD_TIMEOUT_MS,
    )?;
    validate_limit(
        service,
        "idle_timeout_ms",
        service.idle_timeout_ms,
        MAX_FORWARD_TIMEOUT_MS,
    )?;
    validate_limit(
        service,
        "lifetime_timeout_ms",
        service.lifetime_timeout_ms,
        MAX_FORWARD_TIMEOUT_MS,
    )?;
    let body_limit = service.max_request_body_bytes.ok_or_else(|| {
        invalid(
            &service.name,
            "max_request_body_bytes",
            "must be a finite non-null limit",
        )
    })?;
    validate_limit(
        service,
        "max_request_body_bytes",
        body_limit,
        MAX_FORWARD_BODY_BYTES,
    )?;
    validate_limit(
        service,
        "max_header_bytes",
        service.max_header_bytes,
        MAX_FORWARD_HEADER_BYTES,
    )?;
    if service.max_header_bytes < 8_192 {
        return Err(invalid(
            &service.name,
            "max_header_bytes",
            "must be at least 8192 bytes for the HTTP/1 parser",
        ));
    }
    validate_limit(
        service,
        "max_connections",
        service.max_connections,
        MAX_FORWARD_CONNECTIONS,
    )?;
    validate_limit(
        service,
        "resolver.max_cache_entries",
        service.resolver.max_cache_entries,
        MAX_FORWARD_RESOLVER_CACHE_ENTRIES,
    )?;
    validate_limit(
        service,
        "resolver.max_concurrent_queries",
        service.resolver.max_concurrent_queries,
        MAX_FORWARD_RESOLVER_CONCURRENT_QUERIES,
    )?;
    validate_limit(
        service,
        "resolver.max_addresses_per_name",
        service.resolver.max_addresses_per_name,
        MAX_FORWARD_RESOLVER_ADDRESSES,
    )?;
    validate_limit(
        service,
        "resolver.min_ttl_ms",
        service.resolver.min_ttl_ms,
        MAX_FORWARD_TIMEOUT_MS,
    )?;
    validate_limit(
        service,
        "resolver.max_ttl_ms",
        service.resolver.max_ttl_ms,
        MAX_FORWARD_TIMEOUT_MS,
    )?;
    if service.resolver.negative_ttl_ms > MAX_FORWARD_TIMEOUT_MS {
        return Err(invalid(
            &service.name,
            "resolver.negative_ttl_ms",
            format!("must not exceed {MAX_FORWARD_TIMEOUT_MS}"),
        ));
    }
    if service.resolver.min_ttl_ms > service.resolver.max_ttl_ms {
        return Err(invalid(
            &service.name,
            "resolver.min_ttl_ms",
            "must not exceed resolver.max_ttl_ms",
        ));
    }
    Ok(())
}

fn validate_destinations(service: &mut ForwardProxyService) -> Result<(), ConfigError> {
    let policy = &mut service.destination_policy;
    validate_domains(
        &service.name,
        "destination_policy.allow_domains",
        &mut policy.allow_domains,
    )?;
    validate_domains(
        &service.name,
        "destination_policy.deny_domains",
        &mut policy.deny_domains,
    )?;
    validate_cidrs(
        &service.name,
        "destination_policy.allow_cidrs",
        &mut policy.allow_cidrs,
    )?;
    validate_cidrs(
        &service.name,
        "destination_policy.deny_cidrs",
        &mut policy.deny_cidrs,
    )?;
    validate_time_ranges(
        &service.name,
        "destination_policy.allow_times",
        &mut policy.allow_times,
    )?;
    validate_time_ranges(
        &service.name,
        "destination_policy.deny_times",
        &mut policy.deny_times,
    )?;

    let allowed_domains = policy.allow_domains.iter().collect::<HashSet<_>>();
    if let Some(domain) = policy
        .deny_domains
        .iter()
        .find(|domain| allowed_domains.contains(domain))
    {
        return Err(invalid(
            &service.name,
            "destination_policy",
            format!("domain `{domain}` appears in both allow and deny lists"),
        ));
    }
    let allowed_cidrs = policy.allow_cidrs.iter().collect::<HashSet<_>>();
    if let Some(cidr) = policy
        .deny_cidrs
        .iter()
        .find(|cidr| allowed_cidrs.contains(cidr))
    {
        return Err(invalid(
            &service.name,
            "destination_policy",
            format!("CIDR `{cidr}` appears in both allow and deny lists"),
        ));
    }
    if let Some(range) = policy
        .deny_times
        .iter()
        .find(|range| policy.allow_times.contains(range))
    {
        return Err(invalid(
            &service.name,
            "destination_policy",
            format!("time range `{range:?}` appears in both allow and deny lists"),
        ));
    }
    Ok(())
}

fn validate_domains(
    service: &str,
    field: &'static str,
    domains: &mut [String],
) -> Result<(), ConfigError> {
    if domains.len() > MAX_FORWARD_DOMAINS {
        return Err(invalid(
            service,
            field,
            format!("must contain at most {MAX_FORWARD_DOMAINS} domains"),
        ));
    }
    let mut unique = HashSet::with_capacity(domains.len());
    for domain in domains {
        domain.make_ascii_lowercase();
        if !is_valid_certificate_dns_name(domain) || domain.parse::<IpAddr>().is_ok() {
            return Err(invalid(
                service,
                field,
                format!("invalid domain `{domain}`"),
            ));
        }
        if !unique.insert(domain.clone()) {
            return Err(invalid(
                service,
                field,
                format!("duplicate domain `{domain}`"),
            ));
        }
    }
    Ok(())
}

fn validate_time_ranges(
    service: &str,
    field: &'static str,
    ranges: &mut [ForwardTimeRange],
) -> Result<(), ConfigError> {
    if ranges.len() > MAX_FORWARD_TIME_RANGES {
        return Err(invalid(
            service,
            field,
            format!("must contain at most {MAX_FORWARD_TIME_RANGES} time ranges"),
        ));
    }
    let mut unique = HashSet::with_capacity(ranges.len());
    for range in ranges {
        if range.days.is_empty() || range.days.len() > 7 {
            return Err(invalid(
                service,
                field,
                "days must contain 1..=7 unique weekdays",
            ));
        }
        range.days.sort_by_key(|day| weekday_order(*day));
        if range.days.windows(2).any(|days| days[0] == days[1]) {
            return Err(invalid(service, field, "days must contain unique weekdays"));
        }
        let start = parse_time(&range.start).ok_or_else(|| {
            invalid(
                service,
                field,
                "start must use canonical UTC HH:MM between 00:00 and 23:59",
            )
        })?;
        let end = parse_time(&range.end).ok_or_else(|| {
            invalid(
                service,
                field,
                "end must use canonical UTC HH:MM between 00:01 and 24:00",
            )
        })?;
        if start >= end || end > 24 * 60 {
            return Err(invalid(
                service,
                field,
                "time ranges must have start before end and end at or before 24:00",
            ));
        }
        range.start = format_time(start);
        range.end = format_time(end);
        if !unique.insert((range.days.clone(), start, end)) {
            return Err(invalid(service, field, "time ranges must be unique"));
        }
    }
    Ok(())
}

fn parse_time(value: &str) -> Option<u16> {
    if value.len() != 5 || !value.is_ascii() || value.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let hour = value[..2].parse::<u16>().ok()?;
    let minute = value[3..].parse::<u16>().ok()?;
    if hour > 24 || minute > 59 || (hour == 24 && minute != 0) {
        return None;
    }
    Some(hour * 60 + minute)
}

fn format_time(value: u16) -> String {
    format!("{:02}:{:02}", value / 60, value % 60)
}

const fn weekday_order(day: ForwardWeekday) -> u8 {
    match day {
        ForwardWeekday::Monday => 0,
        ForwardWeekday::Tuesday => 1,
        ForwardWeekday::Wednesday => 2,
        ForwardWeekday::Thursday => 3,
        ForwardWeekday::Friday => 4,
        ForwardWeekday::Saturday => 5,
        ForwardWeekday::Sunday => 6,
    }
}

fn validate_cidrs(
    service: &str,
    field: &'static str,
    cidrs: &mut [String],
) -> Result<(), ConfigError> {
    if cidrs.len() > MAX_FORWARD_CIDRS {
        return Err(invalid(
            service,
            field,
            format!("must contain at most {MAX_FORWARD_CIDRS} CIDRs"),
        ));
    }
    let mut unique = HashSet::with_capacity(cidrs.len());
    for cidr in cidrs {
        *cidr = normalize_cidr(cidr)
            .ok_or_else(|| invalid(service, field, format!("invalid canonical CIDR `{cidr}`")))?;
        if !unique.insert(cidr.clone()) {
            return Err(invalid(service, field, format!("duplicate CIDR `{cidr}`")));
        }
    }
    Ok(())
}

pub(crate) fn normalize_cidr(value: &str) -> Option<String> {
    let (address, prefix) = value.split_once('/')?;
    if prefix.is_empty() || prefix.starts_with('+') || (prefix.len() > 1 && prefix.starts_with('0'))
    {
        return None;
    }
    let address = address.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    match address {
        IpAddr::V4(address) if prefix <= 32 && ipv4_is_network(address, prefix) => {
            Some(format!("{address}/{prefix}"))
        }
        IpAddr::V6(address) if prefix <= 128 && ipv6_is_network(address, prefix) => {
            Some(format!("{address}/{prefix}"))
        }
        _ => None,
    }
}

fn ipv4_is_network(address: Ipv4Addr, prefix: u8) -> bool {
    let host_bits = 32 - u32::from(prefix);
    host_bits == 32 || u32::from(address) & ((1_u32 << host_bits) - 1) == 0
}

fn ipv6_is_network(address: Ipv6Addr, prefix: u8) -> bool {
    let host_bits = 128 - u32::from(prefix);
    host_bits == 128 || u128::from(address) & ((1_u128 << host_bits) - 1) == 0
}

fn validate_limit(
    service: &ForwardProxyService,
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return Err(invalid(
            &service.name,
            field,
            format!("must be between 1 and {maximum}"),
        ));
    }
    Ok(())
}

fn invalid(service: &str, field: &'static str, detail: impl Into<String>) -> ConfigError {
    ConfigError::InvalidForwardProxyService {
        service: service.into(),
        field,
        detail: detail.into(),
    }
}
