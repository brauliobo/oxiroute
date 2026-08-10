use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
struct RtmpRecorderStorageLimits {
    bytes: Option<u64>,
    files: Option<u64>,
    active_recorders: u64,
}

#[allow(clippy::too_many_lines)]
pub(super) fn validate_rtmp_services(services: &mut [RtmpService]) -> Result<(), ConfigError> {
    if services.len() > MAX_RTMP_SERVICES {
        return Err(ConfigError::TooManyRtmpServices);
    }
    let mut total_recorders = 0_usize;
    let mut total_exec_profiles = 0_usize;
    let mut roots = HashMap::<PathBuf, (RtmpRecorderStorageLimits, String)>::new();
    let mut hls_outputs = 0_usize;
    let mut hls_roots = HashMap::<PathBuf, ((u64, u64, u64, u64), String)>::new();
    let mut dash_outputs = 0_usize;
    let mut dash_roots = HashMap::<PathBuf, ((u64, u64, u64, u64), String)>::new();
    let mut media_roots = HashMap::<PathBuf, ((u64, u64, u64, u64), String)>::new();
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
        if service.max_inbound_message_size == 0
            || service.max_inbound_message_size > MAX_RTMP_INBOUND_MESSAGE_SIZE
        {
            return Err(ConfigError::InvalidRtmpServicePolicy {
                service: service.name.clone(),
                field: "max_inbound_message_size",
                detail: "must be between 1 and 8388608",
            });
        }
        if service.ack_window_size == 0 {
            return Err(ConfigError::InvalidRtmpServicePolicy {
                service: service.name.clone(),
                field: "ack_window_size",
                detail: "must be nonzero",
            });
        }
        validate_access_log("RTMP service", &service.name, service.access_log.as_ref())?;
        validate_rtmp_outbound_policy(&service.name, &mut service.outbound_policy)?;
        validate_rtmp_callbacks(&service.name, None, &service.callbacks)?;
        validate_rtmp_auto_push_policy(&service.name, &mut service.auto_push)?;
        if service.exec_profiles.len() > MAX_RTMP_EXEC_PROFILES_PER_SERVICE {
            return Err(ConfigError::InvalidRtmpServicePolicy {
                service: service.name.clone(),
                field: "exec_profiles",
                detail: "must contain at most 64 profiles",
            });
        }
        validate_names(
            "RTMP exec profile",
            service
                .exec_profiles
                .iter()
                .map(|profile| profile.name.as_str()),
        )?;
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
        for profile in &mut service.exec_profiles {
            total_exec_profiles = total_exec_profiles.checked_add(1).ok_or(
                ConfigError::InvalidRtmpServicePolicy {
                    service: service.name.clone(),
                    field: "exec_profiles",
                    detail: "profile count overflow",
                },
            )?;
            if total_exec_profiles > MAX_TOTAL_RTMP_EXEC_PROFILES {
                return Err(ConfigError::InvalidRtmpServicePolicy {
                    service: service.name.clone(),
                    field: "exec_profiles",
                    detail: "configuration exceeds the 256-profile limit",
                });
            }
            validate_rtmp_exec_profile(&service.name, &service.applications, profile)?;
        }
        for application in &mut service.applications {
            if application.name.len() > MAX_RTMP_APPLICATION_NAME_BYTES {
                return Err(ConfigError::InvalidRtmpApplicationPolicy {
                    service: service.name.clone(),
                    application: application.name.clone(),
                    field: "name",
                    detail: "must be between 1 and 128 bytes",
                });
            }
            validate_rtmp_application(&service.name, &service.outbound_policy, application)?;
            if let Some(hls) = &mut application.hls {
                if !application.live {
                    return Err(ConfigError::InvalidRtmpApplicationPolicy {
                        service: service.name.clone(),
                        application: application.name.clone(),
                        field: "hls",
                        detail: "requires live = true",
                    });
                }
                hls_outputs = hls_outputs
                    .checked_add(1)
                    .ok_or(ConfigError::TooManyRtmpHlsOutputs)?;
                if hls_outputs > MAX_RTMP_HLS_OUTPUTS {
                    return Err(ConfigError::TooManyRtmpHlsOutputs);
                }
                validate_rtmp_hls(&service.name, &application.name, hls)?;
                let limits = (
                    hls.max_storage_bytes,
                    hls.max_storage_files,
                    hls.max_active_streams,
                    hls.max_segment_bytes,
                );
                let identity = format!("{}/{}/hls", service.name, application.name);
                if let Some((first_limits, first_output)) = hls_roots.get(&hls.root_directory) {
                    if *first_limits != limits {
                        return Err(ConfigError::RtmpHlsStorageLimitsMismatch {
                            root_directory: hls.root_directory.display().to_string(),
                            first_output: first_output.clone(),
                            second_output: identity,
                        });
                    }
                } else {
                    if hls_roots.len() >= MAX_RTMP_HLS_OUTPUTS {
                        return Err(ConfigError::TooManyRtmpHlsRoots);
                    }
                    hls_roots.insert(hls.root_directory.clone(), (limits, identity));
                }
                if let Some((first_limits, first_output)) = media_roots.get(&hls.root_directory) {
                    if *first_limits != limits {
                        return Err(ConfigError::RtmpHlsStorageLimitsMismatch {
                            root_directory: hls.root_directory.display().to_string(),
                            first_output: first_output.clone(),
                            second_output: format!("{}/{}", service.name, application.name),
                        });
                    }
                } else {
                    media_roots.insert(
                        hls.root_directory.clone(),
                        (limits, format!("{}/{}", service.name, application.name)),
                    );
                }
            }
            if let Some(dash) = &mut application.dash {
                if !application.live {
                    return Err(ConfigError::InvalidRtmpApplicationPolicy {
                        service: service.name.clone(),
                        application: application.name.clone(),
                        field: "dash",
                        detail: "requires live = true",
                    });
                }
                dash_outputs = dash_outputs
                    .checked_add(1)
                    .ok_or(ConfigError::TooManyRtmpDashOutputs)?;
                if dash_outputs > MAX_RTMP_DASH_OUTPUTS {
                    return Err(ConfigError::TooManyRtmpDashOutputs);
                }
                validate_rtmp_dash(&service.name, &application.name, dash)?;
                let limits = (
                    dash.max_storage_bytes,
                    dash.max_storage_files,
                    dash.max_active_streams,
                    dash.max_segment_bytes,
                );
                let identity = format!("{}/{}/dash", service.name, application.name);
                if let Some((first_limits, first_output)) = dash_roots.get(&dash.root_directory) {
                    if *first_limits != limits {
                        return Err(ConfigError::RtmpDashStorageLimitsMismatch {
                            root_directory: dash.root_directory.display().to_string(),
                            first_output: first_output.clone(),
                            second_output: identity.clone(),
                        });
                    }
                } else {
                    if dash_roots.len() >= MAX_RTMP_DASH_OUTPUTS {
                        return Err(ConfigError::TooManyRtmpDashRoots);
                    }
                    dash_roots.insert(dash.root_directory.clone(), (limits, identity.clone()));
                }
                if let Some((first_limits, first_output)) = media_roots.get(&dash.root_directory) {
                    if *first_limits != limits {
                        return Err(ConfigError::RtmpDashStorageLimitsMismatch {
                            root_directory: dash.root_directory.display().to_string(),
                            first_output: first_output.clone(),
                            second_output: identity,
                        });
                    }
                } else {
                    media_roots.insert(dash.root_directory.clone(), (limits, identity));
                }
            }
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
            if !application.live
                && let Some(recorder) = application.recorders.first()
            {
                return Err(ConfigError::RtmpRecorderRequiresLiveApplication {
                    service: service.name.clone(),
                    application: application.name.clone(),
                    recorder: recorder.name.clone(),
                });
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

#[allow(clippy::too_many_lines)]
fn validate_rtmp_application(
    service: &str,
    outbound_policy: &RtmpOutboundPolicy,
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
    if application.pull_targets.len() > MAX_RTMP_PULL_TARGETS {
        return Err(invalid("pull_targets", "must contain at most 16 targets"));
    }
    if !application.live && !application.pull_targets.is_empty() {
        return Err(invalid("pull_targets", "requires live = true"));
    }
    validate_rtmp_relay_policy(service, application, &application.relay)?;
    validate_rtmp_callbacks(service, Some(&application.name), &application.callbacks)?;
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
        validate_rtmp_transport(
            outbound_policy,
            &invalid,
            target.scheme,
            "push_targets[].scheme",
        )?;
        validate_rtmp_relay_stream_name(
            target.stream_name.as_ref(),
            true,
            &invalid,
            "push_targets[].stream_name",
        )?;
        validate_rtmp_tc_url(target.tc_url.as_ref(), &invalid, "push_targets[].tc_url")?;
        validate_rtmp_flash_version(
            target.flash_version.as_ref(),
            &invalid,
            "push_targets[].flash_version",
        )?;
        validate_rtmp_credentials(
            target.credentials.as_ref(),
            &invalid,
            "push_targets[].credentials",
        )?;
    }
    let mut pull_targets = HashSet::with_capacity(application.pull_targets.len());
    for target in &mut application.pull_targets {
        target.host.make_ascii_lowercase();
        if target.port == 0 {
            return Err(invalid("pull_targets[].port", "must be nonzero"));
        }
        if !is_valid_dns_name(&target.host) && target.host.parse::<std::net::IpAddr>().is_err() {
            return Err(invalid(
                "pull_targets[].host",
                "must be an IP address or canonical DNS name",
            ));
        }
        if !valid_rtmp_literal_component(&target.application) {
            return Err(invalid(
                "pull_targets[].application",
                "must be one nonempty path component without variables",
            ));
        }
        if !valid_rtmp_literal_component(&target.stream_name) {
            return Err(invalid(
                "pull_targets[].stream_name",
                "must be one nonempty path component without variables",
            ));
        }
        if !pull_targets.insert((
            &target.host,
            target.port,
            &target.application,
            &target.stream_name,
        )) {
            return Err(invalid("pull_targets", "must not contain duplicates"));
        }
        validate_rtmp_transport(
            outbound_policy,
            &invalid,
            target.scheme,
            "pull_targets[].scheme",
        )?;
        validate_rtmp_tc_url(target.tc_url.as_ref(), &invalid, "pull_targets[].tc_url")?;
        validate_rtmp_flash_version(
            target.flash_version.as_ref(),
            &invalid,
            "pull_targets[].flash_version",
        )?;
        validate_rtmp_credentials(
            target.credentials.as_ref(),
            &invalid,
            "pull_targets[].credentials",
        )?;
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

#[allow(clippy::too_many_lines)]
fn validate_rtmp_exec_profile(
    service: &str,
    applications: &[crate::model::RtmpApplication],
    profile: &mut RtmpExecProfile,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpServicePolicy {
        service: service.into(),
        field,
        detail,
    };
    if profile.name.len() > MAX_RTMP_EXEC_NAME_BYTES {
        return Err(invalid("exec_profiles[].name", "must be at most 64 bytes"));
    }
    if !applications
        .iter()
        .any(|application| application.name == profile.application)
    {
        return Err(invalid(
            "exec_profiles[].application",
            "must reference an exact application name",
        ));
    }
    normalize_exec_path(&mut profile.executable)
        .map_err(|detail| invalid("exec_profiles[].executable", detail))?;
    normalize_absolute_directory(&mut profile.working_directory)
        .map_err(|detail| invalid("exec_profiles[].working_directory", detail))?;
    if profile.filesystem == RtmpExecFilesystemPolicy::Host {
        return Err(invalid(
            "exec_profiles[].filesystem",
            "host filesystem access is not supported",
        ));
    }
    if profile.mode == RtmpExecMode::Transcode && profile.trigger != RtmpExecTrigger::Publisher {
        return Err(invalid(
            "exec_profiles[].trigger",
            "transcode profiles require the publisher trigger",
        ));
    }
    if profile
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_shell_executable_name)
    {
        return Err(invalid(
            "exec_profiles[].executable",
            "shell interpreters are not allowed",
        ));
    }
    let mut argv_bytes = 0_u64;
    if profile.arguments.len() > MAX_RTMP_EXEC_ARGUMENTS {
        return Err(invalid(
            "exec_profiles[].arguments",
            "must contain at most 64 arguments",
        ));
    }
    for argument in &profile.arguments {
        if argument.len() > MAX_RTMP_EXEC_ARGUMENT_BYTES
            || argument
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(invalid(
                "exec_profiles[].arguments",
                "arguments must be bounded UTF-8 without NUL or control bytes",
            ));
        }
        argv_bytes = argv_bytes
            .checked_add(u64::try_from(argument.len()).unwrap_or(u64::MAX))
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or_else(|| invalid("exec_profiles[].arguments", "argument byte count overflow"))?;
    }
    if argv_bytes > MAX_RTMP_EXEC_ARGV_BYTES {
        return Err(invalid(
            "exec_profiles[].arguments",
            "combined argument bytes exceed 16384",
        ));
    }
    if profile.environment.len() > MAX_RTMP_EXEC_ENVIRONMENT {
        return Err(invalid(
            "exec_profiles[].environment",
            "must contain at most 32 variables",
        ));
    }
    let mut environment_bytes = 0_u64;
    let mut environment_names = HashSet::new();
    for environment in &profile.environment {
        if environment.name.len() > MAX_RTMP_EXEC_ENV_NAME_BYTES
            || !valid_exec_environment_name(&environment.name)
            || is_forbidden_exec_environment_name(&environment.name)
        {
            return Err(invalid(
                "exec_profiles[].environment[].name",
                "must be an allowed ASCII environment name",
            ));
        }
        if !environment_names.insert(&environment.name) {
            return Err(invalid(
                "exec_profiles[].environment",
                "variable names must be unique",
            ));
        }
        if environment.value.len() > MAX_RTMP_EXEC_ENV_VALUE_BYTES
            || environment
                .value
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(invalid(
                "exec_profiles[].environment[].value",
                "values must be bounded UTF-8 without NUL or control bytes",
            ));
        }
        environment_bytes = environment_bytes
            .checked_add(u64::try_from(environment.name.len()).unwrap_or(u64::MAX))
            .and_then(|bytes| bytes.checked_add(1))
            .and_then(|bytes| {
                bytes.checked_add(u64::try_from(environment.value.len()).unwrap_or(u64::MAX))
            })
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or_else(|| {
                invalid(
                    "exec_profiles[].environment",
                    "environment byte count overflow",
                )
            })?;
    }
    if environment_bytes > MAX_RTMP_EXEC_ENV_BYTES {
        return Err(invalid(
            "exec_profiles[].environment",
            "combined environment bytes exceed 16384",
        ));
    }
    for (field, value, maximum, detail) in [
        (
            "exec_profiles[].timeout_ms",
            profile.timeout_ms,
            MAX_RTMP_EXEC_TIMEOUT_MS,
            "must be between 1 and 86400000",
        ),
        (
            "exec_profiles[].shutdown_timeout_ms",
            profile.shutdown_timeout_ms,
            MAX_RTMP_EXEC_SHUTDOWN_TIMEOUT_MS,
            "must be between 1 and 60000",
        ),
        (
            "exec_profiles[].max_processes",
            profile.max_processes,
            MAX_RTMP_EXEC_PROCESSES,
            "must be between 1 and 256",
        ),
        (
            "exec_profiles[].max_queue_messages",
            profile.max_queue_messages,
            MAX_RTMP_EXEC_QUEUE_MESSAGES,
            "must be between 1 and 65536",
        ),
        (
            "exec_profiles[].max_queue_bytes",
            profile.max_queue_bytes,
            MAX_RTMP_EXEC_QUEUE_BYTES,
            "must be between 1 and 1073741824",
        ),
        (
            "exec_profiles[].max_stdout_bytes",
            profile.max_stdout_bytes,
            MAX_RTMP_EXEC_STDOUT_BYTES,
            "must be between 1 and 16777216",
        ),
        (
            "exec_profiles[].max_stderr_bytes",
            profile.max_stderr_bytes,
            MAX_RTMP_EXEC_STDERR_BYTES,
            "must be between 1 and 16777216",
        ),
        (
            "exec_profiles[].respawn_delay_ms",
            profile.respawn_delay_ms,
            MAX_RTMP_EXEC_RESPAWN_DELAY_MS,
            "must be between 1 and 300000",
        ),
        (
            "exec_profiles[].max_respawns",
            profile.max_respawns,
            MAX_RTMP_EXEC_RESPAWNS,
            "must be between 0 and 64",
        ),
    ] {
        if (field != "exec_profiles[].max_respawns" && value == 0) || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    Ok(())
}

fn normalize_exec_path(path: &mut PathBuf) -> Result<(), &'static str> {
    let value = path.to_str().ok_or("path must be valid UTF-8")?;
    if !value.starts_with('/') {
        return Err("path must be absolute");
    }
    if value.is_empty() || value.ends_with('/') || value.len() > 4_096 {
        return Err("path must be a bounded executable path");
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err("path must not contain NUL or control bytes");
    }
    if value.strip_prefix('/').is_none_or(|value| {
        value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    }) {
        return Err("path must not contain empty, `.` or `..` segments");
    }
    *path = value.into();
    Ok(())
}

fn valid_exec_environment_name(name: &str) -> bool {
    let mut characters = name.bytes();
    matches!(characters.next(), Some(b'A'..=b'Z' | b'_'))
        && characters.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_forbidden_exec_environment_name(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "IFS" | "SHELL" | "LD_PRELOAD" | "LD_LIBRARY_PATH"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
}

fn is_shell_executable_name(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "fish" | "cmd" | "cmd.exe" | "powershell"
    )
}

#[allow(clippy::too_many_lines)]
fn validate_rtmp_hls(
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

fn validate_rtmp_dash(
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

fn validate_rtmp_callbacks(
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

fn validate_rtmp_outbound_policy(
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

fn validate_rtmp_relay_policy(
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

fn validate_rtmp_auto_push_policy(
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

fn validate_rtmp_transport(
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

fn validate_rtmp_relay_stream_name(
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

fn validate_rtmp_tc_url(
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

fn validate_rtmp_flash_version(
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

fn validate_rtmp_credentials(
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

fn valid_rtmp_literal_component(value: &str) -> bool {
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

#[allow(clippy::too_many_lines)]
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
    }
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
