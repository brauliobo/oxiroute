pub const RTMP_VERSION: u8 = 3;
pub const HANDSHAKE_BLOCK_SIZE: usize = 1536;

mod catalog;
mod directives;
mod flv;
mod live;
mod nginx;
mod recording_path;
mod recording_runtime;
mod recording_store;
mod recording_worker;
mod relay;
mod session;

pub use catalog::{
    CatalogError, MAX_RTMP_APPLICATION_BYTES, MAX_RTMP_QUERY_BYTES, MAX_RTMP_STREAM_NAME_BYTES,
    MediaSnapshot, OperationId, PublisherRegistration, PublisherSnapshot, RecorderDefinition,
    RecorderErrorCode, RecorderId, RecorderPhase, RecorderSnapshot, RecordingAction, RelayId,
    RelaySnapshot, RtmpCapabilities, RtmpCatalogSnapshot, RtmpRegistry, RtmpRegistryWorkStats,
    RtmpStreamPath, RtmpStreamPathError, SessionId, StreamId, StreamKey, StreamSnapshot,
    SubscriberRegistration, TrackSnapshot,
};
pub use directives::{directive_specs, validate_directive};
pub use flv::{
    FlvMuxer, FlvMuxerError, FlvTagType, MAX_CACHED_CODEC_HEADER_SIZE, MAX_FLV_TAG_DATA_SIZE,
};
pub use live::{
    LiveHub, LiveHubError, LiveHubLimits, LiveHubStats, MediaEvent, MediaEventError,
    MediaEventKind, PlaybackSubscription, PublishReport, PublisherIncarnation, PublisherLease,
    VideoCodec, VideoCodecIdentifier,
};
pub use nginx::{NginxDirective, NginxParseError, parse_nginx_config};
pub use recording_path::{
    MAX_RECORDING_FILENAME_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES, RecordingDateTime,
    RecordingPathError, RecordingPathPolicy, RecordingSegmentNaming, RecordingTimeBasis,
    RecordingTimezone,
};
pub use recording_runtime::{RtmpRecorderPolicy, RtmpRecorderShutdown, RtmpRecorderStart};
pub use recording_store::{
    RecorderLease, RecordingCommit, RecordingFile, RecordingQuotaScope, RecordingStore,
    RecordingStoreError, RecordingStoreLimits, RecordingStoreStats,
};
pub use recording_worker::{
    RecorderEnqueueResult, RecorderFailure, RecorderShutdown, RecorderVideoCodec, RecorderWorker,
    RecorderWorkerConfig, RecorderWorkerPhase, RecorderWorkerStartError, RecorderWorkerStatus,
    RecorderWorkerSupervisor,
};
pub use relay::{
    RtmpDestination, RtmpPushApplication, RtmpPushTarget, RtmpRelayConfig, RtmpRelayFailure,
    RtmpRelayPhase, RtmpRelayStatus,
};
pub use rml_rtmp::sessions::StreamMetadata;
pub use session::{
    MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE, MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN,
    RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpApplication, RtmpRecorderLifecycle, RtmpServiceRuntime,
    RtmpSession, RtmpSessionError, RtmpSessionPolicy,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DirectiveContext {
    NginxMain,
    RtmpMain,
    RtmpServer,
    RtmpApplication,
    RtmpRecorder,
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayKind {
    Push,
    Pull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    AccessLog,
    AccessRule,
    Bitmask(&'static [&'static str]),
    Block,
    Command,
    Duration,
    DurationOrOff,
    Enum(&'static [&'static str]),
    Flag,
    HlsVariant,
    Integer,
    Listen,
    LogFormat,
    NamedBlock,
    Path,
    RelayTarget(RelayKind),
    Signal,
    Size,
    Strings,
    Url,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSupport {
    ParsedNotEnforced,
    SourceNoOp,
    SourceBug,
    Deprecated,
    PlatformLimited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveSpec {
    pub name: &'static str,
    pub contexts: &'static [DirectiveContext],
    pub min_args: u8,
    pub max_args: Option<u8>,
    pub value_kind: ValueKind,
    pub default: Option<&'static str>,
    pub repeatable: bool,
    pub runtime_support: RuntimeSupport,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum DirectiveError {
    #[error("unknown nginx-rtmp directive `{0}`")]
    UnknownDirective(String),
    #[error("directive `{name}` is not valid in {context:?}")]
    InvalidContext {
        name: &'static str,
        context: DirectiveContext,
    },
    #[error("directive `{name}` expects {expected} arguments, got {actual}")]
    InvalidArity {
        name: &'static str,
        expected: String,
        actual: usize,
    },
    #[error("invalid value for directive `{name}`: {detail}")]
    InvalidValue { name: &'static str, detail: String },
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
