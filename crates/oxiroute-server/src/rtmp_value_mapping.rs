//! Pure canonical RTMP conversion into opaque value-plan inputs.
//!
//! This module performs no acquisition. Production runtime preparation consumes the resulting
//! opaque plans and keeps environmental acquisition in `service_plan`.

use std::time::Duration;

use oxiroute_config::{
    RtmpAclAction, RtmpAutoPushPolicy, RtmpDashPolicy, RtmpDashSegmentNaming, RtmpExecEnvironment,
    RtmpExecFilesystemPolicy, RtmpExecMode, RtmpExecNetworkPolicy, RtmpExecProfile,
    RtmpExecTrigger, RtmpFanoutPolicy, RtmpHlsFragmentNaming, RtmpHlsKeyPolicy, RtmpHlsPolicy,
    RtmpHlsVariant, RtmpNotifyMethod, RtmpOutboundPolicy as ConfigOutboundPolicy, RtmpRecorder,
    RtmpRecorderSegmentNaming, RtmpRecorderStart as ConfigRecorderStart, RtmpRecorderTimeBasis,
    RtmpRecorderTimezone, RtmpRtmpsPolicy, RtmpTransport as ConfigTransport, RtmpVodPolicy,
    RtmpVodSource,
};
use oxiroute_rtmp::{
    DashSegmentNaming, ExecEnvironment, ExecFilesystemPolicy, ExecLimits, ExecMode,
    ExecNetworkPolicy, ExecTrigger, HlsFragmentNaming, HlsKeyConfig, HlsVariant, RecorderMediaMask,
    RecorderWorkerConfig, RecordingPathPolicy, RecordingSegmentNaming, RecordingStoreLimits,
    RecordingTimeBasis, RecordingTimezone, RtmpAccessAction, RtmpAutoPushConfig,
    RtmpCallbackMethod, RtmpNetwork, RtmpOutboundPolicy, RtmpPushApplication, RtmpRecorderStart,
    RtmpRtmpsMode, RtmpSessionCeilings, RtmpTransport, VodLimits, VodSourceDefinition,
};

pub(crate) fn outbound_policy(policy: &ConfigOutboundPolicy) -> RtmpOutboundPolicy {
    RtmpOutboundPolicy {
        allow_domains: policy.allow_domains.clone(),
        deny_domains: policy.deny_domains.clone(),
        allow_cidrs: policy.allow_cidrs.clone(),
        deny_cidrs: policy.deny_cidrs.clone(),
        deny_private: policy.deny_private,
        rtmps: match policy.rtmps {
            RtmpRtmpsPolicy::Disabled => RtmpRtmpsMode::Disabled,
            RtmpRtmpsPolicy::Allowed => RtmpRtmpsMode::Allowed,
            RtmpRtmpsPolicy::Required => RtmpRtmpsMode::Required,
        },
        max_chain_depth: policy.max_chain_depth,
    }
}

pub(crate) const fn transport(value: ConfigTransport) -> RtmpTransport {
    match value {
        ConfigTransport::Rtmp => RtmpTransport::Rtmp,
        ConfigTransport::Rtmps => RtmpTransport::Rtmps,
    }
}

pub(crate) const fn access_action(value: RtmpAclAction) -> RtmpAccessAction {
    match value {
        RtmpAclAction::Allow => RtmpAccessAction::Allow,
        RtmpAclAction::Deny => RtmpAccessAction::Deny,
    }
}

pub(crate) fn access_network(value: &str) -> Option<RtmpNetwork> {
    RtmpNetwork::parse(value)
}

pub(crate) fn push_application(value: &str) -> RtmpPushApplicationRef<'_> {
    if value == "$name" {
        RtmpPushApplicationRef::StreamName
    } else {
        RtmpPushApplicationRef::Exact(value)
    }
}

pub(crate) enum RtmpPushApplicationRef<'a> {
    StreamName,
    Exact(&'a str),
}

impl RtmpPushApplicationRef<'_> {
    pub(crate) fn to_owned(&self) -> RtmpPushApplication {
        match self {
            Self::StreamName => RtmpPushApplication::StreamName,
            Self::Exact(value) => RtmpPushApplication::Exact((*value).to_owned()),
        }
    }
}

pub(crate) fn auto_push(policy: &RtmpAutoPushPolicy) -> Option<RtmpAutoPushConfig> {
    policy.enabled.then(|| RtmpAutoPushConfig {
        enabled: true,
        socket_dir: policy.socket_dir.clone(),
        secret_file: policy.secret_file.clone(),
        reconnect_interval: Duration::from_millis(policy.reconnect_ms),
        connect_timeout: Duration::from_millis(policy.connect_timeout_ms),
        handshake_timeout: Duration::from_millis(policy.handshake_timeout_ms),
        max_peers: usize::try_from(policy.max_peers).expect("validated auto-push peers"),
        max_queue_messages: usize::try_from(policy.max_queue_messages)
            .expect("validated auto-push queue messages"),
        max_queue_bytes: usize::try_from(policy.max_queue_bytes)
            .expect("validated auto-push queue bytes"),
        max_streams: usize::try_from(policy.max_streams).expect("validated auto-push streams"),
    })
}

