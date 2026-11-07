#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_rtmp_access_policy(
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

pub(super) fn validate_rtmp_session_ceilings(
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
