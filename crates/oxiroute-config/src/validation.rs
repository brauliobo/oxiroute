use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use http::{Uri, uri::PathAndQuery};

use crate::{
    defaults::{
        MAX_ACME_CONTACTS, MAX_ACME_DIRECTORY_URL_BYTES, MAX_ACME_DNS_SUFFIXES,
        MAX_CERTIFICATE_DNS_NAMES, MAX_CERTIFICATES, MAX_ENDPOINTS_PER_POOL, MAX_HEALTH_HOST_BYTES,
        MAX_HEALTH_INTERVAL_MS, MAX_HEALTH_PATH_BYTES, MAX_HEALTH_THRESHOLD, MAX_HEALTH_TIMEOUT_MS,
        MAX_HTTP_TIMEOUT_MS, MAX_RECORDER_ACTIVE_RECORDERS, MAX_RECORDER_FILE_BYTES,
        MAX_RECORDER_FRAME_COUNT, MAX_RECORDER_QUEUE_BYTES, MAX_RECORDER_QUEUE_MESSAGES,
        MAX_RECORDER_ROTATION_INTERVAL_MS, MAX_RECORDER_SHUTDOWN_TIMEOUT_MS,
        MAX_RECORDER_STORAGE_BYTES, MAX_RECORDER_STORAGE_FILES,
        MAX_RTMP_ACCESS_RULES_PER_OPERATION, MAX_RTMP_APPLICATION_BYTES,
        MAX_RTMP_APPLICATION_CONNECTIONS, MAX_RTMP_APPLICATION_NAME_BYTES,
        MAX_RTMP_APPLICATION_PUBLISHERS, MAX_RTMP_APPLICATION_VIEWERS,
        MAX_RTMP_APPLICATIONS_PER_SERVICE, MAX_RTMP_FANOUT_QUEUE_BYTES,
        MAX_RTMP_FANOUT_QUEUE_MESSAGES, MAX_RTMP_OUTBOUND_CHUNK_SIZE, MAX_RTMP_PUSH_TARGETS,
        MAX_RTMP_RECORDERS_PER_APPLICATION, MAX_RTMP_RECORDING_ROOTS, MAX_RTMP_SERVICES,
        MAX_RTMP_SUBSCRIBERS, MAX_RTMP_TOKEN_BYTES, MAX_RTMP_TOKEN_PARAMETER_BYTES,
        MAX_RTMP_VOD_DURATION_MS, MAX_RTMP_VOD_FILE_BYTES, MAX_RTMP_VOD_ORIGIN_BYTES,
        MAX_RTMP_VOD_SESSIONS, MAX_RTMP_VOD_SOURCE_NAME_BYTES, MAX_RTMP_VOD_SOURCES,
        MAX_SAFE_JSON_INTEGER, MAX_SELF_SIGNED_VALIDITY_DAYS, MAX_TLS_PROFILES,
        MAX_TOTAL_ENDPOINTS, MAX_TOTAL_RTMP_RECORDERS, MAX_UDP_DATAGRAM_BYTES, MAX_UDP_QUEUE_BYTES,
        MAX_UDP_QUEUE_DATAGRAMS, MAX_UDP_SESSION_BYTES, MAX_UDP_SESSIONS, MAX_UPSTREAM_WEIGHT,
        MIN_HEALTH_INTERVAL_MS, MIN_SELF_SIGNED_VALIDITY_DAYS,
    },
    lexical::{
        authority_has_invalid_port, canonical_ip, is_unambiguous_http_path,
        is_valid_certificate_dns_name, is_valid_dns_name, normalize_absolute_directory,
        normalize_listener_binds, normalize_recording_root, normalize_upstream_endpoint,
        normalize_upstream_endpoints, normalize_upstream_server_names, validate_directory_path,
        validate_file_path, validate_recording_suffix_template,
    },
    model::{
        AccessLogPolicy, AlpnProtocol, Certificate, CertificateSource, Config, ConfigError,
        DnsResolutionPolicy, ForwardHttpVersion, ForwardProxyService, HealthCheck, HealthCheckType,
        HttpVersion, L4Service, Listener, ListenerBind, Management, Protocol, RtmpAccessPolicy,
        RtmpRecorder, RtmpService, RtmpSessionCeilings, RtmpTokenSource, RtmpVodSource, Stats,
        StatsPage, TlsProfile, TlsVersion, UdpPolicy, UpstreamAlgorithm, UpstreamEndpoint,
        UpstreamPool,
    },
};

/// Validates and normalizes a complete configuration regardless of how it was constructed.
///
/// # Errors
///
/// Returns an error when any configured value or cross-reference is invalid.
pub fn validate_config(config: &mut Config) -> Result<(), ConfigError> {
    if config.version != 1 {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }
    validate_optional_safe_limit(
        "configuration",
        "root",
        "max_connections",
        config.max_connections,
    )?;

    validate_management(config.management.as_ref())?;
    validate_stats(config.stats.as_ref())?;
    validate_certificates(&mut config.certificates)?;
    validate_tls_profiles(&config.tls_profiles, &config.certificates)?;
    let cache_stores = crate::cache_validation::validate_cache_stores(&mut config.cache_stores)?;
    crate::forward_validation::validate_forward_proxy_services(&mut config.forward_proxy_services)?;
    validate_config_names(config)?;
    validate_rtmp_services(&mut config.rtmp_services)?;

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
    let rtmp_service_names = config
        .rtmp_services
        .iter()
        .map(|service| service.name.clone())
        .collect::<HashSet<_>>();
    let tls_profile_names = config
        .tls_profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect::<HashSet<_>>();
    let forward_proxy_services = config
        .forward_proxy_services
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect::<HashMap<_, _>>();
    let tls_profiles = config
        .tls_profiles
        .iter()
        .map(|profile| (profile.name.as_str(), profile))
        .collect::<HashMap<_, _>>();

    normalize_listener_binds(&mut config.listeners)?;
    validate_listeners(
        &config.listeners,
        &http_service_names,
        &rtmp_service_names,
        &l4_service_names,
        &tls_profile_names,
        &forward_proxy_services,
        &tls_profiles,
    )?;
    validate_bind_conflicts(
        config.management.as_ref(),
        config.stats.as_ref(),
        &config.listeners,
    )?;
    normalize_upstream_endpoints(&mut config.upstream_pools)?;
    normalize_upstream_server_names(&mut config.upstream_pools);
    validate_upstream_pools(
        &config.upstream_pools,
        config.management.as_ref().map(|management| management.bind),
    )?;
    crate::http_validation::validate_http_services(
        &mut config.http_services,
        &config.upstream_pools,
        &cache_stores,
    )?;
    let tls_upstream_pool_names = config
        .upstream_pools
        .iter()
        .filter(|pool| pool.tls.is_some())
        .map(|pool| pool.name.clone())
        .collect::<HashSet<_>>();
    validate_l4_services(
        &config.l4_services,
        &upstream_pool_names,
        &tls_upstream_pool_names,
    )?;

    Ok(())
}

