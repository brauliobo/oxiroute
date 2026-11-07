#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_rtmp_callbacks(
    service: &str,
    application: Option<&str>,
    callbacks: &RtmpCallbackConfig,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| match application {
        Some(application) => ConfigError::InvalidRtmpApplicationPolicy {
            service: service.into(),
            application: application.into(),
            field,
            detail,
        },
        None => ConfigError::InvalidRtmpServicePolicy {
            service: service.into(),
            field,
            detail,
        },
    };
    for (field, value) in [
        ("callbacks.on_connect", callbacks.on_connect.as_deref()),
        (
            "callbacks.on_disconnect",
            callbacks.on_disconnect.as_deref(),
        ),
        ("callbacks.on_publish", callbacks.on_publish.as_deref()),
        (
            "callbacks.on_publish_done",
            callbacks.on_publish_done.as_deref(),
        ),
        ("callbacks.on_play", callbacks.on_play.as_deref()),
        ("callbacks.on_play_done", callbacks.on_play_done.as_deref()),
        ("callbacks.on_done", callbacks.on_done.as_deref()),
        ("callbacks.on_update", callbacks.on_update.as_deref()),
    ] {
        if value.is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_RTMP_CALLBACK_URL_BYTES
                || (!value.starts_with("http://") && !value.starts_with("https://"))
                || value.bytes().any(|byte| byte.is_ascii_control())
                || value.contains(' ')
                || value.contains('#')
        }) {
            return Err(invalid(field, "must be a bounded HTTP or HTTPS URL"));
        }
    }
    if callbacks.timeout_ms == 0 || callbacks.timeout_ms > MAX_RTMP_RELAY_TIMEOUT_MS {
        return Err(invalid(
            "callbacks.timeout_ms",
            "must be between 1 and 30000",
        ));
    }
    if callbacks.notify_update_timeout_ms > MAX_RTMP_RELAY_TIMEOUT_MS {
        return Err(invalid(
            "callbacks.notify_update_timeout_ms",
            "must be between 0 and 30000",
        ));
    }
    Ok(())
}

pub(super) fn validate_rtmp_vod(
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
            || !valid_rtmp_literal_component(name)
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
