pub const RTMP_VERSION: u8 = 3;
pub const HANDSHAKE_BLOCK_SIZE: usize = 1536;

#[cfg(test)]
extern crate self as oxiroute_rtmp;

#[macro_use]
mod stream;

mod auto_push;
mod callback;
mod catalog;
mod client;
mod clock;
mod composition;
mod exec;
mod flv;
mod live;
mod media;
mod recording;
mod relay;
mod runtime_handles;
mod session;
mod vod;

pub use auto_push::{
    RtmpAutoPushConfig, RtmpAutoPushConfigError, RtmpAutoPushError, RtmpAutoPushStatus,
};
pub use callback::{
    RtmpCallbackContext, RtmpCallbackEndpoint, RtmpCallbackEndpointBlueprint, RtmpCallbackError,
    RtmpCallbackEvent, RtmpCallbackMethod, RtmpCallbackPolicy, RtmpCallbackValueError,
    validate_callback_url_intrinsic,
};
pub use catalog::{
    CatalogError, MAX_RTMP_APPLICATION_BYTES, MAX_RTMP_QUERY_BYTES, MAX_RTMP_STREAM_NAME_BYTES,
    MediaSnapshot, OperationId, PublisherSnapshot, RecorderDefinition, RecorderErrorCode,
    RecorderId, RecorderPhase, RecorderSnapshot, RecordingAction, RelayId, RelaySnapshot,
    RtmpCapabilities, RtmpCatalogSnapshot, RtmpRegistryWorkStats, RtmpStreamPath,
    RtmpStreamPathError, SessionId, StreamId, StreamKey, StreamSnapshot, TrackSnapshot,
};
pub(crate) use catalog::{PublisherRegistration, RtmpRegistry, SubscriberRegistration};
pub use client::{
    DestinationPolicyError, RtmpClientOptions, RtmpCredential, RtmpOutboundPolicy, RtmpRtmpsMode,
    RtmpTransport,
};
pub use composition::{
    RtmpAccessPlan, RtmpAccessRulePlan, RtmpApplicationPlan, RtmpAutoPushPlan,
    RtmpCallbackEventPlan, RtmpCallbackPlan, RtmpClientPlan, RtmpCredentialPlan, RtmpDashPlan,
    RtmpExecEnvironmentPlan, RtmpExecPlan, RtmpFanoutPlan, RtmpHlsPlan, RtmpMediaPlan,
    RtmpPrepareCategory, RtmpPrepareContext, RtmpPrepareError, RtmpPrepareMode, RtmpPrepareSource,
    RtmpPullPlan, RtmpPushPlan, RtmpRecorderPlan, RtmpRelayPlan, RtmpServicePlan, RtmpTokenPlan,
    RtmpVodPlan,
};
pub use exec::worker::{
    ExecEnqueueResult, ExecWorkerCorrelation, ExecWorkerFailure, ExecWorkerPhase,
    ExecWorkerStartError, ExecWorkerStatus,
};
pub use exec::{
    ExecEnvironment, ExecFilesystemPolicy, ExecLimits, ExecMode, ExecNetworkPolicy, ExecProfile,
    ExecProfileError, ExecTrigger,
};
pub use flv::{
    FlvMuxer, FlvMuxerError, FlvTagType, MAX_CACHED_CODEC_HEADER_SIZE, MAX_FLV_TAG_DATA_SIZE,
};
pub(crate) use live::{LiveHub, PlaybackSubscription, PublisherLease};
pub use live::{
    LiveHubError, LiveHubLimits, LiveHubStats, MediaEvent, MediaEventError, MediaEventKind,
    PublishReport, PublisherIncarnation, VideoCodec, VideoCodecIdentifier,
};
pub use media::dash::{DashOutputConfig, DashSegmentNaming};
pub use media::segmenter::{
    HlsFragmentNaming, HlsKeyConfig, HlsOutputConfig, HlsValueError, HlsVariant, MediaApplication,
    MediaCatalog, MediaEnqueueResult, MediaObject, MediaOutputError, MediaPublisher,
};
pub use media::storage::{
    MAX_MEDIA_PATH_BYTES, MediaStoreError, MediaStoreLimits, MediaStoreStats,
};
pub(crate) use media::storage::{MediaStore, RtmpMediaStoreRegistry};
pub use recording::path::{
    MAX_RECORDING_FILENAME_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES, RecordingDateTime,
    RecordingPathError, RecordingPathPolicy, RecordingSegmentNaming, RecordingTimeBasis,
    RecordingTimezone,
};
pub(crate) use recording::recording_store::{
    RecorderLease, RecordingCommit, RecordingFile, RecordingStore, RtmpRecorderStoreRegistry,
};
pub use recording::recording_store::{
    RecordingQuotaScope, RecordingStoreError, RecordingStoreLimits, RecordingStoreLimitsError,
    RecordingStoreStats,
};
#[cfg(test)]
pub(crate) use recording::recording_worker::RecorderShutdown;
pub use recording::recording_worker::{
    RecorderEnqueueResult, RecorderFailure, RecorderMediaMask, RecorderNotification,
    RecorderVideoCodec, RecorderWorkerConfig, RecorderWorkerPhase, RecorderWorkerStartError,
    RecorderWorkerStatus,
};
pub(crate) use recording::recording_worker::{RecorderWorker, RecorderWorkerSupervisor};
pub use recording::runtime::{RtmpRecorderPolicy, RtmpRecorderShutdown, RtmpRecorderStart};
pub use relay::{
    RtmpDestination, RtmpDestinationResolver, RtmpDestinationResolverError, RtmpDnsRefreshFailure,
    RtmpDnsResolver, RtmpPullTarget, RtmpPushApplication, RtmpPushTarget, RtmpRelayConfig,
    RtmpRelayFailure, RtmpRelayPhase, RtmpRelayStatus,
};
pub use rml_rtmp::sessions::StreamMetadata;
pub use runtime_handles::{
    PreparedRtmpRuntimeSet, RtmpControlHandle, RtmpRuntimeSet, RtmpRuntimeSetError,
    RtmpServiceHandle, RtmpShutdown,
};
pub use session::control::{
    MAX_RTMP_SESSION_CONTROLS, RtmpClientSnapshot, RtmpMessageStreamSnapshot,
    RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome, RtmpSessionRole,
};
pub use session::{
    MAX_INBOUND_AMF0_CONTAINER_ENTRIES, MAX_INBOUND_AMF0_DEPTH, MAX_INBOUND_AMF0_STRING_BYTES,
    MAX_INBOUND_AMF0_VALUES, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE,
    MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, MAX_RTMP_MESSAGE_STREAMS,
    RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule,
    RtmpApplication, RtmpNetwork, RtmpSession, RtmpSessionCeilings, RtmpSessionError,
    RtmpSessionLimitError, RtmpSessionLimits, RtmpSessionPolicy, RtmpTokenPolicy,
};
pub(crate) use session::{RtmpRecorderLifecycle, RtmpServicePreparation, RtmpServiceRuntime};
pub use vod::{
    MAX_VOD_EVENTS, MAX_VOD_HTTP_HEADER_BYTES, MAX_VOD_ORIGIN_BYTES, MAX_VOD_PATH_BYTES,
    MAX_VOD_REDIRECTS, MAX_VOD_SOURCE_NAME_BYTES, VodApplication, VodApplicationBlueprint,
    VodCatalog, VodError, VodLease, VodLimits, VodObject, VodRange, VodSourceDefinition,
    VodValueError,
};