fn validate_config_names(config: &Config) -> Result<(), ConfigError> {
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
    validate_names(
        "RTMP service",
        config
            .rtmp_services
            .iter()
            .map(|service| service.name.as_str()),
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_certificates(certificates: &mut [Certificate]) -> Result<(), ConfigError> {
    validate_names(
        "certificate",
        certificates
            .iter()
            .map(|certificate| certificate.name.as_str()),
    )?;
    if certificates.len() > MAX_CERTIFICATES {
        return Err(ConfigError::TooManyCertificates);
    }

    for certificate in certificates {
        if certificate.dns_names.is_empty() {
            return Err(ConfigError::EmptyCertificateDnsNames {
                certificate: certificate.name.clone(),
            });
        }
        if certificate.dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
            return Err(ConfigError::TooManyCertificateDnsNames {
                certificate: certificate.name.clone(),
            });
        }
        let mut unique_dns_names = HashSet::with_capacity(certificate.dns_names.len());
        for dns_name in &mut certificate.dns_names {
            if let Ok(ip) = dns_name.parse::<IpAddr>() {
                *dns_name = canonical_ip(ip).to_string();
            } else {
                dns_name.make_ascii_lowercase();
            }
            if !is_valid_certificate_dns_name(dns_name) {
                return Err(ConfigError::InvalidCertificateDnsName {
                    certificate: certificate.name.clone(),
                    dns_name: dns_name.clone(),
                });
            }
            if !unique_dns_names.insert(dns_name.clone()) {
                return Err(ConfigError::DuplicateCertificateDnsName {
                    certificate: certificate.name.clone(),
                    dns_name: dns_name.clone(),
                });
            }
        }

        match &certificate.source {
            CertificateSource::Files {
                certificate_chain_path,
                private_key_path,
            } => {
                validate_file_path(
                    "certificate",
                    &certificate.name,
                    "source.certificate_chain_path",
                    certificate_chain_path,
                )?;
                validate_file_path(
                    "certificate",
                    &certificate.name,
                    "source.private_key_path",
                    private_key_path,
                )?;
                if certificate_chain_path == private_key_path {
                    return Err(ConfigError::DuplicateCertificatePaths {
                        certificate: certificate.name.clone(),
                    });
                }
            }
            CertificateSource::Certbot {
                live_directory_path,
                archive_directory_path,
            } => {
                validate_directory_path(
                    "certificate",
                    &certificate.name,
                    "source.live_directory_path",
                    live_directory_path,
                )?;
                validate_directory_path(
                    "certificate",
                    &certificate.name,
                    "source.archive_directory_path",
                    archive_directory_path,
                )?;
                if live_directory_path == archive_directory_path {
                    return Err(ConfigError::DuplicateCertbotDirectories {
                        certificate: certificate.name.clone(),
                    });
                }
            }
            CertificateSource::AcmeManaged {
                directory_url,
                state_root,
                contacts,
                terms_agreed,
                challenge,
                allowed_dns_suffixes,
                ..
            } => {
                validate_acme_source(
                    certificate,
                    directory_url,
                    state_root,
                    contacts,
                    *terms_agreed,
                    *challenge,
                    allowed_dns_suffixes,
                )?;
            }
            CertificateSource::SelfSignedDevelopment { validity_days, .. } => {
                if !(MIN_SELF_SIGNED_VALIDITY_DAYS..=MAX_SELF_SIGNED_VALIDITY_DAYS)
                    .contains(validity_days)
                {
                    return Err(ConfigError::InvalidSelfSignedValidityDays {
                        certificate: certificate.name.clone(),
                        value: *validity_days,
                        min: MIN_SELF_SIGNED_VALIDITY_DAYS,
                        max: MAX_SELF_SIGNED_VALIDITY_DAYS,
                    });
                }
            }
        }
    }

    Ok(())
}

fn validate_acme_source(
    certificate: &Certificate,
    directory_url: &str,
    state_root: &Path,
    contacts: &[String],
    terms_agreed: bool,
    challenge: crate::model::AcmeChallengeType,
    allowed_dns_suffixes: &[String],
) -> Result<(), ConfigError> {
    if !certificate
        .name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || certificate.name == "."
        || certificate.name == ".."
        || certificate.name.len() > 128
    {
        return Err(ConfigError::InvalidAcmeCertificateName {
            certificate: certificate.name.clone(),
        });
    }
    let parsed_directory_url = directory_url.parse::<Uri>().ok();
    if directory_url.len() > MAX_ACME_DIRECTORY_URL_BYTES
        || !directory_url.is_ascii()
        || !directory_url.starts_with("https://")
        || directory_url.contains('@')
        || directory_url.contains('#')
        || parsed_directory_url.as_ref().is_none_or(|url| {
            url.scheme_str() != Some("https")
                || url
                    .authority()
                    .is_none_or(|authority| authority.host().is_empty())
        })
    {
        return Err(ConfigError::InvalidAcmeDirectoryUrl {
            certificate: certificate.name.clone(),
        });
    }
    validate_directory_path(
        "certificate",
        &certificate.name,
        "source.state_root",
        state_root,
    )?;
    if !terms_agreed {
        return Err(ConfigError::AcmeTermsNotAgreed {
            certificate: certificate.name.clone(),
        });
    }
    if !matches!(challenge, crate::model::AcmeChallengeType::Http01) {
        return Err(ConfigError::UnsupportedAcmeChallenge {
            certificate: certificate.name.clone(),
        });
    }
    if contacts.len() > MAX_ACME_CONTACTS
        || contacts.iter().any(|contact| {
            contact.is_empty()
                || contact.len() > 320
                || !contact.is_ascii()
                || !contact.starts_with("mailto:")
        })
    {
        return Err(ConfigError::InvalidAcmeContacts {
            certificate: certificate.name.clone(),
        });
    }
    if allowed_dns_suffixes.is_empty() || allowed_dns_suffixes.len() > MAX_ACME_DNS_SUFFIXES {
        return Err(ConfigError::InvalidAcmeDnsSuffixes {
            certificate: certificate.name.clone(),
        });
    }
    let mut suffixes = HashSet::with_capacity(allowed_dns_suffixes.len());
    for suffix in allowed_dns_suffixes {
        let suffix = suffix.trim().to_ascii_lowercase();
        if suffix.is_empty()
            || suffix.starts_with("*.")
            || suffix.parse::<IpAddr>().is_ok()
            || !is_valid_certificate_dns_name(&suffix)
            || !suffixes.insert(suffix)
        {
            return Err(ConfigError::InvalidAcmeDnsSuffixes {
                certificate: certificate.name.clone(),
            });
        }
    }
    for dns_name in &certificate.dns_names {
        if dns_name.starts_with("*.") || dns_name.parse::<IpAddr>().is_ok() {
            return Err(ConfigError::AcmeIdentifierUnsupported {
                certificate: certificate.name.clone(),
            });
        }
        if !suffixes
            .iter()
            .any(|suffix| dns_name == suffix || dns_name.ends_with(&format!(".{suffix}")))
        {
            return Err(ConfigError::AcmeIdentifierOutsidePolicy {
                certificate: certificate.name.clone(),
                dns_name: dns_name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_tls_profiles(
    tls_profiles: &[TlsProfile],
    certificates: &[Certificate],
) -> Result<(), ConfigError> {
    validate_names(
        "TLS profile",
        tls_profiles.iter().map(|profile| profile.name.as_str()),
    )?;
    if tls_profiles.len() > MAX_TLS_PROFILES {
        return Err(ConfigError::TooManyTlsProfiles);
    }

    let certificates_by_name = certificates
        .iter()
        .map(|certificate| (certificate.name.as_str(), certificate))
        .collect::<HashMap<_, _>>();
    for profile in tls_profiles {
        if profile.certificates.is_empty() {
            return Err(ConfigError::EmptyTlsProfileCertificates {
                profile: profile.name.clone(),
            });
        }

        let mut referenced_certificates = HashSet::with_capacity(profile.certificates.len());
        let mut dns_name_owners = HashMap::new();
        for certificate_name in &profile.certificates {
            if !referenced_certificates.insert(certificate_name.as_str()) {
                return Err(ConfigError::DuplicateTlsProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                });
            }
            let certificate = certificates_by_name
                .get(certificate_name.as_str())
                .ok_or_else(|| ConfigError::UnknownTlsProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                })?;
            for dns_name in &certificate.dns_names {
                if dns_name.parse::<IpAddr>().is_ok() {
                    continue;
                }
                if let Some(first_certificate) =
                    dns_name_owners.insert(dns_name.as_str(), certificate.name.as_str())
                {
                    return Err(ConfigError::OverlappingTlsProfileDnsName {
                        profile: profile.name.clone(),
                        dns_name: dns_name.clone(),
                        first_certificate: first_certificate.into(),
                        second_certificate: certificate.name.clone(),
                    });
                }
            }
        }
        if !referenced_certificates.contains(profile.default_certificate.as_str()) {
            return Err(ConfigError::TlsProfileDefaultNotListed {
                profile: profile.name.clone(),
                certificate: profile.default_certificate.clone(),
            });
        }
        if !matches!(
            profile.alpn.as_slice(),
            [AlpnProtocol::Http11 | AlpnProtocol::H2 | AlpnProtocol::H3]
                | [AlpnProtocol::H2, AlpnProtocol::Http11]
        ) {
            return Err(ConfigError::InvalidTlsProfileAlpn {
                profile: profile.name.clone(),
            });
        }
        validate_tls_policy(profile)?;
    }

    Ok(())
}

fn validate_tls_policy(profile: &TlsProfile) -> Result<(), ConfigError> {
    if profile
        .policy
        .cipher_list
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.as_bytes().contains(&0))
    {
        return Err(ConfigError::InvalidTlsProfilePolicy {
            profile: profile.name.clone(),
            field: "cipher_list",
            detail: "must be nonempty and contain no NUL bytes",
        });
    }
    if let Some(path) = &profile.policy.dh_parameters_path {
        validate_file_path(
            "TLS profile",
            &profile.name,
            "policy.dh_parameters_path",
            path,
        )?;
    }
    if let Some(cache) = &profile.policy.session_cache {
        if cache.name.is_empty()
            || cache.name.len() > 255
            || !cache
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(ConfigError::InvalidTlsProfilePolicy {
                profile: profile.name.clone(),
                field: "session_cache.name",
                detail: "must be 1 through 255 ASCII letters, digits, `_`, `-`, or `.`",
            });
        }
        if !(256..=u64::from(i32::MAX as u32) * 256).contains(&cache.size_bytes) {
            return Err(ConfigError::InvalidTlsProfilePolicy {
                profile: profile.name.clone(),
                field: "session_cache.size_bytes",
                detail: "must hold between 1 and i32::MAX estimated 256-byte sessions",
            });
        }
    }
    if profile
        .policy
        .session_timeout_seconds
        .is_some_and(|seconds| seconds == 0 || seconds > u64::from(i32::MAX as u32))
    {
        return Err(ConfigError::InvalidTlsProfilePolicy {
            profile: profile.name.clone(),
            field: "session_timeout_seconds",
            detail: "must be between 1 and i32::MAX seconds",
        });
    }
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

fn validate_stats(stats: Option<&Stats>) -> Result<(), ConfigError> {
    let Some(stats) = stats else {
        return Ok(());
    };
    if stats.binds.len() + stats.pages.len() == 0 || stats.binds.len() + stats.pages.len() > 8 {
        return Err(ConfigError::InvalidStatsBinds);
    }
    for bind in &stats.binds {
        if bind.port() == 0 {
            return Err(ConfigError::ZeroPort {
                kind: "statistics listener",
                name: "stats".into(),
                field: "binds",
            });
        }
    }
    if let Some(path) = &stats.admin_token_file {
        validate_file_path("statistics", "stats", "admin_token_file", path)?;
    }
    for (index, page) in stats.pages.iter().enumerate() {
        validate_stats_page(index, page)?;
    }
    Ok(())
}

fn validate_stats_page(index: usize, page: &StatsPage) -> Result<(), ConfigError> {
    let page_name = format!("page-{index}");
    if page.bind.port() == 0 {
        return Err(ConfigError::ZeroPort {
            kind: "statistics page",
            name: page_name.clone(),
            field: "bind",
        });
    }
    if page.uri_prefix.len() > crate::defaults::MAX_STATS_PAGE_URI_BYTES {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "uri_prefix",
            detail: "URI prefix exceeds 2048 bytes",
        });
    }
    let path_and_query = page.uri_prefix.parse::<PathAndQuery>();
    if !page.uri_prefix.is_ascii()
        || page.uri_prefix.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(
                    byte,
                    b'?' | b'#'
                        | b'<'
                        | b'>'
                        | b'['
                        | b']'
                        | b'\\'
                        | b'^'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                )
        })
        || !path_and_query.is_ok_and(|path| {
            path.query().is_none()
                && path.path() == page.uri_prefix
                && is_unambiguous_http_path(path.path())
        })
    {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "uri_prefix",
            detail: "URI prefix must be an absolute unambiguous HTTP path without a query or fragment",
        });
    }
    if page.refresh_ms == 0 || page.refresh_ms > crate::defaults::MAX_STATS_PAGE_REFRESH_MS {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "refresh_ms",
            detail: "refresh must be between 1 and 86400000 milliseconds",
        });
    }
    if page.max_connections == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind: "statistics page",
            name: page_name.clone(),
            field: "max_connections",
        });
    }
    if let Some(max_connections) = page.max_connections {
        validate_safe_integer(
            "statistics page",
            &page_name,
            "max_connections",
            max_connections,
        )?;
    }
    for (field, timeout) in [
        (
            "downstream_timeouts.client_timeout_ms",
            page.downstream_timeouts.client_timeout_ms,
        ),
        (
            "downstream_timeouts.request_timeout_ms",
            page.downstream_timeouts.request_timeout_ms,
        ),
        (
            "downstream_timeouts.keepalive_timeout_ms",
            page.downstream_timeouts.keepalive_timeout_ms,
        ),
    ] {
        if timeout.is_some_and(|value| value == 0 || value > MAX_HTTP_TIMEOUT_MS) {
            return Err(ConfigError::InvalidStatsPage {
                page: index,
                field,
                detail: "downstream timeouts must be between 1 and 86400000 milliseconds",
            });
        }
        if let Some(timeout) = timeout {
            validate_safe_integer("statistics page", &page_name, field, timeout)?;
        }
    }
    Ok(())
}

