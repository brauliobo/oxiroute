pub const RTMP_VERSION: u8 = 3;
pub const HANDSHAKE_BLOCK_SIZE: usize = 1536;

#[macro_use]
mod stream;

mod auto_push;
mod callback;
mod catalog;
mod client;
mod clock;
mod composition;
mod dash_segmenter;
mod exec;
mod exec_worker;
mod flv;
mod live;
mod media_parser;
mod media_segmenter;
mod media_snapshot;
mod media_storage;
mod recording_path;
mod recording_runtime;
mod recording_store;
mod recording_worker;
mod relay;
mod runtime_handles;
mod segment_window;
mod session;
mod session_control;
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
    MediaSnapshot, OperationId, PublisherRegistration, PublisherSnapshot, RecorderDefinition,
    RecorderErrorCode, RecorderId, RecorderPhase, RecorderSnapshot, RecordingAction, RelayId,
    RelaySnapshot, RtmpCapabilities, RtmpCatalogSnapshot, RtmpRegistry, RtmpRegistryWorkStats,
    RtmpStreamPath, RtmpStreamPathError, SessionId, StreamId, StreamKey, StreamSnapshot,
    SubscriberRegistration, TrackSnapshot,
};
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
pub use dash_segmenter::{DashOutputConfig, DashSegmentNaming};
pub use exec::{
    ExecEnvironment, ExecFilesystemPolicy, ExecLimits, ExecMode, ExecNetworkPolicy, ExecProfile,
    ExecProfileError, ExecTrigger,
};
pub use exec_worker::{
    ExecEnqueueResult, ExecWorkerCorrelation, ExecWorkerFailure, ExecWorkerPhase,
    ExecWorkerStartError, ExecWorkerStatus,
};
pub use flv::{
    FlvMuxer, FlvMuxerError, FlvTagType, MAX_CACHED_CODEC_HEADER_SIZE, MAX_FLV_TAG_DATA_SIZE,
};
pub use live::{
    LiveHub, LiveHubError, LiveHubLimits, LiveHubStats, MediaEvent, MediaEventError,
    MediaEventKind, PlaybackSubscription, PublishReport, PublisherIncarnation, PublisherLease,
    VideoCodec, VideoCodecIdentifier,
};
pub use media_segmenter::{
    HlsFragmentNaming, HlsKeyConfig, HlsOutputConfig, HlsValueError, HlsVariant, MediaApplication,
    MediaCatalog, MediaEnqueueResult, MediaObject, MediaOutputError, MediaPublisher,
};
pub use media_storage::{
    MAX_MEDIA_PATH_BYTES, MediaStore, MediaStoreError, MediaStoreLimits, MediaStoreStats,
    RtmpMediaStoreRegistry,
};
pub use recording_path::{
    MAX_RECORDING_FILENAME_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES, RecordingDateTime,
    RecordingPathError, RecordingPathPolicy, RecordingSegmentNaming, RecordingTimeBasis,
    RecordingTimezone,
};
pub use recording_runtime::{RtmpRecorderPolicy, RtmpRecorderShutdown, RtmpRecorderStart};
pub use recording_store::{
    RecorderLease, RecordingCommit, RecordingFile, RecordingQuotaScope, RecordingStore,
    RecordingStoreError, RecordingStoreLimits, RecordingStoreLimitsError, RecordingStoreStats,
    RtmpRecorderStoreRegistry,
};
pub use recording_worker::{
    RecorderEnqueueResult, RecorderFailure, RecorderMediaMask, RecorderNotification,
    RecorderShutdown, RecorderVideoCodec, RecorderWorker, RecorderWorkerConfig,
    RecorderWorkerPhase, RecorderWorkerStartError, RecorderWorkerStatus, RecorderWorkerSupervisor,
};
pub use relay::{
    RtmpDestination, RtmpDestinationResolver, RtmpDestinationResolverError, RtmpDnsRefreshFailure,
    RtmpDnsResolver, RtmpPullTarget, RtmpPushApplication, RtmpPushTarget, RtmpRelayConfig,
    RtmpRelayFailure, RtmpRelayPhase, RtmpRelayStatus,
};
pub use rml_rtmp::sessions::StreamMetadata;
pub use runtime_handles::{
    RtmpControlHandle, RtmpRuntimeSet, RtmpRuntimeSetError, RtmpServiceHandle, RtmpShutdown,
};
pub use session::{
    MAX_INBOUND_AMF0_CONTAINER_ENTRIES, MAX_INBOUND_AMF0_DEPTH, MAX_INBOUND_AMF0_STRING_BYTES,
    MAX_INBOUND_AMF0_VALUES, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE,
    MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, MAX_RTMP_MESSAGE_STREAMS,
    RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule,
    RtmpApplication, RtmpNetwork, RtmpRecorderLifecycle, RtmpServicePreparation,
    RtmpServiceRuntime, RtmpSession, RtmpSessionCeilings, RtmpSessionError, RtmpSessionLimitError,
    RtmpSessionLimits, RtmpSessionPolicy, RtmpTokenPolicy,
};
pub use session_control::{
    MAX_RTMP_SESSION_CONTROLS, RtmpClientSnapshot, RtmpMessageStreamSnapshot,
    RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome, RtmpSessionRole,
};
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
    let _ = media_parser::parse_avc_configuration(payload);
    let _ = media_parser::parse_aac_configuration(payload);
    media_segmenter::fuzz_media_configuration(payload);
    dash_segmenter::fuzz_media_configuration(payload);
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