/// Exercises structural media-configuration parsing and output-specific policies.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_media_configuration(payload: &[u8]) {
    let _ = media::parser::parse_avc_configuration(payload);
    let _ = media::parser::parse_aac_configuration(payload);
    media::segmenter::fuzz_media_configuration(payload);
    media::dash::fuzz_media_configuration(payload);
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum HandshakeError {
    #[error("RTMP client hello must contain 1537 bytes, got {0}")]
    InvalidLength(usize),
    #[error("unsupported RTMP version {0}")]
    UnsupportedVersion(u8),
}

/// Builds the server response for an RTMP simple handshake client hello (`C0+C1`).
///
/// # Errors
///
/// Returns an error when the hello length or RTMP version is invalid.
pub fn simple_handshake_response(
    client_hello: &[u8],
    server_time: u32,
    random: &[u8; HANDSHAKE_BLOCK_SIZE - 8],
) -> Result<Vec<u8>, HandshakeError> {
    if client_hello.len() != HANDSHAKE_BLOCK_SIZE + 1 {
        return Err(HandshakeError::InvalidLength(client_hello.len()));
    }
    if client_hello[0] != RTMP_VERSION {
        return Err(HandshakeError::UnsupportedVersion(client_hello[0]));
    }

    let mut response = Vec::with_capacity(1 + HANDSHAKE_BLOCK_SIZE * 2);
    response.push(RTMP_VERSION);
    response.extend_from_slice(&server_time.to_be_bytes());
    response.extend_from_slice(&[0; 4]);
    response.extend_from_slice(random);
    response.extend_from_slice(&client_hello[1..]);
    Ok(response)
}

#[cfg(test)]
#[path = "../tests/amf_hardening.rs"]
mod amf_hardening_tests;
#[cfg(test)]
#[path = "../tests/catalog.rs"]
mod catalog_tests;
#[cfg(test)]
#[path = "../tests/live_fanout.rs"]
mod live_fanout_tests;
#[cfg(test)]
#[path = "../tests/multi_stream_session.rs"]
mod multi_stream_session_tests;
#[cfg(test)]
#[path = "../tests/playback_session.rs"]
mod playback_session_tests;
#[cfg(test)]
#[path = "../tests/publish_session.rs"]
mod publish_session_tests;
#[cfg(test)]
#[path = "../tests/pull_relay.rs"]
mod pull_relay_tests;
#[cfg(test)]
#[path = "../tests/push_relay.rs"]
mod push_relay_tests;
#[cfg(test)]
#[path = "../tests/recorder_session.rs"]
mod recorder_session_tests;
#[cfg(test)]
#[path = "../tests/recording_store.rs"]
mod recording_store_tests;
#[cfg(test)]
#[path = "../tests/recording_worker.rs"]
mod recording_worker_tests;
#[cfg(test)]
#[path = "../tests/service_plans.rs"]
mod service_plan_tests;
#[cfg(test)]
#[path = "../tests/session_policy.rs"]
mod session_policy_tests;