fn validate_bind_conflicts(
    management: Option<&Management>,
    stats: Option<&Stats>,
    listeners: &[Listener],
) -> Result<(), ConfigError> {
    let mut binds = Vec::with_capacity(
        listeners.len()
            + usize::from(management.is_some())
            + stats.map_or(0, |stats| stats.binds.len() + stats.pages.len()),
    );
    if let Some(management) = management {
        binds.push((
            "management".to_owned(),
            ListenerBind::Socket {
                address: management.bind,
            },
        ));
    }
    if let Some(stats) = stats {
        for (index, address) in stats.binds.iter().enumerate() {
            let bind = ListenerBind::Socket { address: *address };
            for (first_name, first_bind) in &binds {
                if binds_overlap(first_bind, &bind) {
                    return Err(ConfigError::OverlappingBind {
                        first_name: first_name.clone(),
                        first_bind: Box::new(first_bind.clone()),
                        second_name: format!("stats-{index}"),
                        second_bind: Box::new(bind),
                    });
                }
            }
            binds.push((format!("stats-{index}"), bind));
        }
        for (index, page) in stats.pages.iter().enumerate() {
            let bind = ListenerBind::Socket { address: page.bind };
            for (first_name, first_bind) in &binds {
                if binds_overlap(first_bind, &bind) {
                    return Err(ConfigError::OverlappingBind {
                        first_name: first_name.clone(),
                        first_bind: Box::new(first_bind.clone()),
                        second_name: format!("stats-page-{index}"),
                        second_bind: Box::new(bind),
                    });
                }
            }
            binds.push((format!("stats-page-{index}"), bind));
        }
    }

    for listener in listeners {
        for (first_name, first_bind) in &binds {
            if binds_overlap(first_bind, &listener.bind) {
                return Err(ConfigError::OverlappingBind {
                    first_name: first_name.clone(),
                    first_bind: Box::new(first_bind.clone()),
                    second_name: listener.name.clone(),
                    second_bind: Box::new(listener.bind.clone()),
                });
            }
        }
        binds.push((listener.name.clone(), listener.bind.clone()));
    }

    Ok(())
}

