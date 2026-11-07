#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn is_supported_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"allow"
            | b"deny"
            | b"max_connections"
            | b"live"
            | b"idle_streams"
            | b"exec"
            | b"exec_push"
            | b"exec_publish"
            | b"exec_publish_done"
            | b"respawn"
            | b"respawn_timeout"
            | b"record"
            | b"record_path"
            | b"record_suffix"
            | b"record_unique"
            | b"record_interval"
            | b"record_append"
            | b"record_lock"
            | b"record_max_size"
            | b"record_max_frames"
            | b"record_notify"
            | b"rtmp_auto_push"
            | b"rtmp_auto_push_reconnect"
            | b"rtmp_socket_dir"
            | b"hls"
            | b"hls_fragment"
            | b"hls_max_fragment"
            | b"hls_path"
            | b"hls_playlist_length"
            | b"hls_nested"
            | b"hls_fragment_naming"
            | b"hls_cleanup"
            | b"hls_keys"
            | b"hls_key_url"
            | b"hls_fragments_per_key"
    )
}

pub(super) fn is_root_auto_push_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"rtmp_auto_push" | b"rtmp_auto_push_reconnect" | b"rtmp_socket_dir"
    )
}

pub(super) fn unsupported_rtmp_reason(name: &[u8]) -> &'static str {
    match name {
        b"allow" | b"deny" => {
            "nginx-RTMP publish/play access rules have no canonical RTMP authorization policy"
        }
        b"push"
        | b"pull"
        | b"relay_buffer"
        | b"push_reconnect"
        | b"pull_reconnect"
        | b"session_relay"
        | b"rtmp_auto_push"
        | b"rtmp_auto_push_reconnect"
        | b"rtmp_socket_dir" => {
            "nginx-RTMP relay and auto-push topology has no canonical RTMP relay model"
        }
        b"exec" | b"exec_push" | b"exec_pull" | b"exec_publish" | b"exec_publish_done"
        | b"exec_play" | b"exec_play_done" | b"exec_record_done" | b"exec_static" | b"respawn"
        | b"respawn_timeout" | b"exec_kill_signal" | b"exec_options" => {
            "nginx-RTMP process hooks have no canonical command-execution model"
        }
        b"play" | b"play_temp_path" | b"play_local_path" | b"netcall_timeout"
        | b"netcall_buffer" => {
            "nginx-RTMP VOD and netcall sources have no canonical playback-source model"
        }
        b"on_connect"
        | b"on_disconnect"
        | b"on_publish"
        | b"on_play"
        | b"on_publish_done"
        | b"on_play_done"
        | b"on_done"
        | b"on_record_done"
        | b"on_update"
        | b"notify_method"
        | b"notify_update_timeout"
        | b"notify_update_strict"
        | b"notify_relay_redirect" => {
            "nginx-RTMP HTTP callbacks have no canonical notification policy"
        }
        b"access_log" | b"log_format" => {
            "only one absolute rtmp-scope access_log with the combined format has canonical semantics"
        }
        b"max_connections" => {
            "nginx-RTMP max_connections is one process-wide RTMP CONNECT cap, not a per-listener connection cap"
        }
        name if name.starts_with(b"hls_") || name == b"hls" => {
            "nginx-RTMP HLS packaging has no canonical media-packaging service"
        }
        name if name.starts_with(b"dash_") || name == b"dash" => {
            "nginx-RTMP DASH packaging has no canonical media-packaging service"
        }
        b"so_keepalive" | b"timeout" | b"ping" | b"ping_timeout" | b"max_streams"
        | b"ack_window" | b"chunk_size" | b"max_message" | b"out_queue" | b"out_cork" | b"busy"
        | b"play_time_fix" | b"publish_time_fix" | b"buflen" => {
            "nginx-RTMP transport and session tuning has no canonical RTMP session policy"
        }
        b"meta"
        | b"stream_buckets"
        | b"buffer"
        | b"sync"
        | b"interleave"
        | b"wait_key"
        | b"wait_video"
        | b"publish_notify"
        | b"play_restart"
        | b"drop_idle_publisher" => {
            "nginx-RTMP stream timing and codec behavior has no canonical application policy"
        }
        _ => "registered nginx-RTMP behavior has no canonical or runtime abstraction",
    }
}

