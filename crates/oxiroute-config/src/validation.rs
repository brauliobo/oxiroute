use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use http::{Uri, uri::PathAndQuery};

use crate::{
    defaults::{
        MAX_ACME_CONTACTS, MAX_ACME_DIRECTORY_URL_BYTES, MAX_ACME_DNS_SUFFIXES,
        MAX_ACME_DNS01_PROVIDER_BYTES, MAX_ACME_DNS01_TIMEOUT_SECONDS, MAX_ACME_RETAINED_REVISIONS,
        MAX_ACME_RETENTION_DAYS, MAX_CERTIFICATE_DNS_NAMES, MAX_CERTIFICATES,
        MAX_ENDPOINTS_PER_POOL, MAX_HEALTH_HOST_BYTES, MAX_HEALTH_INTERVAL_MS,
        MAX_HEALTH_PATH_BYTES, MAX_HEALTH_THRESHOLD, MAX_HEALTH_TIMEOUT_MS, MAX_HTTP_TIMEOUT_MS,
        MAX_HTTP3_REQUEST_BODY_BYTES, MAX_PASSIVE_BACKOFF_MS, MAX_PASSIVE_ERROR_LIMIT,
        MAX_PASSIVE_RECOVERY_THRESHOLD, MAX_PROXY_PROTOCOL_TIMEOUT_MS,
        MAX_RECORDER_ACTIVE_RECORDERS, MAX_RECORDER_FILE_BYTES, MAX_RECORDER_FRAME_COUNT,
        MAX_RECORDER_QUEUE_BYTES, MAX_RECORDER_QUEUE_MESSAGES, MAX_RECORDER_ROTATION_INTERVAL_MS,
        MAX_RECORDER_SHUTDOWN_TIMEOUT_MS, MAX_RECORDER_STORAGE_BYTES, MAX_RECORDER_STORAGE_FILES,
        MAX_RTMP_ACCESS_RULES_PER_OPERATION, MAX_RTMP_APPLICATION_BYTES,
        MAX_RTMP_APPLICATION_CONNECTIONS, MAX_RTMP_APPLICATION_NAME_BYTES,
        MAX_RTMP_APPLICATION_PUBLISHERS, MAX_RTMP_APPLICATION_VIEWERS,
        MAX_RTMP_APPLICATIONS_PER_SERVICE, MAX_RTMP_AUTO_PUSH_PEERS,
        MAX_RTMP_AUTO_PUSH_QUEUE_BYTES, MAX_RTMP_AUTO_PUSH_QUEUE_MESSAGES,
        MAX_RTMP_AUTO_PUSH_SOCKET_DIR_BYTES, MAX_RTMP_AUTO_PUSH_STREAMS,
        MAX_RTMP_CALLBACK_URL_BYTES, MAX_RTMP_CHAIN_DEPTH, MAX_RTMP_CREDENTIAL_USERNAME_BYTES,
        MAX_RTMP_DASH_ACTIVE_STREAMS, MAX_RTMP_DASH_OUTPUTS, MAX_RTMP_DASH_PLAYLIST_LENGTH_MS,
        MAX_RTMP_DASH_QUEUE_MESSAGES, MAX_RTMP_DASH_SEGMENT_BYTES,
        MAX_RTMP_DASH_SEGMENT_DURATION_MS, MAX_RTMP_DASH_STORAGE_BYTES,
        MAX_RTMP_DASH_STORAGE_FILES, MAX_RTMP_DNS_REFRESH_MS, MAX_RTMP_EXEC_ARGUMENT_BYTES,
        MAX_RTMP_EXEC_ARGUMENTS, MAX_RTMP_EXEC_ARGV_BYTES, MAX_RTMP_EXEC_ENV_BYTES,
        MAX_RTMP_EXEC_ENV_NAME_BYTES, MAX_RTMP_EXEC_ENV_VALUE_BYTES, MAX_RTMP_EXEC_ENVIRONMENT,
        MAX_RTMP_EXEC_NAME_BYTES, MAX_RTMP_EXEC_PROCESSES, MAX_RTMP_EXEC_PROFILES_PER_SERVICE,
        MAX_RTMP_EXEC_QUEUE_BYTES, MAX_RTMP_EXEC_QUEUE_MESSAGES, MAX_RTMP_EXEC_RESPAWN_DELAY_MS,
        MAX_RTMP_EXEC_RESPAWNS, MAX_RTMP_EXEC_SHUTDOWN_TIMEOUT_MS, MAX_RTMP_EXEC_STDERR_BYTES,
        MAX_RTMP_EXEC_STDOUT_BYTES, MAX_RTMP_EXEC_TIMEOUT_MS, MAX_RTMP_FANOUT_QUEUE_BYTES,
        MAX_RTMP_FANOUT_QUEUE_MESSAGES, MAX_RTMP_HLS_ACTIVE_STREAMS,
        MAX_RTMP_HLS_KEY_ROTATION_SEGMENTS, MAX_RTMP_HLS_KEY_URL_PREFIX_BYTES,
        MAX_RTMP_HLS_NAME_BYTES, MAX_RTMP_HLS_OUTPUTS, MAX_RTMP_HLS_PLAYLIST_LENGTH_MS,
        MAX_RTMP_HLS_QUEUE_MESSAGES, MAX_RTMP_HLS_SEGMENT_BYTES, MAX_RTMP_HLS_SEGMENT_DURATION_MS,
        MAX_RTMP_HLS_STORAGE_BYTES, MAX_RTMP_HLS_STORAGE_FILES, MAX_RTMP_HLS_VARIANTS,
        MAX_RTMP_INBOUND_MESSAGE_SIZE, MAX_RTMP_OUTBOUND_CHUNK_SIZE, MAX_RTMP_OUTBOUND_CIDRS,
        MAX_RTMP_OUTBOUND_DOMAINS, MAX_RTMP_PULL_TARGETS, MAX_RTMP_PUSH_TARGETS,
        MAX_RTMP_RECONNECT_MS, MAX_RTMP_RECORDERS_PER_APPLICATION, MAX_RTMP_RECORDING_ROOTS,
        MAX_RTMP_RELAY_BUFFER_MS, MAX_RTMP_RELAY_TIMEOUT_MS, MAX_RTMP_SECRET_FILE_BYTES,
        MAX_RTMP_SERVICES, MAX_RTMP_SUBSCRIBERS, MAX_RTMP_TOKEN_BYTES,
        MAX_RTMP_TOKEN_PARAMETER_BYTES, MAX_RTMP_VOD_DURATION_MS, MAX_RTMP_VOD_FILE_BYTES,
        MAX_RTMP_VOD_ORIGIN_BYTES, MAX_RTMP_VOD_SESSIONS, MAX_RTMP_VOD_SOURCE_NAME_BYTES,
        MAX_RTMP_VOD_SOURCES, MAX_SAFE_JSON_INTEGER, MAX_SELF_SIGNED_VALIDITY_DAYS,
        MAX_TLS_PROFILES, MAX_TOTAL_ENDPOINTS, MAX_TOTAL_RTMP_EXEC_PROFILES,
        MAX_TOTAL_RTMP_RECORDERS, MAX_UDP_DATAGRAM_BYTES, MAX_UDP_QUEUE_BYTES,
        MAX_UDP_QUEUE_DATAGRAMS, MAX_UDP_SESSION_BYTES, MAX_UDP_SESSIONS, MAX_UPSTREAM_WEIGHT,
        MIN_HEALTH_INTERVAL_MS, MIN_RTMP_DNS_REFRESH_MS, MIN_SELF_SIGNED_VALIDITY_DAYS,
    },
    lexical::{
        authority_has_invalid_port, canonical_ip, is_unambiguous_http_path,
        is_valid_certificate_dns_name, is_valid_dns_name, normalize_absolute_directory,
        normalize_listener_binds, normalize_recording_root, normalize_upstream_endpoint,
        normalize_upstream_endpoints, normalize_upstream_server_names, validate_directory_path,
        validate_file_path, validate_recording_suffix_template,
    },
    model::{
        AccessLogPolicy, AcmeDns01Config, AlpnProtocol, Certificate, CertificateSource,
        ConfigDraft, ConfigError, DnsResolutionPolicy, ForwardHttpVersion, ForwardProxyService,
        HealthCheck, HealthCheckType, HttpRequestHeaderMutation, HttpResponseHeaderMutation,
        HttpRouteAction, HttpService, HttpVersion, L4Service, Listener, ListenerBind, Management,
        Protocol, ProxyProtocolVersion, RtmpAccessPolicy, RtmpAutoPushPolicy, RtmpCallbackConfig,
        RtmpCredentialReference, RtmpExecFilesystemPolicy, RtmpExecMode, RtmpExecProfile,
        RtmpExecTrigger, RtmpOutboundPolicy, RtmpRecorder, RtmpRelayPolicy, RtmpRtmpsPolicy,
        RtmpService, RtmpSessionCeilings, RtmpTokenSource, RtmpTransport, RtmpVodSource, Stats,
        StatsPage, TlsProfile, TlsVersion, UdpPolicy, UpstreamAlgorithm, UpstreamEndpoint,
        UpstreamPool,
    },
};