fn binds_overlap(first: &ListenerBind, second: &ListenerBind) -> bool {
    match (first, second) {
        (
            ListenerBind::Socket {
                address: first_address,
            }
            | ListenerBind::Udp {
                address: first_address,
            },
            ListenerBind::Socket {
                address: second_address,
            }
            | ListenerBind::Udp {
                address: second_address,
            },
        ) if matches!(
            (first, second),
            (ListenerBind::Socket { .. }, ListenerBind::Socket { .. })
                | (ListenerBind::Udp { .. }, ListenerBind::Udp { .. })
        ) =>
        {
            let first_ip = canonical_ip(first_address.ip());
            let second_ip = canonical_ip(second_address.ip());
            first_address.port() == second_address.port()
                && (first_ip == second_ip
                    || first_ip.is_unspecified()
                    || second_ip.is_unspecified())
        }
        (ListenerBind::Unix { path: first, .. }, ListenerBind::Unix { path: second, .. }) => {
            first == second
        }
        _ => false,
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
    rtmp_service_names: &HashSet<String>,
    l4_service_names: &HashSet<String>,
    tls_profile_names: &HashSet<String>,
    forward_proxy_services: &HashMap<&str, &ForwardProxyService>,
    tls_profiles: &HashMap<&str, &TlsProfile>,
) -> Result<(), ConfigError> {
    for listener in listeners {
        validate_listener_basics(listener, tls_profile_names)?;
        match (listener.protocol, listener.service.as_deref()) {
            (
                Protocol::Http
                | Protocol::Rtmp
                | Protocol::Tcp
                | Protocol::Udp
                | Protocol::ForwardHttp1
                | Protocol::ForwardHttp2
                | Protocol::ForwardHttp3,
                None,
            ) => {
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
            (Protocol::Tcp | Protocol::Udp, Some(service))
                if !l4_service_names.contains(service) =>
            {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (Protocol::Rtmp, Some(service)) if !rtmp_service_names.contains(service) => {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (
                Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3,
                Some(service),
            ) => {
                let forward_service = forward_proxy_services.get(service).ok_or_else(|| {
                    ConfigError::UnknownListenerService {
                        listener: listener.name.clone(),
                        protocol: listener.protocol,
                        service: service.into(),
                    }
                })?;
                validate_forward_listener(listener, forward_service, tls_profiles)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_listener_basics(
    listener: &Listener,
    tls_profile_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    if matches!(
        &listener.bind,
        ListenerBind::Socket { address } | ListenerBind::Udp { address }
            if address.port() == 0
    ) {
        return Err(ConfigError::ZeroPort {
            kind: "listener",
            name: listener.name.clone(),
            field: "bind",
        });
    }
    validate_listener_policies(listener)?;
    if let (ListenerBind::Unix { .. }, Some(profile)) =
        (&listener.bind, listener.tls_profile.as_deref())
    {
        return Err(ConfigError::UnsupportedUnixListenerTls {
            listener: listener.name.clone(),
            profile: profile.into(),
        });
    }

    let datagram = matches!(listener.bind, ListenerBind::Udp { .. });
    let datagram_protocol = matches!(listener.protocol, Protocol::ForwardHttp3 | Protocol::Udp);
    if datagram != datagram_protocol {
        return Err(ConfigError::InvalidListenerTransport {
            listener: listener.name.clone(),
            protocol: listener.protocol,
            detail: if datagram_protocol {
                "this protocol requires a UDP bind"
            } else {
                "UDP binds require forward_http3 or udp"
            },
        });
    }

    match (listener.protocol, listener.tls_profile.as_deref()) {
        (Protocol::Http, Some(profile)) if !tls_profile_names.contains(profile) => {
            Err(ConfigError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile.into(),
            })
        }
        (
            Protocol::Http
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3,
            _,
        )
        | (_, None) => Ok(()),
        (protocol, Some(profile)) => Err(ConfigError::UnexpectedListenerTlsProfile {
            listener: listener.name.clone(),
            protocol,
            profile: profile.into(),
        }),
    }
}

fn validate_listener_policies(listener: &Listener) -> Result<(), ConfigError> {
    if listener.max_connections == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind: "listener",
            name: listener.name.clone(),
            field: "max_connections",
        });
    }
    if let Some(max_connections) = listener.max_connections {
        validate_safe_integer(
            "listener",
            &listener.name,
            "max_connections",
            max_connections,
        )?;
    }
    if let ListenerBind::Unix {
        mode: Some(mode), ..
    } = listener.bind
    {
        if mode == 0 || mode > 0o777 {
            return Err(ConfigError::InvalidListenerUnixMode {
                listener: listener.name.clone(),
                mode,
            });
        }
    }
    for (field, timeout) in [
        (
            "downstream_timeouts.client_timeout_ms",
            listener.downstream_timeouts.client_timeout_ms,
        ),
        (
            "downstream_timeouts.request_timeout_ms",
            listener.downstream_timeouts.request_timeout_ms,
        ),
        (
            "downstream_timeouts.keepalive_timeout_ms",
            listener.downstream_timeouts.keepalive_timeout_ms,
        ),
    ] {
        if timeout.is_some_and(|value| value == 0 || value > MAX_HTTP_TIMEOUT_MS) {
            return Err(ConfigError::InvalidListenerTransport {
                listener: listener.name.clone(),
                protocol: listener.protocol,
                detail: "downstream timeouts must be between 1 and 86400000 milliseconds",
            });
        }
        if let Some(timeout) = timeout {
            validate_safe_integer("listener", &listener.name, field, timeout)?;
        }
    }
    if !matches!(
        listener.protocol,
        Protocol::Http | Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3
    ) && (listener.downstream_timeouts.request_timeout_ms.is_some()
        || listener.downstream_timeouts.keepalive_timeout_ms.is_some())
    {
        return Err(ConfigError::InvalidListenerTransport {
            listener: listener.name.clone(),
            protocol: listener.protocol,
            detail: "request and keepalive timeouts apply only to HTTP listeners",
        });
    }
    Ok(())
}

fn validate_forward_listener(
    listener: &Listener,
    service: &ForwardProxyService,
    tls_profiles: &HashMap<&str, &TlsProfile>,
) -> Result<(), ConfigError> {
    let version = match listener.protocol {
        Protocol::ForwardHttp1 => ForwardHttpVersion::H1,
        Protocol::ForwardHttp2 => ForwardHttpVersion::H2,
        Protocol::ForwardHttp3 => ForwardHttpVersion::H3,
        Protocol::Http | Protocol::Rtmp | Protocol::Tcp | Protocol::Udp => return Ok(()),
    };
    let invalid = |detail: String| ConfigError::InvalidForwardProxyListener {
        listener: listener.name.clone(),
        detail,
    };

    if listener.downstream_timeouts.client_timeout_ms.is_some()
        || listener.downstream_timeouts.keepalive_timeout_ms.is_some()
    {
        return Err(invalid(
            "forward HTTP/1 currently accepts only the request-header downstream timeout".into(),
        ));
    }

    if !service.enabled_versions.contains(&version) {
        return Err(invalid(format!(
            "referenced service `{}` does not enable {version:?}",
            service.name
        )));
    }
    if matches!(listener.bind, ListenerBind::Unix { .. }) && service.tls_required {
        return Err(invalid(
            "a Unix listener requires its service to set tls_required = false".into(),
        ));
    }
    let tls_required = version == ForwardHttpVersion::H3
        || (service.tls_required && !matches!(listener.bind, ListenerBind::Unix { .. }));
    if tls_required && listener.tls_profile.is_none() {
        return Err(invalid("this listener requires a TLS profile".into()));
    }
    let Some(profile_name) = listener.tls_profile.as_deref() else {
        return Ok(());
    };
    if version == ForwardHttpVersion::H1 {
        return Err(invalid(
            "forward_http1 does not support downstream TLS yet".into(),
        ));
    }
    let profile =
        tls_profiles
            .get(profile_name)
            .ok_or_else(|| ConfigError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile_name.into(),
            })?;
    let required_alpn = match version {
        ForwardHttpVersion::H1 => AlpnProtocol::Http11,
        ForwardHttpVersion::H2 => AlpnProtocol::H2,
        ForwardHttpVersion::H3 => AlpnProtocol::H3,
    };
    if !profile.alpn.contains(&required_alpn) {
        return Err(invalid(format!(
            "TLS profile `{profile_name}` does not advertise the required ALPN"
        )));
    }
    if version == ForwardHttpVersion::H3
        && (profile.min_version != TlsVersion::Tls13
            || profile.alpn.as_slice() != [AlpnProtocol::H3])
    {
        return Err(invalid(
            "forward_http3 requires a TLS 1.3 profile advertising only h3".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RtmpRecorderStorageLimits {
    bytes: Option<u64>,
    files: Option<u64>,
    active_recorders: u64,
}

fn validate_rtmp_services(services: &mut [RtmpService]) -> Result<(), ConfigError> {
    if services.len() > MAX_RTMP_SERVICES {
        return Err(ConfigError::TooManyRtmpServices);
    }
    let mut total_recorders = 0_usize;
    let mut roots = HashMap::<PathBuf, (RtmpRecorderStorageLimits, String)>::new();
    for service in services {
        if service.outbound_chunk_size == 0
            || service.outbound_chunk_size > MAX_RTMP_OUTBOUND_CHUNK_SIZE
        {
            return Err(ConfigError::InvalidRtmpServicePolicy {
                service: service.name.clone(),
                field: "outbound_chunk_size",
                detail: "must be between 1 and 1048576",
            });
        }
        validate_access_log("RTMP service", &service.name, service.access_log.as_ref())?;
        if service.applications.is_empty() {
            return Err(ConfigError::EmptyRtmpApplications {
                service: service.name.clone(),
            });
        }
        if service.applications.len() > MAX_RTMP_APPLICATIONS_PER_SERVICE {
            return Err(ConfigError::TooManyRtmpApplications {
                service: service.name.clone(),
            });
        }
        validate_names(
            "RTMP application",
            service
                .applications
                .iter()
                .map(|application| application.name.as_str()),
        )?;
        for application in &mut service.applications {
            if application.name.len() > MAX_RTMP_APPLICATION_NAME_BYTES {
                return Err(ConfigError::InvalidRtmpApplicationPolicy {
                    service: service.name.clone(),
                    application: application.name.clone(),
                    field: "name",
                    detail: "must be between 1 and 128 bytes",
                });
            }
            validate_rtmp_application(&service.name, application)?;
            if application.recorders.len() > MAX_RTMP_RECORDERS_PER_APPLICATION {
                return Err(ConfigError::TooManyRtmpRecorders {
                    service: service.name.clone(),
                    application: application.name.clone(),
                });
            }
            validate_names(
                "RTMP recorder",
                application
                    .recorders
                    .iter()
                    .map(|recorder| recorder.name.as_str()),
            )?;
            if !application.live {
                if let Some(recorder) = application.recorders.first() {
                    return Err(ConfigError::RtmpRecorderRequiresLiveApplication {
                        service: service.name.clone(),
                        application: application.name.clone(),
                        recorder: recorder.name.clone(),
                    });
                }
            }
            for recorder in &mut application.recorders {
                total_recorders = total_recorders
                    .checked_add(1)
                    .ok_or(ConfigError::TooManyTotalRtmpRecorders)?;
                if total_recorders > MAX_TOTAL_RTMP_RECORDERS {
                    return Err(ConfigError::TooManyTotalRtmpRecorders);
                }
                validate_rtmp_recorder(&service.name, &application.name, recorder)?;

                let limits = RtmpRecorderStorageLimits {
                    bytes: recorder.max_storage_bytes,
                    files: recorder.max_storage_files,
                    active_recorders: recorder.max_active_recorders,
                };
                let identity = format!("{}/{}/{}", service.name, application.name, recorder.name);
                if let Some((first_limits, first_recorder)) = roots.get(&recorder.root_directory) {
                    if *first_limits != limits {
                        return Err(ConfigError::RtmpRecorderStorageLimitsMismatch {
                            root_directory: recorder.root_directory.display().to_string(),
                            first_recorder: first_recorder.clone(),
                            second_recorder: identity,
                        });
                    }
                } else {
                    if roots.len() >= MAX_RTMP_RECORDING_ROOTS {
                        return Err(ConfigError::TooManyRtmpRecordingRoots);
                    }
                    roots.insert(recorder.root_directory.clone(), (limits, identity));
                }
            }
        }
    }
    Ok(())
}

fn validate_rtmp_application(
    service: &str,
    application: &mut crate::model::RtmpApplication,
) -> Result<(), ConfigError> {
    validate_rtmp_vod(service, application)?;
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.name.clone(),
        field,
        detail,
    };
    if application.push_targets.len() > MAX_RTMP_PUSH_TARGETS {
        return Err(invalid("push_targets", "must contain at most 16 targets"));
    }
    if !application.live && !application.push_targets.is_empty() {
        return Err(invalid("push_targets", "requires live = true"));
    }
    validate_rtmp_access_policy(service, application, "publish", &application.publish)?;
    validate_rtmp_access_policy(service, application, "play", &application.play)?;
    validate_rtmp_session_ceilings(service, application, &application.limits)?;
    let mut targets = HashSet::with_capacity(application.push_targets.len());
    for target in &mut application.push_targets {
        target.host.make_ascii_lowercase();
        if target.port == 0 {
            return Err(invalid("push_targets[].port", "must be nonzero"));
        }
        if !is_valid_dns_name(&target.host) && target.host.parse::<std::net::IpAddr>().is_err() {
            return Err(invalid(
                "push_targets[].host",
                "must be an IP address or canonical DNS name",
            ));
        }
        if target.application.is_empty()
            || target.application.len() > MAX_RTMP_APPLICATION_BYTES
            || target.application.contains('$') && target.application != "$name"
            || target
                .application
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'?' | b'#'))
        {
            return Err(invalid(
                "push_targets[].application",
                "must be $name or 1..=255 literal bytes without $, separators, query, fragment, or controls",
            ));
        }
        if !targets.insert((&target.host, target.port, &target.application)) {
            return Err(invalid("push_targets", "must not contain duplicates"));
        }
    }
    let fanout = application.fanout;
    if fanout.max_subscribers == 0 || fanout.max_subscribers > MAX_RTMP_SUBSCRIBERS {
        return Err(invalid(
            "fanout.max_subscribers",
            "must be between 1 and 1000000",
        ));
    }
    if fanout.max_queue_messages_per_subscriber == 0
        || fanout.max_queue_messages_per_subscriber > MAX_RTMP_FANOUT_QUEUE_MESSAGES
    {
        return Err(invalid(
            "fanout.max_queue_messages_per_subscriber",
            "must be between 1 and 65536",
        ));
    }
    if fanout.max_queue_bytes_per_subscriber == 0
        || fanout.max_queue_bytes_per_subscriber > MAX_RTMP_FANOUT_QUEUE_BYTES
    {
        return Err(invalid(
            "fanout.max_queue_bytes_per_subscriber",
            "must be between 1 and 1073741824",
        ));
    }
    Ok(())
}

fn validate_rtmp_vod(
    service: &str,
    application: &mut crate::model::RtmpApplication,
) -> Result<(), ConfigError> {
    let Some(vod) = &mut application.vod else {
        return Ok(());
    };
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.name.clone(),
        field,
        detail,
    };
    if vod.sources.is_empty() || vod.sources.len() > MAX_RTMP_VOD_SOURCES {
        return Err(invalid(
            "vod.sources",
            "must contain between 1 and 16 sources",
        ));
    }
    if vod.max_sessions == 0 || vod.max_sessions > MAX_RTMP_VOD_SESSIONS {
        return Err(invalid("vod.max_sessions", "must be between 1 and 1024"));
    }
    if vod.max_file_bytes == 0 || vod.max_file_bytes > MAX_RTMP_VOD_FILE_BYTES {
        return Err(invalid(
            "vod.max_file_bytes",
            "must be between 1 and 1073741824",
        ));
    }
    if vod.max_duration_ms == 0 || vod.max_duration_ms > MAX_RTMP_VOD_DURATION_MS {
        return Err(invalid(
            "vod.max_duration_ms",
            "must be between 1 and 86400000",
        ));
    }
    let mut names = HashSet::with_capacity(vod.sources.len());
    for source in &mut vod.sources {
        let name = match source {
            RtmpVodSource::Local { name, .. } | RtmpVodSource::Http { name, .. } => name,
        };
        if name.is_empty()
            || name.len() > MAX_RTMP_VOD_SOURCE_NAME_BYTES
            || !valid_rtmp_vod_component(name)
            || !name.bytes().all(|byte| byte.is_ascii_graphic())
            || !names.insert(name.clone())
        {
            return Err(invalid(
                "vod.sources[].name",
                "must be unique and one nonempty path component of at most 128 bytes",
            ));
        }
        match source {
            RtmpVodSource::Local { root_directory, .. } => {
                validate_directory_path(
                    "RTMP VOD",
                    &application.name,
                    "vod.sources[].root_directory",
                    root_directory,
                )
                .map_err(|_| {
                    invalid(
                        "vod.sources[].root_directory",
                        "must be an absolute directory path",
                    )
                })?;
                normalize_absolute_directory(root_directory)
                    .map_err(|detail| invalid("vod.sources[].root_directory", detail))?;
            }
            RtmpVodSource::Http { origin, .. } => {
                validate_rtmp_vod_origin(origin, &invalid)?;
            }
        }
    }
    Ok(())
}

fn validate_rtmp_vod_origin(
    origin: &str,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    if origin.len() > MAX_RTMP_VOD_ORIGIN_BYTES {
        return Err(invalid(
            "vod.sources[].origin",
            "must not exceed 2048 bytes",
        ));
    }
    let uri = origin.parse::<Uri>().map_err(|_| {
        invalid(
            "vod.sources[].origin",
            "must be an absolute HTTP or HTTPS origin",
        )
    })?;
    let has_query = uri
        .path_and_query()
        .is_some_and(|path_and_query| path_and_query.query().is_some());
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none()
        || has_query
        || origin.contains('#')
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
        || uri.path().contains("..")
        || uri.path().contains('%')
    {
        return Err(invalid(
            "vod.sources[].origin",
            "must be an HTTP or HTTPS origin without credentials, query, fragment, traversal, or encoded bytes",
        ));
    }
    let authority = uri.authority().expect("absolute URI authority was checked");
    let host = authority.host();
    if host.is_empty()
        || (!is_valid_dns_name(host) && host.parse::<IpAddr>().is_err())
        || authority.port_u16().is_some_and(|port| port == 0)
    {
        return Err(invalid(
            "vod.sources[].origin",
            "must contain a valid IP address or DNS host and nonzero port",
        ));
    }
    Ok(())
}

fn valid_rtmp_vod_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\', '?', '#', '%'])
        && !value.chars().any(char::is_control)
}

fn validate_rtmp_access_policy(
    service: &str,
    application: &crate::model::RtmpApplication,
    operation: &'static str,
    policy: &RtmpAccessPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.name.clone(),
        field,
        detail,
    };
    if policy.rules.len() > MAX_RTMP_ACCESS_RULES_PER_OPERATION {
        return Err(invalid(
            match operation {
                "publish" => "publish.rules",
                "play" => "play.rules",
                _ => unreachable!("RTMP access operation is closed"),
            },
            "must contain at most 64 rules",
        ));
    }
    let mut seen = HashSet::with_capacity(policy.rules.len());
    for rule in &policy.rules {
        if !valid_rtmp_network(&rule.network) {
            return Err(invalid(
                match operation {
                    "publish" => "publish.rules[].network",
                    "play" => "play.rules[].network",
                    _ => unreachable!("RTMP access operation is closed"),
                },
                "must be `all`, an IP address, or an IP address with a valid CIDR prefix",
            ));
        }
        if !seen.insert((rule.action, rule.network.as_str())) {
            return Err(ConfigError::DuplicateRtmpAccessRule {
                service: service.into(),
                application: application.name.clone(),
                operation,
                network: rule.network.clone(),
            });
        }
    }
    if let Some(token) = &policy.token {
        if token.source != RtmpTokenSource::StreamQuery {
            return Err(invalid(
                match operation {
                    "publish" => "publish.token.source",
                    "play" => "play.token.source",
                    _ => unreachable!("RTMP access operation is closed"),
                },
                "only `stream_query` is supported",
            ));
        }
        if token.parameter.is_empty()
            || token.parameter.len() > MAX_RTMP_TOKEN_PARAMETER_BYTES
            || !token
                .parameter
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(invalid(
                match operation {
                    "publish" => "publish.token.parameter",
                    "play" => "play.token.parameter",
                    _ => unreachable!("RTMP access operation is closed"),
                },
                "must be 1..=32 ASCII query-key bytes",
            ));
        }
        if token.secret.is_empty()
            || token.secret.len() > MAX_RTMP_TOKEN_BYTES
            || !token
                .secret
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'=' | b'#' | b'?'))
        {
            return Err(invalid(
                match operation {
                    "publish" => "publish.token.secret",
                    "play" => "play.token.secret",
                    _ => unreachable!("RTMP access operation is closed"),
                },
                "must be 1..=128 query-safe visible ASCII bytes",
            ));
        }
    }
    Ok(())
}

