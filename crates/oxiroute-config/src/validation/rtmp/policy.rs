#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_rtmp_outbound_policy(
    service: &str,
    policy: &mut RtmpOutboundPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpServicePolicy {
        service: service.into(),
        field,
        detail,
    };
    if policy.allow_domains.len() > MAX_RTMP_OUTBOUND_DOMAINS
        || policy.deny_domains.len() > MAX_RTMP_OUTBOUND_DOMAINS
    {
        return Err(invalid(
            "outbound_policy.allow_domains",
            "must contain at most 64 domains per list",
        ));
    }
    for (field, domains) in [
        ("outbound_policy.allow_domains", &mut policy.allow_domains),
        ("outbound_policy.deny_domains", &mut policy.deny_domains),
    ] {
        for domain in domains {
            domain.make_ascii_lowercase();
            if !is_valid_dns_name(domain) {
                return Err(invalid(field, "must contain canonical DNS names"));
            }
        }
    }
    if policy.allow_cidrs.len() > MAX_RTMP_OUTBOUND_CIDRS
        || policy.deny_cidrs.len() > MAX_RTMP_OUTBOUND_CIDRS
    {
        return Err(invalid(
            "outbound_policy.allow_cidrs",
            "must contain at most 64 CIDRs per list",
        ));
    }
    for (field, cidrs) in [
        ("outbound_policy.allow_cidrs", &mut policy.allow_cidrs),
        ("outbound_policy.deny_cidrs", &mut policy.deny_cidrs),
    ] {
        for cidr in cidrs {
            if !valid_rtmp_cidr(cidr) {
                return Err(invalid(
                    field,
                    "must contain IP addresses with valid prefixes",
                ));
            }
        }
    }
    if policy.max_chain_depth == 0 || policy.max_chain_depth > MAX_RTMP_CHAIN_DEPTH {
        return Err(invalid(
            "outbound_policy.max_chain_depth",
            "must be between 1 and 16",
        ));
    }
    Ok(())
}

pub(super) fn validate_rtmp_relay_policy(
    service: &str,
    application: &crate::model::RtmpApplication,
    policy: &RtmpRelayPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.name.clone(),
        field,
        detail,
    };
    for (field, value, detail) in [
        (
            "relay.max_queue_messages",
            policy.max_queue_messages,
            "must be between 1 and 65536",
        ),
        (
            "relay.max_queue_bytes",
            policy.max_queue_bytes,
            "must be between 1 and 1073741824",
        ),
        (
            "relay.buffer_ms",
            policy.buffer_ms,
            "must be between 1 and 60000",
        ),
        (
            "relay.push_reconnect_ms",
            policy.push_reconnect_ms,
            "must be between 1 and 300000",
        ),
        (
            "relay.pull_reconnect_ms",
            policy.pull_reconnect_ms,
            "must be between 1 and 300000",
        ),
        (
            "relay.dns_refresh_ms",
            policy.dns_refresh_ms,
            "must be between 1000 and 300000",
        ),
        (
            "relay.connect_timeout_ms",
            policy.connect_timeout_ms,
            "must be between 1 and 30000",
        ),
        (
            "relay.handshake_timeout_ms",
            policy.handshake_timeout_ms,
            "must be between 1 and 30000",
        ),
    ] {
        let maximum = match field {
            "relay.max_queue_messages" => 65_536,
            "relay.max_queue_bytes" => 1_073_741_824,
            "relay.buffer_ms" => MAX_RTMP_RELAY_BUFFER_MS,
            "relay.push_reconnect_ms" | "relay.pull_reconnect_ms" => MAX_RTMP_RECONNECT_MS,
            "relay.dns_refresh_ms" => MAX_RTMP_DNS_REFRESH_MS,
            "relay.connect_timeout_ms" | "relay.handshake_timeout_ms" => MAX_RTMP_RELAY_TIMEOUT_MS,
            _ => unreachable!("RTMP relay policy field is closed"),
        };
        let minimum = if field == "relay.dns_refresh_ms" {
            MIN_RTMP_DNS_REFRESH_MS
        } else {
            1
        };
        if value < minimum || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    Ok(())
}

