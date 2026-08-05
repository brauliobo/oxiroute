pub const RTMP_VERSION: u8 = 3;
pub const HANDSHAKE_BLOCK_SIZE: usize = 1536;

mod callback;
mod catalog;
mod client;
mod dash_segmenter;
mod directives;
mod flv;
mod live;
mod media_segmenter;
mod media_storage;
mod nginx;
mod recording_path;
mod recording_runtime;
mod recording_store;
mod recording_worker;
mod relay;
mod session;
mod vod;

pub use callback::{
    RtmpCallbackContext, RtmpCallbackEndpoint, RtmpCallbackError, RtmpCallbackEvent,
    RtmpCallbackMethod, RtmpCallbackPolicy,
};
pub use dash_segmenter::{DashOutputConfig, DashSegmentNaming};
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
pub use directives::{directive_compatibility_report, directive_specs, validate_directive};
pub use flv::{
    FlvMuxer, FlvMuxerError, FlvTagType, MAX_CACHED_CODEC_HEADER_SIZE, MAX_FLV_TAG_DATA_SIZE,
};
pub use live::{
    LiveHub, LiveHubError, LiveHubLimits, LiveHubStats, MediaEvent, MediaEventError,
    MediaEventKind, PlaybackSubscription, PublishReport, PublisherIncarnation, PublisherLease,
    VideoCodec, VideoCodecIdentifier,
};
pub use media_segmenter::{
    HlsFragmentNaming, HlsKeyConfig, HlsOutputConfig, HlsVariant, MediaApplication, MediaCatalog,
    MediaEnqueueResult, MediaObject, MediaOutputError, MediaPublisher,
};
pub use media_storage::{
    MAX_MEDIA_PATH_BYTES, MediaStore, MediaStoreError, MediaStoreLimits, MediaStoreStats,
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
    RecorderEnqueueResult, RecorderFailure, RecorderMediaMask, RecorderNotification,
    RecorderShutdown, RecorderVideoCodec, RecorderWorker, RecorderWorkerConfig,
    RecorderWorkerPhase, RecorderWorkerStartError, RecorderWorkerStatus, RecorderWorkerSupervisor,
};
pub use relay::{
    RtmpDestination, RtmpPullTarget, RtmpPushApplication, RtmpPushTarget, RtmpRelayConfig,
    RtmpRelayFailure, RtmpRelayPhase, RtmpRelayStatus,
};
pub use rml_rtmp::sessions::StreamMetadata;
pub use session::{
    MAX_INBOUND_AMF0_CONTAINER_ENTRIES, MAX_INBOUND_AMF0_DEPTH, MAX_INBOUND_AMF0_STRING_BYTES,
    MAX_INBOUND_AMF0_VALUES, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE,
    MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpAccessAction,
    RtmpAccessPolicy, RtmpAccessRule, RtmpApplication, RtmpNetwork, RtmpRecorderLifecycle,
    RtmpServiceRuntime, RtmpSession, RtmpSessionCeilings, RtmpSessionError, RtmpSessionLimits,
    RtmpSessionPolicy, RtmpTokenPolicy,
};
pub use vod::{
    MAX_VOD_EVENTS, MAX_VOD_HTTP_HEADER_BYTES, MAX_VOD_ORIGIN_BYTES, MAX_VOD_PATH_BYTES,
    MAX_VOD_REDIRECTS, MAX_VOD_SOURCE_NAME_BYTES, VodApplication, VodCatalog, VodError, VodLimits,
    VodLease, VodObject, VodRange, VodSourceDefinition,
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

/// Runtime status for a directive key or one of its explicitly described forms.
///
/// `runtime_support` on [`DirectiveSpec`] remains the coarse, key-level status
/// used by existing consumers. The form-level status is authoritative whenever
/// a directive has entries in [`DirectiveSpec::runtime_forms`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectiveStatus {
    Enforced,
    Partial,
    DisableOnly,
    ParsedOnly,
    SourceNoOp,
    SourceBug,
    Deprecated,
    PlatformLimited,
}

impl DirectiveStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::Partial => "partial",
            Self::DisableOnly => "disable_only",
            Self::ParsedOnly => "parsed_only",
            Self::SourceNoOp => "source_no_op",
            Self::SourceBug => "source_bug",
            Self::Deprecated => "deprecated",
            Self::PlatformLimited => "platform_limited",
        }
    }
}

/// Runtime status for one named normalized form of a directive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveForm {
    pub form: &'static str,
    pub contexts: &'static [DirectiveContext],
    pub status: DirectiveStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DirectiveStatusCounts {
    pub enforced: usize,
    pub partial: usize,
    pub disable_only: usize,
    pub parsed_only: usize,
    pub source_no_op: usize,
    pub source_bug: usize,
    pub deprecated: usize,
    pub platform_limited: usize,
}

impl DirectiveStatusCounts {
    #[must_use]
    pub const fn total(self) -> usize {
        self.enforced
            + self.partial
            + self.disable_only
            + self.parsed_only
            + self.source_no_op
            + self.source_bug
            + self.deprecated
            + self.platform_limited
    }
}

/// Generated status counts and registry entries for the RTMP compatibility report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveCompatibilityReport {
    pub entries: &'static [DirectiveSpec],
    pub directive_status: DirectiveStatusCounts,
    pub form_status: DirectiveStatusCounts,
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
    pub runtime_forms: &'static [DirectiveForm],
}

impl DirectiveSpec {
    #[must_use]
    pub fn compatibility_status(&self) -> DirectiveStatus {
        let Some(first) = self.runtime_forms.first() else {
            return match self.runtime_support {
                RuntimeSupport::ParsedNotEnforced => DirectiveStatus::ParsedOnly,
                RuntimeSupport::SourceNoOp => DirectiveStatus::SourceNoOp,
                RuntimeSupport::SourceBug => DirectiveStatus::SourceBug,
                RuntimeSupport::Deprecated => DirectiveStatus::Deprecated,
                RuntimeSupport::PlatformLimited => DirectiveStatus::PlatformLimited,
            };
        };

        if self
            .runtime_forms
            .iter()
            .all(|form| form.status == first.status)
        {
            first.status
        } else {
            DirectiveStatus::Partial
        }
    }

    #[must_use]
    pub fn runtime_form(&self, form: &str) -> Option<&DirectiveForm> {
        self.runtime_forms
            .iter()
            .find(|runtime_form| runtime_form.form == form)
    }
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