pub(super) fn valid_exec_path(value: &str) -> bool {
    value.starts_with('/')
        && !value.is_empty()
        && !value.ends_with('/')
        && value.len() <= 4_096
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && value.strip_prefix('/').is_some_and(|value| {
            value
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        })
}

pub(super) fn parse_access_rule(
    directive: &ExpandedDirective,
    name: &[u8],
    origin: DirectiveOrigin,
) -> Option<EffectiveRtmpAccessRule> {
    let (operation, target) = match directive.directive.arguments.as_slice() {
        [target] => (AccessOperation::Both, target),
        [operation, target] => (
            match operation.value.as_slice() {
                b"publish" => AccessOperation::Publish,
                b"play" => AccessOperation::Play,
                _ => return None,
            },
            target,
        ),
        _ => return None,
    };
    let network = std::str::from_utf8(&target.value).ok()?.to_owned();
    Some(EffectiveRtmpAccessRule {
        action: match name {
            b"allow" => RtmpAclAction::Allow,
            b"deny" => RtmpAclAction::Deny,
            _ => return None,
        },
        network,
        origin,
        operation,
    })
}

pub(super) fn parse_record(
    arguments: &[Word],
) -> Result<(RecordSetting, RtmpRecordMask), &'static str> {
    let values = arguments
        .iter()
        .map(|argument| argument.value.as_slice())
        .collect::<Vec<_>>();
    let mut audio = false;
    let mut video = false;
    let mut keyframes = false;
    let mut manual = false;
    let mut off = false;
    for value in values {
        match value {
            b"off" => off = true,
            b"all" => {
                audio = true;
                video = true;
            }
            b"audio" => audio = true,
            b"video" => video = true,
            b"keyframes" => {
                video = true;
                keyframes = true;
            }
            b"manual" => manual = true,
            _ => return Err("nginx-RTMP record bitmask is not exactly representable"),
        }
    }
    if off {
        return Ok((RecordSetting::Off, RtmpRecordMask::default()));
    }
    if !audio && !video {
        return Err(
            "bare record manual has no nginx audio/video bits and is not canonical recording",
        );
    }
    Ok((
        if manual {
            RecordSetting::Manual
        } else {
            RecordSetting::Continuous
        },
        RtmpRecordMask {
            audio,
            video,
            keyframes,
        },
    ))
}

