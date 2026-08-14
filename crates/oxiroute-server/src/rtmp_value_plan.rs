use std::time::Duration;

use oxiroute_config::{ConfigDraft, RtmpAccessPolicy as ConfigRtmpAccessPolicy};
use oxiroute_rtmp::RtmpSessionLimits;

use crate::rtmp_value_mapping as rtmp_map;

#[allow(clippy::too_many_lines)]
pub(crate) fn compile_rtmp_value_plans_from_draft(
    config: &ConfigDraft,
) -> Result<Vec<oxiroute_rtmp::RtmpServicePlan>, oxiroute_rtmp::RtmpPrepareError> {
    config
        .rtmp_services
        .iter()
        .map(|service| {
            let build = || -> Result<oxiroute_rtmp::RtmpServicePlan, oxiroute_rtmp::RtmpPrepareError> {
            let outbound_policy = rtmp_map::outbound_policy(&service.outbound_policy);
            let applications = service
                .applications
                .iter()
                .map(|application| {
                    let build = || -> Result<oxiroute_rtmp::RtmpApplicationPlan, oxiroute_rtmp::RtmpPrepareError> {
                    let access = |policy: &ConfigRtmpAccessPolicy| {
                        let rules = policy
                            .rules
                            .iter()
                            .map(|rule| {
                                let action = rtmp_map::access_action(rule.action);
                                let network = rtmp_map::access_network(&rule.network)
                                    .expect("validated RTMP network");
                                oxiroute_rtmp::RtmpAccessRulePlan::new(action, network)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let token = policy
                            .token
                            .as_ref()
                            .map(|token| {
                                oxiroute_rtmp::RtmpTokenPlan::new(
                                    token.parameter.clone(),
                                    token.secret.as_bytes(),
                                )
                            })
                            .transpose()?;
                        Ok::<_, oxiroute_rtmp::RtmpPrepareError>(
                            oxiroute_rtmp::RtmpAccessPlan::new(rules, token),
                        )
                    };
                    let client = |tc_url: &Option<String>,
                                  flash_version: &Option<String>,
                                  credential: Option<&oxiroute_config::RtmpCredentialReference>| {
                        let credential = credential
                            .map(|credential| {
                                oxiroute_rtmp::RtmpCredentialPlan::new(
                                    credential.username.clone(),
                                    credential.secret_file.clone(),
                                )
                            })
                            .transpose()?;
                        oxiroute_rtmp::RtmpClientPlan::new(
                            flash_version
                                .clone()
                                .unwrap_or_else(|| "WIN 23,0,0,207".into()),
                            2_000,
                            tc_url.clone(),
                            credential,
                        )
                    };
                    let relay = oxiroute_rtmp::RtmpRelayPlan::new(
                        outbound_policy.clone(),
                        usize::try_from(application.relay.max_queue_messages)
                            .expect("validated RTMP relay queue messages"),
                        usize::try_from(application.relay.max_queue_bytes)
                            .expect("validated RTMP relay queue bytes"),
                        Duration::from_millis(application.relay.buffer_ms),
                        Duration::from_millis(application.relay.push_reconnect_ms),
                        Duration::from_millis(application.relay.pull_reconnect_ms),
                        Duration::from_millis(application.relay.dns_refresh_ms),
                        Duration::from_millis(application.relay.connect_timeout_ms),
                        Duration::from_millis(application.relay.handshake_timeout_ms),
                        application
                            .push_targets
                            .iter()
                            .map(|target| {
                                oxiroute_rtmp::RtmpPushPlan::new(
                                    target.host.clone(),
                                    target.port,
                                    rtmp_map::transport(target.scheme),
                                    rtmp_map::push_application(&target.application).to_owned(),
                                    target.stream_name.clone(),
                                    client(
                                        &target.tc_url,
                                        &target.flash_version,
                                        target.credentials.as_ref(),
                                    )?,
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        application
                            .pull_targets
                            .iter()
                            .map(|target| {
                                oxiroute_rtmp::RtmpPullPlan::new(
                                    target.host.clone(),
                                    target.port,
                                    rtmp_map::transport(target.scheme),
                                    target.application.clone(),
                                    target.stream_name.clone(),
                                    application.name.clone(),
                                    target.stream_name.clone(),
                                    client(
                                        &target.tc_url,
                                        &target.flash_version,
                                        target.credentials.as_ref(),
                                    )?,
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    )?;
                    let hls = application
                        .hls
                        .as_ref()
                        .map(|policy| {
                            oxiroute_rtmp::RtmpHlsPlan::new(
                                policy.root_directory.clone(),
                                rtmp_map::hls_durations(policy).0,
                                rtmp_map::hls_durations(policy).1,
                                rtmp_map::hls_durations(policy).2,
                                rtmp_map::hls_naming(policy.fragment_naming),
                                policy.nested,
                                policy.cleanup,
                                policy.variants.iter().map(rtmp_map::hls_variant),
                                policy.keys.as_ref().map(rtmp_map::hls_key),
                                usize::try_from(policy.max_segment_bytes)
                                    .expect("validated HLS segment bytes"),
                                usize::try_from(policy.max_queue_messages)
                                    .expect("validated HLS queue messages"),
                                policy.max_storage_bytes,
                                usize::try_from(policy.max_storage_files)
                                    .expect("validated HLS storage files"),
                                usize::try_from(policy.max_active_streams)
                                    .expect("validated HLS active streams"),
                            )
                        })
                        .transpose()?;
                    let dash = application
                        .dash
                        .as_ref()
                        .map(|policy| {
                            oxiroute_rtmp::RtmpDashPlan::new(
                                policy.root_directory.clone(),
                                rtmp_map::dash_durations(policy).0,
                                rtmp_map::dash_durations(policy).1,
                                rtmp_map::dash_durations(policy).2,
                                rtmp_map::dash_naming(policy.segment_naming),
                                policy.nested,
                                policy.cleanup,
                                usize::try_from(policy.max_segment_bytes)
                                    .expect("validated DASH segment bytes"),
                                usize::try_from(policy.max_queue_messages)
                                    .expect("validated DASH queue messages"),
                                policy.max_storage_bytes,
                                usize::try_from(policy.max_storage_files)
                                    .expect("validated DASH storage files"),
                                usize::try_from(policy.max_active_streams)
                                    .expect("validated DASH active streams"),
                            )
                        })
                        .transpose()?;
                    let media = if hls.is_some() || dash.is_some() {
                        Some(oxiroute_rtmp::RtmpMediaPlan::new(hls, dash)?)
                    } else {
                        None
                    };
                    let vod = application
                        .vod
                        .as_ref()
                        .map(|policy| {
                            let (limits, sources) = rtmp_map::vod(policy);
                            oxiroute_rtmp::RtmpVodPlan::new(
                                limits,
                                sources,
                                outbound_policy.clone(),
                            )
                        })
                        .transpose()?;
                    let recorders = application
                        .recorders
                        .iter()
                        .map(|recorder| {
                            let path_policy = rtmp_map::recorder_path(recorder);
                            oxiroute_rtmp::RtmpRecorderPlan::new(
                                recorder.name.clone(),
                                rtmp_map::recorder_start(recorder.start),
                                recorder.root_directory.clone(),
                                path_policy,
                                rtmp_map::recorder_worker(recorder),
                                rtmp_map::recorder_store(recorder),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let exec = service
                        .exec_profiles
                        .iter()
                        .filter(|profile| profile.application == application.name)
                        .map(|profile| {
                            let environment = profile
                                .environment
                                .iter()
                                .map(|entry| {
                                    oxiroute_rtmp::RtmpExecEnvironmentPlan::new(
                                        rtmp_map::exec_environment(entry).name(),
                                        entry.value.clone(),
                                    )
                                    .map_err(|error| {
                                        error.contextualize_profile(&profile.name)
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            let limits = rtmp_map::exec_limits(profile);
                            oxiroute_rtmp::RtmpExecPlan::new(
                                profile.name.clone(),
                                profile.application.clone(),
                                rtmp_map::exec_mode(profile.mode),
                                rtmp_map::exec_trigger(profile.trigger),
                                profile.executable.clone(),
                                profile.arguments.clone(),
                                environment,
                                profile.working_directory.clone(),
                                rtmp_map::exec_filesystem(profile.filesystem)
                                    .expect("validated supported exec filesystem"),
                                rtmp_map::exec_network(profile.network),
                                limits,
                                profile.respawn,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    oxiroute_rtmp::RtmpApplicationPlan::new(
                        application.name.clone(),
                        application.live,
                        application.idle_streams,
                        access(&application.publish)?,
                        access(&application.play)?,
                        rtmp_map::session_ceilings(application.limits),
                        oxiroute_rtmp::RtmpFanoutPlan::new(
                            rtmp_map::fanout(application.fanout).0,
                            rtmp_map::fanout(application.fanout).1,
                            rtmp_map::fanout(application.fanout).2,
                        )?,
                        relay,
                        media,
                        recorders,
                        vod,
                        compile_rtmp_callback_value_plan(&application.callbacks)?,
                        exec,
                    )
                    };
                    build().map_err(|error| error.contextualize_application(&application.name))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let auto_push = rtmp_map::auto_push(&service.auto_push)
                .map(oxiroute_rtmp::RtmpAutoPushPlan::new)
                .transpose()?;
            oxiroute_rtmp::RtmpServicePlan::new(
                service.name.clone(),
                service.outbound_chunk_size,
                RtmpSessionLimits::default()
                    .with_max_inbound_message_size(
                        usize::try_from(service.max_inbound_message_size)
                            .expect("validated inbound message size"),
                    )
                    .with_window_ack_size(service.ack_window_size),
                compile_rtmp_callback_value_plan(&service.callbacks)?,
                applications,
                auto_push,
            )?
            .with_outbound_policy(outbound_policy)
            };
            build().map_err(|error| error.contextualize_service(&service.name))
        })
        .collect()
}

fn compile_rtmp_callback_value_plan(
    callbacks: &oxiroute_config::RtmpCallbackConfig,
) -> Result<oxiroute_rtmp::RtmpCallbackPlan, oxiroute_rtmp::RtmpPrepareError> {
    let mut plan = oxiroute_rtmp::RtmpCallbackPlan::new(
        rtmp_map::callback_method(callbacks.notify_method),
        Duration::from_millis(callbacks.timeout_ms),
        Duration::from_millis(callbacks.notify_update_timeout_ms),
    )?
    .with_update_policy(
        callbacks.notify_update_strict,
        callbacks.notify_relay_redirect,
    );
    for (event, endpoint) in [
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Connect,
            &callbacks.on_connect,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Disconnect,
            &callbacks.on_disconnect,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Publish,
            &callbacks.on_publish,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::PublishDone,
            &callbacks.on_publish_done,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Play,
            &callbacks.on_play,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::PlayDone,
            &callbacks.on_play_done,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Done,
            &callbacks.on_done,
        ),
        (
            oxiroute_rtmp::RtmpCallbackEventPlan::Update,
            &callbacks.on_update,
        ),
    ] {
        if let Some(endpoint) = endpoint {
            plan = plan.with_endpoint(event, endpoint.clone())?;
        }
    }
    Ok(plan)
}