pub(super) fn validate_rtmp_auto_push_policy(
    service: &str,
    policy: &mut RtmpAutoPushPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpServicePolicy {
        service: service.into(),
        field,
        detail,
    };
    normalize_absolute_directory(&mut policy.socket_dir)
        .map_err(|detail| invalid("auto_push.socket_dir", detail))?;
    if policy.enabled
        && (policy.socket_dir == Path::new("/") || policy.socket_dir == Path::new("/tmp"))
    {
        return Err(invalid(
            "auto_push.socket_dir",
            "must use a dedicated owner-only directory, not a shared root",
        ));
    }
    if policy
        .socket_dir
        .to_str()
        .is_none_or(|path| path.len() > MAX_RTMP_AUTO_PUSH_SOCKET_DIR_BYTES)
    {
        return Err(invalid(
            "auto_push.socket_dir",
            "must leave room for the bounded worker socket name",
        ));
    }
    if let Some(secret_file) = &policy.secret_file {
        validate_file_path(
            "RTMP auto-push",
            service,
            "auto_push.secret_file",
            secret_file,
        )?;
    }
    for (field, value, maximum, detail) in [
        (
            "auto_push.reconnect_ms",
            policy.reconnect_ms,
            MAX_RTMP_RECONNECT_MS,
            "must be between 1 and 300000 milliseconds",
        ),
        (
            "auto_push.connect_timeout_ms",
            policy.connect_timeout_ms,
            MAX_RTMP_RELAY_TIMEOUT_MS,
            "must be between 1 and 30000 milliseconds",
        ),
        (
            "auto_push.handshake_timeout_ms",
            policy.handshake_timeout_ms,
            MAX_RTMP_RELAY_TIMEOUT_MS,
            "must be between 1 and 30000 milliseconds",
        ),
        (
            "auto_push.max_peers",
            policy.max_peers,
            MAX_RTMP_AUTO_PUSH_PEERS,
            "must be between 1 and 64",
        ),
        (
            "auto_push.max_queue_messages",
            policy.max_queue_messages,
            MAX_RTMP_AUTO_PUSH_QUEUE_MESSAGES,
            "must be between 1 and 4096",
        ),
        (
            "auto_push.max_queue_bytes",
            policy.max_queue_bytes,
            MAX_RTMP_AUTO_PUSH_QUEUE_BYTES,
            "must be between 1 and 67108864",
        ),
        (
            "auto_push.max_streams",
            policy.max_streams,
            MAX_RTMP_AUTO_PUSH_STREAMS,
            "must be between 1 and 4096",
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    Ok(())
}

pub(super) fn validate_rtmp_transport(
    policy: &RtmpOutboundPolicy,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
    transport: RtmpTransport,
    field: &'static str,
) -> Result<(), ConfigError> {
    let valid = !matches!(
        (policy.rtmps, transport),
        (RtmpRtmpsPolicy::Disabled, RtmpTransport::Rtmps)
            | (RtmpRtmpsPolicy::Required, RtmpTransport::Rtmp)
    );
    if !valid {
        return Err(invalid(
            field,
            "transport does not satisfy the service outbound RTMPS policy",
        ));
    }
    Ok(())
}

pub(super) fn validate_rtmp_relay_stream_name(
    stream_name: Option<&String>,
    allow_dynamic_name: bool,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
    field: &'static str,
) -> Result<(), ConfigError> {
    let Some(stream_name) = stream_name else {
        return Ok(());
    };
    let valid =
        (allow_dynamic_name && stream_name == "$name") || valid_rtmp_literal_component(stream_name);
    if valid {
        Ok(())
    } else {
        Err(invalid(
            field,
            "must be a literal stream component or the exact `$name` variable",
        ))
    }
}

pub(super) fn validate_rtmp_tc_url(
    tc_url: Option<&String>,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
    field: &'static str,
) -> Result<(), ConfigError> {
    let Some(tc_url) = tc_url else {
        return Ok(());
    };
    if tc_url.len() > 2_048
        || (!tc_url.starts_with("rtmp://") && !tc_url.starts_with("rtmps://"))
        || tc_url.contains([' ', '\n', '\r', '#'])
    {
        return Err(invalid(
            field,
            "must be a bounded RTMP or RTMPS URL without fragments",
        ));
    }
    Ok(())
}

pub(super) fn validate_rtmp_flash_version(
    flash_version: Option<&String>,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
    field: &'static str,
) -> Result<(), ConfigError> {
    if flash_version.is_some_and(|value| {
        value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
    }) {
        return Err(invalid(field, "must be 1..=128 non-control bytes"));
    }
    Ok(())
}

pub(super) fn validate_rtmp_credentials(
    credentials: Option<&RtmpCredentialReference>,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
    field: &'static str,
) -> Result<(), ConfigError> {
    let Some(credentials) = credentials else {
        return Ok(());
    };
    if credentials.username.is_empty()
        || credentials.username.len() > MAX_RTMP_CREDENTIAL_USERNAME_BYTES
        || credentials.username.chars().any(char::is_control)
    {
        return Err(invalid(field, "username must be 1..=128 non-control bytes"));
    }
    let path = credentials.secret_file.to_string_lossy();
    if path.len() > MAX_RTMP_SECRET_FILE_BYTES {
        return Err(invalid(field, "secret_file exceeds 4096 bytes"));
    }
    validate_file_path(
        "RTMP credential",
        &credentials.username,
        "secret_file",
        &credentials.secret_file,
    )
    .map_err(|_| invalid(field, "secret_file must be a secure absolute file path"))
}

pub(super) fn valid_rtmp_literal_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '?', '#', '$'])
        && !value.chars().any(char::is_control)
}

fn valid_rtmp_cidr(value: &str) -> bool {
    let Some((address, prefix)) = value.split_once('/') else {
        return false;
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    prefix <= if address.is_ipv4() { 32 } else { 128 }
}
