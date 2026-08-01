use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use http::uri::PathAndQuery;

use crate::{
    defaults::{
        MAX_CERTIFICATE_DNS_NAMES, MAX_CERTIFICATES, MAX_ENDPOINTS_PER_POOL, MAX_HEALTH_HOST_BYTES,
        MAX_HEALTH_INTERVAL_MS, MAX_HEALTH_PATH_BYTES, MAX_HEALTH_THRESHOLD, MAX_HEALTH_TIMEOUT_MS,
        MAX_HTTP_TIMEOUT_MS, MAX_RECORDER_ACTIVE_RECORDERS, MAX_RECORDER_QUEUE_BYTES,
        MAX_RECORDER_QUEUE_MESSAGES, MAX_RECORDER_ROTATION_INTERVAL_MS,
        MAX_RECORDER_SHUTDOWN_TIMEOUT_MS, MAX_RECORDER_STORAGE_BYTES, MAX_RECORDER_STORAGE_FILES,
        MAX_RTMP_APPLICATION_BYTES, MAX_RTMP_APPLICATIONS_PER_SERVICE, MAX_RTMP_FANOUT_QUEUE_BYTES,
        MAX_RTMP_FANOUT_QUEUE_MESSAGES, MAX_RTMP_OUTBOUND_CHUNK_SIZE, MAX_RTMP_PUSH_TARGETS,
        MAX_RTMP_RECORDERS_PER_APPLICATION, MAX_RTMP_RECORDING_ROOTS, MAX_RTMP_SERVICES,
        MAX_RTMP_SUBSCRIBERS, MAX_SAFE_JSON_INTEGER, MAX_TLS_PROFILES, MAX_TOTAL_ENDPOINTS,
        MAX_TOTAL_RTMP_RECORDERS, MIN_HEALTH_INTERVAL_MS,
    },
    lexical::{
        authority_has_invalid_port, canonical_ip, is_unambiguous_http_path,
        is_valid_certificate_dns_name, is_valid_dns_name, normalize_listener_binds,
        normalize_recording_root, normalize_upstream_endpoint, normalize_upstream_endpoints,
        normalize_upstream_server_names, validate_directory_path, validate_file_path,
        validate_recording_suffix_template,
    },
    model::{
        AccessLogPolicy, AlpnProtocol, Certificate, CertificateSource, Config, ConfigError,
        DnsResolutionPolicy, ForwardHttpVersion, ForwardProxyService, HealthCheck, HealthCheckType,
        HttpVersion, L4Service, Listener, ListenerBind, Management, Protocol, RtmpRecorder,
        RtmpService, Stats, TlsProfile, TlsVersion, UpstreamEndpoint, UpstreamPool,
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
    if stats.binds.is_empty() || stats.binds.len() > 8 {
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
            + stats.map_or(0, |stats| stats.binds.len()),
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
            (Protocol::Tcp, Some(service)) if !l4_service_names.contains(service) => {
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
    let h3 = listener.protocol == Protocol::ForwardHttp3;
    if datagram != h3 {
        return Err(ConfigError::InvalidListenerTransport {
            listener: listener.name.clone(),
            protocol: listener.protocol,
            detail: if h3 {
                "forward_http3 requires a UDP bind"
            } else {
                "UDP binds require forward_http3"
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
        Protocol::Http | Protocol::Rtmp | Protocol::Tcp => return Ok(()),
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
        || health_check.timeout_ms >= health_check.interval_ms
    {
        return Err(invalid(
            "timeout_ms must be between 1 and 30000 and less than interval_ms",
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