mod management;
mod rtmp;
mod tls;
mod upstream;

use management::{
    endpoint_exposes_management, validate_bind_conflicts, validate_h3_upstream_usage,
    validate_listeners, validate_management, validate_stats,
};
use rtmp::validate_rtmp_services;
use tls::{validate_certificates, validate_tls_profiles};
pub use upstream::{
    validate_health_check_config, validate_upstream_pool_definitions, validate_upstream_pools,
};
use upstream::{validate_l4_services, validate_proxy_protocol_timeout};

const MAX_UDP_WIRE_DATAGRAM_BYTES: u64 = 65_507;
const MAX_UDP_PROXY_V2_ADDRESS_HEADER_BYTES: u64 = 52;

/// Validates and normalizes a complete configuration regardless of how it was constructed.
///
/// # Errors
///
#[allow(clippy::too_many_lines)]
pub(crate) fn validate_config_in_place(config: &mut ConfigDraft) -> Result<(), ConfigError> {
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
    validate_tls_profiles(&mut config.tls_profiles, &config.certificates)?;
    let cache_stores = crate::cache_validation::validate_cache_stores(&mut config.cache_stores)?;
    crate::forward_validation::validate_forward_proxy_services(
        &mut config.forward_proxy_services,
        &cache_stores,
    )?;
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
    let http_services = config
        .http_services
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect::<HashMap<_, _>>();
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
        &http_services,
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
    validate_h3_upstream_usage(
        &config.listeners,
        &config.http_services,
        &config.upstream_pools,
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
        &config.listeners,
    )?;

    Ok(())
}

fn validate_config_names(config: &ConfigDraft) -> Result<(), ConfigError> {
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