pub(super) fn parse_u32(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub(super) fn parse_u64(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub(super) fn valid_canonical_name(value: &[u8]) -> bool {
    std::str::from_utf8(value).is_ok_and(|name| {
        !name.trim().is_empty() && name.trim() == name && !name.chars().any(char::is_control)
    })
}

pub(super) fn secure_recording_root(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_RECORDING_ROOT_BYTES
        || !value.starts_with('/')
        || value == "/"
        || value.ends_with('/')
        || value.as_bytes().contains(&0)
        || value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(PathBuf::from(value))
}

pub(super) fn secure_auto_push_root(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_RECORDING_ROOT_BYTES
        || !value.starts_with('/')
        || value == "/"
        || value.ends_with('/')
        || value.as_bytes().contains(&0)
        || value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(PathBuf::from(value))
}

pub(super) fn exact_suffix(value: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_SUFFIX_BYTES
        || value.as_bytes().contains(&0)
        || value
            .as_bytes()
            .iter()
            .any(|byte| matches!(*byte, b'/' | b'\\'))
    {
        return None;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if !matches!(
            bytes.get(index + 1),
            Some(b'Y' | b'm' | b'd' | b'H' | b'M' | b'S' | b'%')
        ) {
            return None;
        }
        index += 2;
    }
    Some(value.to_owned())
}

pub(super) fn parse_push_target(value: &[u8]) -> Option<(String, u16, String)> {
    let value = std::str::from_utf8(value).ok()?.strip_prefix("rtmp://")?;
    let (authority, application) = value.split_once('/')?;
    if authority.is_empty()
        || application.is_empty()
        || application.contains(['/', '?', '#'])
        || application.contains('$') && application != "$name"
    {
        return None;
    }
    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() || host.contains(':') {
            return None;
        }
        (host, port.parse().ok()?)
    } else {
        (authority, 1_935)
    };
    if port == 0
        || host.contains(['@', '/', '?', '#'])
        || host.parse::<std::net::IpAddr>().is_err()
            && (!host.is_ascii() || host.split('.').any(str::is_empty))
    {
        return None;
    }
    Some((host.to_ascii_lowercase(), port, application.to_owned()))
}

pub(super) fn parse_nginx_milliseconds(value: &[u8]) -> Option<u64> {
    let mut index = 0;
    let mut previous_unit = 2_u8;
    let mut total = 0_u64;
    while index < value.len() {
        let start = index;
        while value.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if start == index {
            return None;
        }
        let number = std::str::from_utf8(&value[start..index])
            .ok()?
            .parse::<u64>()
            .ok()?;
        let (unit, multiplier) = if index == value.len() {
            (9, 1)
        } else if value[index..].starts_with(b"ms") {
            index += 2;
            (8, 1)
        } else {
            let result = match value[index] {
                b'y' => (1, 365_u64 * 24 * 60 * 60 * 1_000),
                b'M' => (2, 30_u64 * 24 * 60 * 60 * 1_000),
                b'w' => (3, 7_u64 * 24 * 60 * 60 * 1_000),
                b'd' => (4, 24_u64 * 60 * 60 * 1_000),
                b'h' => (5, 60_u64 * 60 * 1_000),
                b'm' => (6, 60_u64 * 1_000),
                b's' => (7, 1_000_u64),
                _ => return None,
            };
            index += 1;
            result
        };
        if unit <= previous_unit {
            return None;
        }
        previous_unit = unit;
        total = total.checked_add(number.checked_mul(multiplier)?)?;
    }
    Some(total)
}

pub(super) fn parse_nginx_size(value: &[u8]) -> Option<u64> {
    let (digits, multiplier) = match value.last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return None,
    };
    std::str::from_utf8(digits)
        .ok()?
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
}

pub(super) fn parse_rtmp_socket(value: &[u8]) -> Option<SocketAddr> {
    if value.iter().all(u8::is_ascii_digit) {
        let port = parse_port(value)?;
        return Some(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    let text = std::str::from_utf8(value).ok()?;
    if let Some(port) = text
        .strip_prefix("*:")
        .and_then(|port| parse_port(port.as_bytes()))
    {
        return Some(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    text.parse::<SocketAddr>()
        .ok()
        .filter(|address| address.port() != 0)
}

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

pub(super) fn sockets_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    if first.port() != second.port() || first.is_ipv4() != second.is_ipv4() {
        return false;
    }
    first.ip() == second.ip() || first.ip().is_unspecified() || second.ip().is_unspecified()
}

pub(super) fn include_failure(edge: &super::super::IncludeEdge) -> Option<DiagnosticCode> {
    edge.failure.or_else(|| {
        edge.candidates
            .iter()
            .find_map(|candidate| match candidate.status {
                IncludeCandidateStatus::Expanded(_) => None,
                IncludeCandidateStatus::Cycle(_) => Some(E_INCLUDE_CYCLE),
                IncludeCandidateStatus::ExpansionLimit(_)
                | IncludeCandidateStatus::SourceSizeLimit
                | IncludeCandidateStatus::SourceFileLimit
                | IncludeCandidateStatus::AggregateSourceLimit => Some(E_SOURCE_LIMIT),
                IncludeCandidateStatus::CanonicalizeFailed
                | IncludeCandidateStatus::SourceChanged => Some(E_SOURCE_CHANGED),
                IncludeCandidateStatus::SourceIo => Some(E_SOURCE_IO),
            })
    })
}
