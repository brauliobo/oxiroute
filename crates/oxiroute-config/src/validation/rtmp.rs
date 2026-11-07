#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
struct RtmpRecorderStorageLimits {
    bytes: Option<u64>,
    files: Option<u64>,
    active_recorders: u64,
}

mod access;
mod exec;
mod hls;
mod policy;
mod recorder;
mod vod;

#[allow(clippy::wildcard_imports)]
use access::*;
#[allow(clippy::wildcard_imports)]
use exec::*;
#[allow(clippy::wildcard_imports)]
use hls::*;
#[allow(clippy::wildcard_imports)]
use policy::*;
#[allow(clippy::wildcard_imports)]
use recorder::*;
#[allow(clippy::wildcard_imports)]
use vod::*;

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
