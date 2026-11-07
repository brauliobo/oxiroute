#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const MAX_RECORDING_ROOT_BYTES: usize = 4_096;
pub(super) const MAX_SUFFIX_BYTES: usize = 128;
pub(super) const MAX_ROTATION_INTERVAL_MS: u64 = (1 << 31) - 1;
pub(super) const MAX_OUTBOUND_CHUNK_SIZE: u32 = 1_048_576;
pub(super) const DEFAULT_NGINX_MAX_MESSAGE_SIZE: u64 = 1_048_576;
pub(super) const MAX_INBOUND_MESSAGE_SIZE: u64 = 8 * 1_024 * 1_024;
pub(super) const DEFAULT_NGINX_ACK_WINDOW_SIZE: u32 = 5_000_000;
pub(super) const MAX_APPLICATION_CONNECTIONS: u64 = 100_000;
pub(super) const MAX_RECORDING_FILE_BYTES: u64 = 1_099_511_627_776;
pub(super) const MAX_RECORDING_FRAME_COUNT: u64 = 1_000_000_000;
const DEFAULT_HLS_SEGMENT_DURATION_MS: u64 = 2_000;
const DEFAULT_HLS_MAX_SEGMENT_DURATION_MS: u64 = 10_000;
const DEFAULT_HLS_PLAYLIST_LENGTH_MS: u64 = 30_000;
pub(super) const MAX_HLS_DURATION_MS: u64 = 120_000;
pub(super) const MAX_HLS_PLAYLIST_LENGTH_MS: u64 = 86_400_000;
pub(super) const MAX_EXEC_RESPAWN_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_EXEC_RESPAWN_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpResolution {
    pub rtmp_blocks: Vec<EffectiveRtmp>,
    /// One entry for every bounded expanded occurrence, in expansion order.
    pub decisions: Vec<OccurrenceDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmp {
    pub origin: DirectiveOrigin,
    pub outbound_chunk_size: u32,
    pub chunk_size_origin: Option<DirectiveOrigin>,
    pub max_inbound_message_size: u64,
    pub max_message_origin: Option<DirectiveOrigin>,
    pub ack_window_size: u32,
    pub ack_window_origin: Option<DirectiveOrigin>,
    pub auto_push: bool,
    pub auto_push_origin: Option<DirectiveOrigin>,
    pub auto_push_reconnect_ms: u64,
    pub auto_push_reconnect_origin: Option<DirectiveOrigin>,
    pub auto_push_socket_dir: PathBuf,
    pub auto_push_socket_origin: Option<DirectiveOrigin>,
    pub access_log_disabled: bool,
    pub access_log_path: Option<PathBuf>,
    pub access_log_origin: Option<DirectiveOrigin>,
    pub servers: Vec<EffectiveRtmpServer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpServer {
    pub origin: DirectiveOrigin,
    pub outbound_chunk_size: Option<u32>,
    pub chunk_size_origin: Option<DirectiveOrigin>,
    pub max_inbound_message_size: Option<u64>,
    pub max_message_origin: Option<DirectiveOrigin>,
    pub ack_window_size: Option<u32>,
    pub ack_window_origin: Option<DirectiveOrigin>,
    pub listens: Vec<EffectiveRtmpListen>,
    pub applications: Vec<EffectiveRtmpApplication>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpListen {
    pub origin: DirectiveOrigin,
    pub value: Option<NginxValue>,
    pub address: Option<SocketAddr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpApplication {
    pub origin: DirectiveOrigin,
    pub name: Option<NginxValue>,
    pub policy: EffectiveRtmpPolicy,
    pub push_targets: Vec<EffectiveRtmpPushTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpPushTarget {
    pub host: String,
    pub port: u16,
    pub application: String,
    pub origin: DirectiveOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpPolicy {
    pub live: bool,
    pub live_origin: Option<DirectiveOrigin>,
    pub idle_streams: bool,
    pub idle_streams_origin: Option<DirectiveOrigin>,
    pub publish_access: Vec<EffectiveRtmpAccessRule>,
    pub play_access: Vec<EffectiveRtmpAccessRule>,
    pub max_connections: Option<u64>,
    pub max_connections_origin: Option<DirectiveOrigin>,
    pub hls: Option<RtmpHlsPolicy>,
    pub exec_profiles: Vec<EffectiveRtmpExecProfile>,
    pub recorders: Vec<EffectiveRtmpRecorder>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpExecProfile {
    pub name: String,
    pub origin: DirectiveOrigin,
    pub mode: RtmpExecMode,
    pub trigger: RtmpExecTrigger,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub respawn: bool,
    pub respawn_origin: Option<DirectiveOrigin>,
    pub respawn_delay_ms: u64,
    pub respawn_delay_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpAccessRule {
    pub action: RtmpAclAction,
    pub network: String,
    pub origin: DirectiveOrigin,
    pub(super) operation: AccessOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessOperation {
    Publish,
    Play,
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct EffectiveRtmpRecorder {
    pub name: String,
    pub name_origin: DirectiveOrigin,
    pub mode: RtmpRecordMode,
    pub record_origin: DirectiveOrigin,
    pub record_mask: RtmpRecordMask,
    pub mask_origin: DirectiveOrigin,
    pub append: bool,
    pub append_origin: Option<DirectiveOrigin>,
    pub lock: bool,
    pub lock_origin: Option<DirectiveOrigin>,
    pub max_size: Option<u64>,
    pub max_size_origin: Option<DirectiveOrigin>,
    pub max_frames: Option<u64>,
    pub max_frames_origin: Option<DirectiveOrigin>,
    pub notify: bool,
    pub notify_origin: Option<DirectiveOrigin>,
    pub root_directory: PathBuf,
    pub path_origin: DirectiveOrigin,
    pub suffix_template: String,
    pub suffix_origin: Option<DirectiveOrigin>,
    pub append_unix_seconds: bool,
    pub unique_origin: Option<DirectiveOrigin>,
    pub rotation_interval_ms: Option<u64>,
    pub interval_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRecordMode {
    Continuous,
    Manual,
}

#[derive(Clone, Debug)]
pub(super) struct Policy {
    pub(super) live: Setting<bool>,
    pub(super) idle_streams: Setting<bool>,
    pub(super) publish_access: Vec<EffectiveRtmpAccessRule>,
    pub(super) play_access: Vec<EffectiveRtmpAccessRule>,
    pub(super) max_connections: Setting<Option<u64>>,
    pub(super) record: Setting<RecordSetting>,
    pub(super) record_mask: Setting<RtmpRecordMask>,
    pub(super) path: Setting<Option<PathBuf>>,
    pub(super) suffix: Setting<String>,
    pub(super) unique: Setting<bool>,
    pub(super) append: Setting<bool>,
    pub(super) lock: Setting<bool>,
    pub(super) max_size: Setting<Option<u64>>,
    pub(super) max_frames: Setting<Option<u64>>,
    pub(super) notify: Setting<bool>,
    pub(super) interval: Setting<Option<u64>>,
    pub(super) exec_profiles: Vec<EffectiveRtmpExecProfile>,
    pub(super) respawn: Setting<bool>,
    pub(super) respawn_timeout_ms: Setting<u64>,
    pub(super) auto_push: Setting<bool>,
    pub(super) auto_push_reconnect_ms: Setting<u64>,
    pub(super) auto_push_socket_dir: Setting<PathBuf>,
    pub(super) hls: HlsPolicy,
}

#[derive(Clone, Debug)]
pub(super) struct HlsPolicy {
    pub(super) enabled: Setting<bool>,
    pub(super) root_directory: Setting<Option<PathBuf>>,
    pub(super) segment_duration_ms: Setting<u64>,
    pub(super) max_segment_duration_ms: Setting<u64>,
    pub(super) playlist_length_ms: Setting<u64>,
    pub(super) fragment_naming: Setting<RtmpHlsFragmentNaming>,
    pub(super) nested: Setting<bool>,
    pub(super) cleanup: Setting<bool>,
    pub(super) keys: Setting<Option<RtmpHlsKeyPolicy>>,
}

#[derive(Clone, Debug)]
pub(super) struct Setting<T> {
    pub(super) value: T,
    pub(super) origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordSetting {
    Off,
    Continuous,
    Manual,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            live: Setting::new(false),
            idle_streams: Setting::new(true),
            publish_access: Vec::new(),
            play_access: Vec::new(),
            max_connections: Setting::new(None),
            record: Setting::new(RecordSetting::Off),
            record_mask: Setting::new(RtmpRecordMask::default()),
            path: Setting::new(None),
            suffix: Setting::new(".flv".into()),
            unique: Setting::new(false),
            append: Setting::new(false),
            lock: Setting::new(false),
            max_size: Setting::new(None),
            max_frames: Setting::new(None),
            notify: Setting::new(false),
            interval: Setting::new(None),
            exec_profiles: Vec::new(),
            respawn: Setting::new(true),
            respawn_timeout_ms: Setting::new(DEFAULT_EXEC_RESPAWN_TIMEOUT_MS),
            auto_push: Setting::new(false),
            auto_push_reconnect_ms: Setting::new(100),
            auto_push_socket_dir: Setting::new(PathBuf::from("/tmp")),
            hls: HlsPolicy::default(),
        }
    }
}

impl Default for HlsPolicy {
    fn default() -> Self {
        Self {
            enabled: Setting::new(false),
            root_directory: Setting::new(None),
            segment_duration_ms: Setting::new(DEFAULT_HLS_SEGMENT_DURATION_MS),
            max_segment_duration_ms: Setting::new(DEFAULT_HLS_MAX_SEGMENT_DURATION_MS),
            playlist_length_ms: Setting::new(DEFAULT_HLS_PLAYLIST_LENGTH_MS),
            fragment_naming: Setting::new(RtmpHlsFragmentNaming::Sequential),
            nested: Setting::new(false),
            cleanup: Setting::new(true),
            keys: Setting::new(None),
        }
    }
}

impl<T> Setting<T> {
    const fn new(value: T) -> Self {
        Self {
            value,
            origin: None,
        }
    }

    pub(super) fn replace(&mut self, value: T, origin: DirectiveOrigin) {
        self.value = value;
        self.origin = Some(origin);
    }
}