pub(crate) const fn callback_method(value: RtmpNotifyMethod) -> RtmpCallbackMethod {
    match value {
        RtmpNotifyMethod::Get => RtmpCallbackMethod::Get,
        RtmpNotifyMethod::Post => RtmpCallbackMethod::Post,
    }
}

pub(crate) fn fanout(policy: RtmpFanoutPolicy) -> (usize, usize, usize) {
    (
        usize::try_from(policy.max_subscribers).expect("validated fanout subscribers"),
        usize::try_from(policy.max_queue_messages_per_subscriber)
            .expect("validated fanout queue messages"),
        usize::try_from(policy.max_queue_bytes_per_subscriber)
            .expect("validated fanout queue bytes"),
    )
}

pub(crate) fn session_ceilings(value: oxiroute_config::RtmpSessionCeilings) -> RtmpSessionCeilings {
    RtmpSessionCeilings::new(
        usize::try_from(value.max_connections).expect("validated application connections"),
        usize::try_from(value.max_publishers).expect("validated application publishers"),
        usize::try_from(value.max_viewers).expect("validated application viewers"),
    )
}

pub(crate) fn hls_variant(value: &RtmpHlsVariant) -> HlsVariant {
    HlsVariant {
        name: value.name.clone(),
        bandwidth: value.bandwidth,
        codecs: value.codecs.clone(),
        width: value.width,
        height: value.height,
    }
}

pub(crate) fn hls_key(value: &RtmpHlsKeyPolicy) -> HlsKeyConfig {
    HlsKeyConfig {
        rotation_segments: usize::try_from(value.rotation_segments)
            .expect("validated HLS key rotation"),
        url_prefix: value.url_prefix.clone(),
    }
}

pub(crate) const fn hls_naming(value: RtmpHlsFragmentNaming) -> HlsFragmentNaming {
    match value {
        RtmpHlsFragmentNaming::Sequential => HlsFragmentNaming::Sequential,
        RtmpHlsFragmentNaming::Timestamp => HlsFragmentNaming::Timestamp,
        RtmpHlsFragmentNaming::System => HlsFragmentNaming::System,
    }
}

pub(crate) const fn dash_naming(value: RtmpDashSegmentNaming) -> DashSegmentNaming {
    match value {
        RtmpDashSegmentNaming::Sequential => DashSegmentNaming::Sequential,
        RtmpDashSegmentNaming::Timestamp => DashSegmentNaming::Timestamp,
        RtmpDashSegmentNaming::System => DashSegmentNaming::System,
    }
}

pub(crate) fn media_store_limits(
    max_storage_bytes: u64,
    max_storage_files: u64,
    max_active_streams: u64,
    max_segment_bytes: u64,
) -> oxiroute_rtmp::MediaStoreLimits {
    oxiroute_rtmp::MediaStoreLimits {
        max_bytes: max_storage_bytes,
        max_files: usize::try_from(max_storage_files).expect("validated media storage files"),
        max_active_streams: usize::try_from(max_active_streams)
            .expect("validated media active streams"),
        max_file_bytes: usize::try_from(max_segment_bytes).expect("validated media segment bytes"),
    }
}

pub(crate) fn vod(policy: &RtmpVodPolicy) -> (VodLimits, Vec<VodSourceDefinition>) {
    let limits = VodLimits {
        max_sessions: usize::try_from(policy.max_sessions).expect("validated VOD sessions"),
        max_file_bytes: policy.max_file_bytes,
        max_duration: Duration::from_millis(policy.max_duration_ms),
    };
    let sources = policy
        .sources
        .iter()
        .map(|source| match source {
            RtmpVodSource::Local {
                name,
                root_directory,
            } => VodSourceDefinition::Local {
                name: name.clone(),
                root_directory: root_directory.clone(),
            },
            RtmpVodSource::Http { name, origin } => VodSourceDefinition::Http {
                name: name.clone(),
                origin: origin.clone(),
            },
        })
        .collect();
    (limits, sources)
}

pub(crate) fn exec_environment(value: &RtmpExecEnvironment) -> ExecEnvironment {
    ExecEnvironment::new(value.name.clone(), value.value.clone())
        .expect("validated exec environment")
}