fn validate_rtmp_session_ceilings(
    service: &str,
    application: &crate::model::RtmpApplication,
    limits: &RtmpSessionCeilings,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.name.clone(),
        field,
        detail,
    };
    for (field, value, maximum) in [
        (
            "limits.max_connections",
            limits.max_connections,
            MAX_RTMP_APPLICATION_CONNECTIONS,
        ),
        (
            "limits.max_publishers",
            limits.max_publishers,
            MAX_RTMP_APPLICATION_PUBLISHERS,
        ),
        (
            "limits.max_viewers",
            limits.max_viewers,
            MAX_RTMP_APPLICATION_VIEWERS,
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(invalid(
                field,
                match field {
                    "limits.max_connections" => "must be between 1 and 100000",
                    "limits.max_publishers" => "must be between 1 and 10000",
                    "limits.max_viewers" => "must be between 1 and 1000000",
                    _ => unreachable!("RTMP session limit field is closed"),
                },
            ));
        }
    }
    Ok(())
}

fn valid_rtmp_network(value: &str) -> bool {
    if value == "all" {
        return true;
    }
    let Some((address, prefix)) = value.split_once('/') else {
        return value.parse::<IpAddr>().is_ok();
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    prefix <= if address.is_ipv4() { 32 } else { 128 }
}

fn validate_rtmp_recorder(
    service: &str,
    application: &str,
    recorder: &mut RtmpRecorder,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpRecorderPolicy {
        service: service.into(),
        application: application.into(),
        recorder: recorder.name.clone(),
        field,
        detail,
    };
    normalize_recording_root(&mut recorder.root_directory)
        .map_err(|detail| invalid("root_directory", detail))?;
    validate_recording_suffix_template(&recorder.suffix_template)
        .map_err(|detail| invalid("suffix_template", detail))?;
    if !recorder.record_mask.audio && !recorder.record_mask.video {
        return Err(invalid("record_mask", "must enable audio or video"));
    }
    if recorder.record_mask.keyframes && !recorder.record_mask.video {
        return Err(invalid(
            "record_mask.keyframes",
            "requires record_mask.video = true",
        ));
    }
    if let crate::model::RtmpRecorderTimezone::Iana(name) = &recorder.timezone {
        let parsed = name.parse::<chrono_tz::Tz>();
        if name.len() > 64 || parsed.is_err() || name.eq_ignore_ascii_case("utc") {
            return Err(invalid(
                "timezone",
                "must be `utc` or an exact IANA timezone name of at most 64 bytes",
            ));
        }
    }
    validate_rtmp_recorder_limit(
        recorder.max_queue_messages,
        MAX_RECORDER_QUEUE_MESSAGES,
        "max_queue_messages",
        "must be between 1 and 65536",
        &invalid,
    )?;
    validate_rtmp_recorder_limit(
        recorder.max_queue_bytes,
        MAX_RECORDER_QUEUE_BYTES,
        "max_queue_bytes",
        "must be between 1 and 1073741824",
        &invalid,
    )?;
    validate_rtmp_recorder_limit(
        recorder.shutdown_timeout_ms,
        MAX_RECORDER_SHUTDOWN_TIMEOUT_MS,
        "shutdown_timeout_ms",
        "must be between 1 and 60000",
        &invalid,
    )?;
    if let Some(max_storage_bytes) = recorder.max_storage_bytes {
        validate_rtmp_recorder_limit(
            max_storage_bytes,
            MAX_RECORDER_STORAGE_BYTES,
            "max_storage_bytes",
            "must be null or between 1 and 1099511627776",
            &invalid,
        )?;
    }
    if let Some(max_storage_files) = recorder.max_storage_files {
        validate_rtmp_recorder_limit(
            max_storage_files,
            MAX_RECORDER_STORAGE_FILES,
            "max_storage_files",
            "must be null or between 1 and 1000000",
            &invalid,
        )?;
        if let Some(max_size) = recorder.max_size {
            validate_rtmp_recorder_limit(
                max_size,
                MAX_RECORDER_FILE_BYTES,
                "max_size",
                "must be null or between 1 and 1099511627776",
                &invalid,
            )?;
        }
        if let Some(max_frames) = recorder.max_frames {
            validate_rtmp_recorder_limit(
                max_frames,
                MAX_RECORDER_FRAME_COUNT,
                "max_frames",
                "must be null or between 1 and 1000000000",
                &invalid,
            )?;
        }
    }
    validate_rtmp_recorder_limit(
        recorder.max_active_recorders,
        MAX_RECORDER_ACTIVE_RECORDERS,
        "max_active_recorders",
        "must be between 1 and 256",
        &invalid,
    )?;
    if recorder
        .rotation_interval_ms
        .is_some_and(|interval| interval == 0 || interval > MAX_RECORDER_ROTATION_INTERVAL_MS)
    {
        return Err(invalid(
            "rotation_interval_ms",
            "must be null or between 1 and 2147483647",
        ));
    }
    if recorder
        .max_storage_bytes
        .is_some_and(|maximum| recorder.max_queue_bytes > maximum)
    {
        return Err(ConfigError::RtmpRecorderQueueExceedsStorage {
            service: service.into(),
            application: application.into(),
            recorder: recorder.name.clone(),
        });
    }
    Ok(())
}

fn validate_rtmp_recorder_limit(
    value: u64,
    maximum: u64,
    field: &'static str,
    detail: &'static str,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return Err(invalid(field, detail));
    }
    Ok(())
}

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
    if pool.http_versions.max == HttpVersion::Http2 && pool.tls.is_none() {
        return Err(ConfigError::H2RequiresUpstreamTls {
            pool: pool.name.clone(),
        });
    }
    validate_upstream_pool_timeouts(pool)
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
    if pool.servers.is_empty() {
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
    if let UpstreamEndpoint::Socket { address } = endpoint {
        if management_bind
            .is_some_and(|management| endpoint_exposes_management(address, management))
        {
            return Err(ConfigError::ManagementUpstreamEndpoint {
                pool: pool.into(),
                endpoint: address,
            });
        }
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

/// Validates a pool health policy independently of Lua decoding.
///
/// # Errors
///
/// Returns an error when timing, thresholds, or probe-specific fields are invalid.
pub fn validate_health_check_config(
    pool: &str,
    health_check: &HealthCheck,
) -> Result<(), ConfigError> {
    let invalid = |detail| ConfigError::InvalidHealthCheck {
        pool: pool.into(),
        detail,
    };
    if !(MIN_HEALTH_INTERVAL_MS..=MAX_HEALTH_INTERVAL_MS).contains(&health_check.interval_ms) {
        return Err(invalid("interval_ms must be between 1000 and 86400000"));
    }
    if health_check.timeout_ms == 0
        || health_check.timeout_ms > MAX_HEALTH_TIMEOUT_MS
        || health_check.timeout_ms > health_check.interval_ms
    {
        return Err(invalid(
            "timeout_ms must be between 1 and 30000 and no greater than interval_ms",
        ));
    }
    for (field, interval) in [
        ("fast_interval_ms", health_check.fast_interval_ms),
        ("down_interval_ms", health_check.down_interval_ms),
    ] {
        if interval.is_some_and(|value| {
            !(MIN_HEALTH_INTERVAL_MS..=MAX_HEALTH_INTERVAL_MS).contains(&value)
        }) {
            return Err(invalid(match field {
                "fast_interval_ms" => "fast_interval_ms must be between 1000 and 86400000",
                _ => "down_interval_ms must be between 1000 and 86400000",
            }));
        }
    }
    if health_check.healthy_threshold == 0
        || health_check.unhealthy_threshold == 0
        || health_check.healthy_threshold > MAX_HEALTH_THRESHOLD
        || health_check.unhealthy_threshold > MAX_HEALTH_THRESHOLD
    {
        return Err(invalid("thresholds must be between 1 and 100"));
    }

    match health_check.kind {
        HealthCheckType::Tcp
            if health_check.host.is_some()
                || health_check.path.is_some()
                || health_check.expected_status.is_some()
                || health_check.http_version.is_some() =>
        {
            Err(invalid("TCP checks do not accept HTTP fields"))
        }
        HealthCheckType::Tcp => Ok(()),
        HealthCheckType::Http => {
            if let Some(host) = health_check.host.as_deref() {
                if host.len() > MAX_HEALTH_HOST_BYTES {
                    return Err(invalid("HTTP check host exceeds 255 bytes"));
                }
                let authority = host
                    .parse::<http::uri::Authority>()
                    .map_err(|_| invalid("HTTP check host must be a valid authority"))?;
                if authority.as_str().contains('@') {
                    return Err(invalid("HTTP check host must not contain userinfo"));
                }
                if authority.host().is_empty() || authority_has_invalid_port(&authority) {
                    return Err(invalid(
                        "HTTP check host must contain a valid host and numeric port",
                    ));
                }
            }
            let path = health_check
                .path
                .as_deref()
                .ok_or_else(|| invalid("HTTP checks require path"))?;
            if path.len() > MAX_HEALTH_PATH_BYTES {
                return Err(invalid("HTTP check path exceeds 2048 bytes"));
            }
            let valid_path = path.starts_with('/')
                && path
                    .parse::<PathAndQuery>()
                    .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == path)
                && is_unambiguous_http_path(path);
            if !valid_path {
                return Err(invalid(
                    "HTTP check path must be an unambiguous absolute path",
                ));
            }
            if health_check
                .expected_status
                .is_some_and(|status| !(200..=599).contains(&status))
            {
                return Err(invalid("expected_status must be between 200 and 599"));
            }
            Ok(())
        }
    }
}

fn validate_l4_services(
    l4_services: &[L4Service],
    upstream_pool_names: &HashSet<String>,
    tls_upstream_pool_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    for service in l4_services {
        if !upstream_pool_names.contains(&service.upstream_pool) {
            return Err(ConfigError::UnknownL4UpstreamPool {
                service: service.name.clone(),
                pool: service.upstream_pool.clone(),
            });
        }
        if tls_upstream_pool_names.contains(&service.upstream_pool) {
            return Err(ConfigError::TlsUpstreamPoolForL4Service {
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
        validate_safe_integer(
            "L4 service",
            &service.name,
            "connect_timeout_ms",
            service.connect_timeout_ms,
        )?;
        validate_safe_integer(
            "L4 service",
            &service.name,
            "idle_timeout_ms",
            service.idle_timeout_ms,
        )?;
        if let Some(lifetime_timeout_ms) = service.lifetime_timeout_ms {
            validate_safe_integer(
                "L4 service",
                &service.name,
                "lifetime_timeout_ms",
                lifetime_timeout_ms,
            )?;
        }
        if let Some(policy) = &service.udp {
            validate_udp_policy(&service.name, policy)?;
        }
    }

    Ok(())
}

fn validate_udp_policy(service: &str, policy: &UdpPolicy) -> Result<(), ConfigError> {
    for (field, value, maximum) in [
        (
            "udp.max_datagram_bytes",
            policy.max_datagram_bytes,
            MAX_UDP_DATAGRAM_BYTES,
        ),
        ("udp.max_sessions", policy.max_sessions, MAX_UDP_SESSIONS),
        (
            "udp.max_session_bytes",
            policy.max_session_bytes,
            MAX_UDP_SESSION_BYTES,
        ),
        (
            "udp.max_queue_datagrams",
            policy.max_queue_datagrams,
            MAX_UDP_QUEUE_DATAGRAMS,
        ),
        (
            "udp.max_queue_bytes",
            policy.max_queue_bytes,
            MAX_UDP_QUEUE_BYTES,
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(ConfigError::InvalidL4UdpPolicy {
                service: service.into(),
                field,
                detail: "must be positive and within the bounded UDP limit",
            });
        }
        validate_safe_integer("L4 service", service, field, value)?;
    }
    if policy.max_queue_bytes < policy.max_datagram_bytes {
        return Err(ConfigError::InvalidL4UdpPolicy {
            service: service.into(),
            field: "udp.max_queue_bytes",
            detail: "must be at least max_datagram_bytes",
        });
    }
    if policy.max_session_bytes < policy.max_datagram_bytes {
        return Err(ConfigError::InvalidL4UdpPolicy {
            service: service.into(),
            field: "udp.max_session_bytes",
            detail: "must be at least max_datagram_bytes",
        });
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

fn validate_optional_safe_limit(
    kind: &'static str,
    name: &str,
    field: &'static str,
    value: Option<u64>,
) -> Result<(), ConfigError> {
    if value == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind,
            name: name.into(),
            field,
        });
    }
    if let Some(value) = value {
        validate_safe_integer(kind, name, field, value)?;
    }
    Ok(())
}

fn validate_access_log(
    kind: &'static str,
    name: &str,
    policy: Option<&AccessLogPolicy>,
) -> Result<(), ConfigError> {
    if let Some(AccessLogPolicy::File { path }) = policy {
        validate_file_path(kind, name, "access_log.path", path)?;
    }
    Ok(())
}

fn validate_safe_integer(
    kind: &'static str,
    name: &str,
    field: &'static str,
    value: u64,
) -> Result<(), ConfigError> {
    if value > MAX_SAFE_JSON_INTEGER {
        return Err(ConfigError::LimitTooLarge {
            kind,
            name: name.into(),
            field,
        });
    }
    Ok(())
}
