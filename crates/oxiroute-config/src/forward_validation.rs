use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use crate::{
    defaults::{
        MAX_FORWARD_BODY_BYTES, MAX_FORWARD_CIDRS, MAX_FORWARD_CONNECT_PORTS,
        MAX_FORWARD_CONNECTIONS, MAX_FORWARD_DOMAINS, MAX_FORWARD_HEADER_BYTES,
        MAX_FORWARD_PROXY_SERVICES, MAX_FORWARD_RESOLVER_ADDRESSES,
        MAX_FORWARD_RESOLVER_CACHE_ENTRIES, MAX_FORWARD_RESOLVER_CONCURRENT_QUERIES,
        MAX_FORWARD_TIMEOUT_MS,
    },
    lexical::{is_valid_certificate_dns_name, validate_file_path},
    model::{ConfigError, ForwardHttpVersion, ForwardProxyAuth, ForwardProxyService},
};

pub(crate) fn validate_forward_proxy_services(
    services: &mut [ForwardProxyService],
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
        validate_service(service)?;
    }
    Ok(())
}

fn validate_service(service: &mut ForwardProxyService) -> Result<(), ConfigError> {
    validate_versions_and_connect(service)?;
    if let Some(ForwardProxyAuth::BearerTokenFile { token_file_path }) = &service.auth {
        validate_file_path(
            "forward proxy service",
            &service.name,
            "auth.token_file_path",
            token_file_path,
        )?;
    }
    validate_destinations(service)?;
    validate_service_limits(service)
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

    if service.connect.allowed_ports.len() > MAX_FORWARD_CONNECT_PORTS
        || (service.connect.enabled && service.connect.allowed_ports.is_empty())
    {
        return Err(invalid(
            &service.name,
            "connect.allowed_ports",
            "must contain 1..=64 ports when CONNECT is enabled",
        ));
    }
    if service.connect.allowed_ports.contains(&0) {
        return Err(invalid(
            &service.name,
            "connect.allowed_ports",
            "must contain only nonzero ports",
        ));
    }
    service.connect.allowed_ports.sort_unstable();
    if service
        .connect
        .allowed_ports
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(invalid(
            &service.name,
            "connect.allowed_ports",
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
        if !is_valid_certificate_dns_name(domain) {
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
