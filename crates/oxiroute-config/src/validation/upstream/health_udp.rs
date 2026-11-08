#[allow(clippy::wildcard_imports)]
use super::*;

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

#[allow(clippy::too_many_lines)]
pub(in super::super) fn validate_l4_services(
    l4_services: &[L4Service],
    upstream_pool_names: &HashSet<String>,
    tls_upstream_pool_names: &HashSet<String>,
    listeners: &[Listener],
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
        let udp_listeners = listeners.iter().filter(|listener| {
            listener.service.as_deref() == Some(service.name.as_str())
                && listener.protocol == Protocol::Udp
        });
        let udp_listener_proxy_header_bytes = udp_listeners
            .filter_map(|listener| listener.proxy_protocol)
            .map(|policy| udp_proxy_protocol_header_bytes(policy.version))
            .max()
            .unwrap_or(0);
        let has_udp_listener = udp_listener_proxy_header_bytes > 0
            || listeners.iter().any(|listener| {
                listener.service.as_deref() == Some(service.name.as_str())
                    && listener.protocol == Protocol::Udp
            });
        if let Some(policy) = service.udp {
            validate_udp_policy(
                &service.name,
                policy,
                if has_udp_listener {
                    udp_listener_proxy_header_bytes.max(
                        service
                            .proxy_protocol
                            .map_or(0, |policy| udp_proxy_protocol_header_bytes(policy.version)),
                    )
                } else {
                    0
                },
            )?;
        } else if has_udp_listener {
            validate_udp_policy(
                &service.name,
                UdpPolicy::default(),
                udp_listener_proxy_header_bytes.max(
                    service
                        .proxy_protocol
                        .map_or(0, |policy| udp_proxy_protocol_header_bytes(policy.version)),
                ),
            )?;
        }
        if let Some(policy) = service.proxy_protocol {
            validate_proxy_protocol_timeout("L4 service", &service.name, policy.timeout_ms)?;
            if matches!(policy.version, ProxyProtocolVersion::Auto) {
                return Err(ConfigError::InvalidProxyProtocolPolicy {
                    kind: "L4 service",
                    name: service.name.clone(),
                    field: "proxy_protocol.version",
                    detail: "upstream PROXY protocol requires an explicit v1 or v2 version",
                });
            }
            if listeners.iter().any(|listener| {
                listener.service.as_deref() == Some(service.name.as_str())
                    && listener.protocol == Protocol::Udp
                    && matches!(policy.version, ProxyProtocolVersion::V1)
            }) {
                return Err(ConfigError::InvalidProxyProtocolPolicy {
                    kind: "L4 service",
                    name: service.name.clone(),
                    field: "proxy_protocol.version",
                    detail: "UDP listeners require an upstream v2 PROXY protocol policy",
                });
            }
        }
    }

    Ok(())
}

pub(in super::super) fn validate_proxy_protocol_timeout(
    kind: &'static str,
    name: &str,
    timeout_ms: u64,
) -> Result<(), ConfigError> {
    if timeout_ms == 0 || timeout_ms > MAX_PROXY_PROTOCOL_TIMEOUT_MS {
        return Err(ConfigError::InvalidProxyProtocolPolicy {
            kind,
            name: name.into(),
            field: "proxy_protocol.timeout_ms",
            detail: "must be between 1 and 86400000 milliseconds",
        });
    }
    validate_safe_integer(kind, name, "proxy_protocol.timeout_ms", timeout_ms)
}

pub(super) fn validate_udp_policy(
    service: &str,
    policy: UdpPolicy,
    proxy_header_bytes: u64,
) -> Result<(), ConfigError> {
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
    if proxy_header_bytes > 0
        && policy.max_datagram_bytes
            > MAX_UDP_WIRE_DATAGRAM_BYTES.saturating_sub(proxy_header_bytes)
    {
        return Err(ConfigError::InvalidL4UdpPolicy {
            service: service.into(),
            field: "udp.max_datagram_bytes",
            detail: "must leave room for the UDP PROXY v2 address header within the 65507-byte datagram limit",
        });
    }
    Ok(())
}

pub(super) fn udp_proxy_protocol_header_bytes(version: ProxyProtocolVersion) -> u64 {
    match version {
        ProxyProtocolVersion::V2 | ProxyProtocolVersion::Auto => {
            MAX_UDP_PROXY_V2_ADDRESS_HEADER_BYTES
        }
        ProxyProtocolVersion::V1 => 0,
    }
}