pub(crate) fn exec_limits(profile: &RtmpExecProfile) -> ExecLimits {
    ExecLimits::new(
        usize::try_from(profile.max_queue_messages).expect("validated exec queue messages"),
        usize::try_from(profile.max_queue_bytes).expect("validated exec queue bytes"),
        usize::try_from(profile.max_stdout_bytes).expect("validated exec stdout bytes"),
        usize::try_from(profile.max_stderr_bytes).expect("validated exec stderr bytes"),
        Duration::from_millis(profile.timeout_ms),
        Duration::from_millis(profile.shutdown_timeout_ms),
        usize::try_from(profile.max_processes).expect("validated exec processes"),
        Duration::from_millis(profile.respawn_delay_ms),
        usize::try_from(profile.max_respawns).expect("validated exec respawns"),
    )
    .expect("validated exec limits")
}

pub(crate) const fn exec_mode(value: RtmpExecMode) -> ExecMode {
    match value {
        RtmpExecMode::Command => ExecMode::Command,
        RtmpExecMode::Transcode => ExecMode::Transcode,
    }
}

pub(crate) const fn exec_trigger(value: RtmpExecTrigger) -> ExecTrigger {
    match value {
        RtmpExecTrigger::Publisher => ExecTrigger::Publisher,
        RtmpExecTrigger::PublishDone => ExecTrigger::PublishDone,
    }
}

pub(crate) fn exec_filesystem(value: RtmpExecFilesystemPolicy) -> Option<ExecFilesystemPolicy> {
    match value {
        RtmpExecFilesystemPolicy::WorkingDirectory => Some(ExecFilesystemPolicy::WorkingDirectory),
        RtmpExecFilesystemPolicy::Host => None,
    }
}

pub(crate) const fn exec_network(value: RtmpExecNetworkPolicy) -> ExecNetworkPolicy {
    match value {
        RtmpExecNetworkPolicy::Disabled => ExecNetworkPolicy::Disabled,
        RtmpExecNetworkPolicy::Inherited => ExecNetworkPolicy::Inherited,
    }
}

pub(crate) fn recorder_path(value: &RtmpRecorder) -> RecordingPathPolicy {
    RecordingPathPolicy::new(&value.suffix_template, value.append_unix_seconds)
        .expect("validated recorder path policy")
        .with_segment_policy(
            match &value.timezone {
                RtmpRecorderTimezone::Utc => RecordingTimezone::Utc,
                RtmpRecorderTimezone::Iana(name) => {
                    RecordingTimezone::Iana(name.parse().expect("validated IANA recorder timezone"))
                }
            },
            match value.time_basis {
                RtmpRecorderTimeBasis::SegmentStart => RecordingTimeBasis::SegmentStart,
                RtmpRecorderTimeBasis::SegmentEnd => RecordingTimeBasis::SegmentEnd,
            },
            match value.segment_naming {
                RtmpRecorderSegmentNaming::SafeUnique => RecordingSegmentNaming::SafeUnique,
                RtmpRecorderSegmentNaming::NginxCompatible => {
                    RecordingSegmentNaming::NginxCompatible
                }
            },
        )
}

pub(crate) const fn recorder_start(value: ConfigRecorderStart) -> RtmpRecorderStart {
    match value {
        ConfigRecorderStart::Continuous => RtmpRecorderStart::Continuous,
        ConfigRecorderStart::Manual => RtmpRecorderStart::Manual,
    }
}

pub(crate) fn recorder_worker(value: &RtmpRecorder) -> RecorderWorkerConfig {
    RecorderWorkerConfig {
        max_queue_messages: usize::try_from(value.max_queue_messages)
            .expect("validated recorder queue messages"),
        max_queue_bytes: usize::try_from(value.max_queue_bytes)
            .expect("validated recorder queue bytes"),
        rotation_interval: value.rotation_interval_ms.map(Duration::from_millis),
        shutdown_timeout: Duration::from_millis(value.shutdown_timeout_ms),
        video_codec: None,
        record_mask: RecorderMediaMask::new(
            value.record_mask.audio,
            value.record_mask.video,
            value.record_mask.keyframes,
        ),
        append: value.append,
        lock: value.lock,
        max_size: value.max_size,
        max_frames: value.max_frames,
        notify: value.notify,
    }
}

pub(crate) fn recorder_store(value: &RtmpRecorder) -> RecordingStoreLimits {
    RecordingStoreLimits {
        max_bytes: value.max_storage_bytes,
        max_files: value
            .max_storage_files
            .map(|files| usize::try_from(files).expect("validated recorder storage files")),
        max_active_recorders: usize::try_from(value.max_active_recorders)
            .expect("validated active recorders"),
    }
}

pub(crate) fn hls_durations(value: &RtmpHlsPolicy) -> (Duration, Duration, Duration) {
    (
        Duration::from_millis(value.segment_duration_ms),
        Duration::from_millis(value.max_segment_duration_ms),
        Duration::from_millis(value.playlist_length_ms),
    )
}

pub(crate) fn dash_durations(value: &RtmpDashPolicy) -> (Duration, Duration, Duration) {
    (
        Duration::from_millis(value.segment_duration_ms),
        Duration::from_millis(value.max_segment_duration_ms),
        Duration::from_millis(value.playlist_length_ms),
    )
}
