#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn validate_rtmp_hls(
    service: &str,
    application: &str,
    hls: &mut crate::model::RtmpHlsPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.into(),
        field,
        detail,
    };
    normalize_absolute_directory(&mut hls.root_directory)
        .map_err(|detail| invalid("hls.root_directory", detail))?;
    if hls.segment_duration_ms == 0 || hls.segment_duration_ms > MAX_RTMP_HLS_SEGMENT_DURATION_MS {
        return Err(invalid(
            "hls.segment_duration_ms",
            "must be between 1 and 120000",
        ));
    }
    if hls.max_segment_duration_ms < hls.segment_duration_ms
        || hls.max_segment_duration_ms > MAX_RTMP_HLS_SEGMENT_DURATION_MS
    {
        return Err(invalid(
            "hls.max_segment_duration_ms",
            "must be at least segment_duration_ms and at most 120000",
        ));
    }
    if hls.playlist_length_ms < hls.segment_duration_ms
        || hls.playlist_length_ms > MAX_RTMP_HLS_PLAYLIST_LENGTH_MS
    {
        return Err(invalid(
            "hls.playlist_length_ms",
            "must be at least segment_duration_ms and at most 86400000",
        ));
    }
    for (field, value, maximum, detail) in [
        (
            "hls.max_segment_bytes",
            hls.max_segment_bytes,
            MAX_RTMP_HLS_SEGMENT_BYTES,
            "must be between 1 and 67108864",
        ),
        (
            "hls.max_queue_messages",
            hls.max_queue_messages,
            MAX_RTMP_HLS_QUEUE_MESSAGES,
            "must be between 1 and 65536",
        ),
        (
            "hls.max_storage_bytes",
            hls.max_storage_bytes,
            MAX_RTMP_HLS_STORAGE_BYTES,
            "must be between 1 and 1099511627776",
        ),
        (
            "hls.max_storage_files",
            hls.max_storage_files,
            MAX_RTMP_HLS_STORAGE_FILES,
            "must be between 1 and 1000000",
        ),
        (
            "hls.max_active_streams",
            hls.max_active_streams,
            MAX_RTMP_HLS_ACTIVE_STREAMS,
            "must be between 1 and 100000",
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    if hls.max_storage_bytes < hls.max_segment_bytes {
        return Err(invalid(
            "hls.max_storage_bytes",
            "must be at least max_segment_bytes",
        ));
    }
    if hls.variants.len() > MAX_RTMP_HLS_VARIANTS {
        return Err(invalid("hls.variants", "must contain at most 16 variants"));
    }
    let mut names = HashSet::with_capacity(hls.variants.len());
    for variant in &hls.variants {
        if variant.name.is_empty()
            || variant.name.len() > MAX_RTMP_HLS_NAME_BYTES
            || !valid_rtmp_literal_component(&variant.name)
            || !variant
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || !names.insert(variant.name.as_str())
        {
            return Err(invalid(
                "hls.variants[].name",
                "must be unique and one nonempty path component of at most 128 bytes",
            ));
        }
        if variant.bandwidth == 0 || variant.bandwidth > MAX_SAFE_JSON_INTEGER {
            return Err(invalid(
                "hls.variants[].bandwidth",
                "must be between 1 and the exact JSON integer limit",
            ));
        }
        if variant.width.is_some() != variant.height.is_some()
            || variant.width.is_some_and(|value| value == 0)
            || variant.height.is_some_and(|value| value == 0)
        {
            return Err(invalid(
                "hls.variants[]",
                "width and height must be provided together and nonzero",
            ));
        }
        if variant.codecs.as_deref().is_some_and(|codecs| {
            codecs.is_empty()
                || codecs.len() > 128
                || !codecs.is_ascii()
                || codecs
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\'))
        }) {
            return Err(invalid(
                "hls.variants[].codecs",
                "must be an ASCII codec string of at most 128 bytes without controls or quotes",
            ));
        }
    }
    if let Some(keys) = &hls.keys {
        if keys.rotation_segments == 0
            || keys.rotation_segments > MAX_RTMP_HLS_KEY_ROTATION_SEGMENTS
        {
            return Err(invalid(
                "hls.keys.rotation_segments",
                "must be between 1 and 100000",
            ));
        }
        if keys.url_prefix.len() > MAX_RTMP_HLS_KEY_URL_PREFIX_BYTES
            || !keys.url_prefix.is_ascii()
            || keys.url_prefix.bytes().any(|byte| {
                byte.is_ascii_control() || matches!(byte, b'?' | b'#' | b'\\' | b'"' | b'%' | b' ')
            })
            || (!keys.url_prefix.is_empty()
                && (!keys.url_prefix.ends_with('/')
                    || keys.url_prefix.starts_with('/')
                    || keys.url_prefix.strip_suffix('/').is_none_or(|prefix| {
                        prefix.split('/').any(|component| {
                            component.is_empty() || component == "." || component == ".."
                        })
                    })))
        {
            return Err(invalid(
                "hls.keys.url_prefix",
                "must be empty or an ASCII relative path prefix ending in `/`",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_rtmp_dash(
    service: &str,
    application: &str,
    dash: &mut crate::model::RtmpDashPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpApplicationPolicy {
        service: service.into(),
        application: application.into(),
        field,
        detail,
    };
    normalize_absolute_directory(&mut dash.root_directory)
        .map_err(|detail| invalid("dash.root_directory", detail))?;
    if dash.segment_duration_ms == 0 || dash.segment_duration_ms > MAX_RTMP_DASH_SEGMENT_DURATION_MS
    {
        return Err(invalid(
            "dash.segment_duration_ms",
            "must be between 1 and 120000",
        ));
    }
    if dash.max_segment_duration_ms < dash.segment_duration_ms
        || dash.max_segment_duration_ms > MAX_RTMP_DASH_SEGMENT_DURATION_MS
    {
        return Err(invalid(
            "dash.max_segment_duration_ms",
            "must be at least segment_duration_ms and at most 120000",
        ));
    }
    if dash.playlist_length_ms < dash.segment_duration_ms
        || dash.playlist_length_ms > MAX_RTMP_DASH_PLAYLIST_LENGTH_MS
    {
        return Err(invalid(
            "dash.playlist_length_ms",
            "must be at least segment_duration_ms and at most 86400000",
        ));
    }
    for (field, value, maximum, detail) in [
        (
            "dash.max_segment_bytes",
            dash.max_segment_bytes,
            MAX_RTMP_DASH_SEGMENT_BYTES,
            "must be between 1 and 67108864",
        ),
        (
            "dash.max_queue_messages",
            dash.max_queue_messages,
            MAX_RTMP_DASH_QUEUE_MESSAGES,
            "must be between 1 and 65536",
        ),
        (
            "dash.max_storage_bytes",
            dash.max_storage_bytes,
            MAX_RTMP_DASH_STORAGE_BYTES,
            "must be between 1 and 1099511627776",
        ),
        (
            "dash.max_storage_files",
            dash.max_storage_files,
            MAX_RTMP_DASH_STORAGE_FILES,
            "must be between 1 and 1000000",
        ),
        (
            "dash.max_active_streams",
            dash.max_active_streams,
            MAX_RTMP_DASH_ACTIVE_STREAMS,
            "must be between 1 and 100000",
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    if dash.max_storage_bytes < dash.max_segment_bytes {
        return Err(invalid(
            "dash.max_storage_bytes",
            "must be at least max_segment_bytes",
        ));
    }
    Ok(())
}
