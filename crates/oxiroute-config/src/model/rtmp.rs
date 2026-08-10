#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpService {
    pub name: String,
    #[serde(default = "default_rtmp_outbound_chunk_size")]
    pub outbound_chunk_size: u32,
    /// Largest assembled inbound RTMP message accepted by the session adapter.
    #[serde(default = "default_rtmp_max_inbound_message_size")]
    pub max_inbound_message_size: u64,
    /// Bytes between server acknowledgement windows announced to the peer.
    #[serde(default = "default_rtmp_ack_window_size")]
    pub ack_window_size: u32,
    #[serde(default)]
    pub access_log: Option<AccessLogPolicy>,
    #[serde(default = "default_rtmp_outbound_policy")]
    pub outbound_policy: RtmpOutboundPolicy,
    #[serde(default)]
    pub callbacks: RtmpCallbackConfig,
    #[serde(default = "default_rtmp_auto_push_policy")]
    pub auto_push: RtmpAutoPushPolicy,
    #[serde(default)]
    pub exec_profiles: Vec<RtmpExecProfile>,
    pub applications: Vec<RtmpApplication>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpAutoPushPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_rtmp_auto_push_socket_dir")]
    pub socket_dir: PathBuf,
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
    #[serde(default = "default_rtmp_auto_push_reconnect_ms")]
    pub reconnect_ms: u64,
    #[serde(default = "default_rtmp_auto_push_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_rtmp_auto_push_handshake_timeout_ms")]
    pub handshake_timeout_ms: u64,
    #[serde(default = "default_rtmp_auto_push_max_peers")]
    pub max_peers: u64,
    #[serde(default = "default_rtmp_auto_push_max_queue_messages")]
    pub max_queue_messages: u64,
    #[serde(default = "default_rtmp_auto_push_max_queue_bytes")]
    pub max_queue_bytes: u64,
    #[serde(default = "default_rtmp_auto_push_max_streams")]
    pub max_streams: u64,
}

fn default_rtmp_auto_push_socket_dir() -> PathBuf {
    "/tmp/oxiroute-rtmp".into()
}

impl Default for RtmpAutoPushPolicy {
    fn default() -> Self {
        default_rtmp_auto_push_policy()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpExecProfile {
    pub name: String,
    pub application: String,
    #[serde(default)]
    pub mode: RtmpExecMode,
    #[serde(default)]
    pub trigger: RtmpExecTrigger,
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: Vec<RtmpExecEnvironment>,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub filesystem: RtmpExecFilesystemPolicy,
    #[serde(default)]
    pub network: RtmpExecNetworkPolicy,
    #[serde(default = "default_rtmp_exec_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_rtmp_exec_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
    #[serde(default = "default_rtmp_exec_max_processes")]
    pub max_processes: u64,
    #[serde(default = "default_rtmp_exec_max_queue_messages")]
    pub max_queue_messages: u64,
    #[serde(default = "default_rtmp_exec_max_queue_bytes")]
    pub max_queue_bytes: u64,
    #[serde(default = "default_rtmp_exec_max_stdout_bytes")]
    pub max_stdout_bytes: u64,
    #[serde(default = "default_rtmp_exec_max_stderr_bytes")]
    pub max_stderr_bytes: u64,
    #[serde(default)]
    pub respawn: bool,
    #[serde(default = "default_rtmp_exec_respawn_delay_ms")]
    pub respawn_delay_ms: u64,
    #[serde(default = "default_rtmp_exec_max_respawns")]
    pub max_respawns: u64,
}

impl fmt::Debug for RtmpExecProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpExecProfile")
            .field("name", &self.name)
            .field("application", &self.application)
            .field("mode", &self.mode)
            .field("trigger", &self.trigger)
            .field("executable", &"<redacted>")
            .field(
                "arguments",
                &format_args!("<{} redacted>", self.arguments.len()),
            )
            .field(
                "environment",
                &format_args!("<{} redacted>", self.environment.len()),
            )
            .field("working_directory", &"<redacted>")
            .field("filesystem", &self.filesystem)
            .field("network", &self.network)
            .field("timeout_ms", &self.timeout_ms)
            .field("shutdown_timeout_ms", &self.shutdown_timeout_ms)
            .field("max_processes", &self.max_processes)
            .field("max_queue_messages", &self.max_queue_messages)
            .field("max_queue_bytes", &self.max_queue_bytes)
            .field("max_stdout_bytes", &self.max_stdout_bytes)
            .field("max_stderr_bytes", &self.max_stderr_bytes)
            .field("respawn", &self.respawn)
            .field("respawn_delay_ms", &self.respawn_delay_ms)
            .field("max_respawns", &self.max_respawns)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpExecEnvironment {
    pub name: String,
    pub value: String,
}

impl fmt::Debug for RtmpExecEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpExecEnvironment")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpExecMode {
    #[default]
    Command,
    Transcode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpExecTrigger {
    #[default]
    Publisher,
    PublishDone,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpExecFilesystemPolicy {
    #[default]
    WorkingDirectory,
    Host,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpExecNetworkPolicy {
    #[default]
    Disabled,
    Inherited,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpApplication {
    pub name: String,
    #[serde(default)]
    pub live: bool,
    #[serde(default = "default_true")]
    pub idle_streams: bool,
    #[serde(default)]
    pub publish: RtmpAccessPolicy,
    #[serde(default)]
    pub play: RtmpAccessPolicy,
    #[serde(default = "default_rtmp_session_ceilings")]
    pub limits: RtmpSessionCeilings,
    #[serde(default)]
    pub push_targets: Vec<RtmpPushTarget>,
    #[serde(default)]
    pub pull_targets: Vec<RtmpPullTarget>,
    #[serde(default = "default_rtmp_relay_policy")]
    pub relay: RtmpRelayPolicy,
    #[serde(default)]
    pub callbacks: RtmpCallbackConfig,
    #[serde(default = "default_rtmp_fanout_policy")]
    pub fanout: RtmpFanoutPolicy,
    #[serde(default)]
    pub vod: Option<RtmpVodPolicy>,
    #[serde(default)]
    pub hls: Option<RtmpHlsPolicy>,
    #[serde(default)]
    pub dash: Option<RtmpDashPolicy>,
    #[serde(default)]
    pub recorders: Vec<RtmpRecorder>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpAccessPolicy {
    #[serde(default)]
    pub rules: Vec<RtmpAccessRule>,
    #[serde(default)]
    pub token: Option<RtmpTokenPolicy>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpAccessRule {
    pub action: RtmpAclAction,
    pub network: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RtmpAclAction {
    Allow,
    Deny,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpTokenPolicy {
    pub source: RtmpTokenSource,
    pub parameter: String,
    pub secret: String,
}

impl fmt::Debug for RtmpTokenPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpTokenPolicy")
            .field("source", &self.source)
            .field("parameter", &self.parameter)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpTokenSource {
    StreamQuery,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpOutboundPolicy {
    #[serde(default)]
    pub allow_domains: Vec<String>,
    #[serde(default)]
    pub deny_domains: Vec<String>,
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    #[serde(default)]
    pub deny_cidrs: Vec<String>,
    #[serde(default = "default_true")]
    pub deny_private: bool,
    #[serde(default)]
    pub rtmps: RtmpRtmpsPolicy,
    #[serde(default = "default_rtmp_max_chain_depth")]
    pub max_chain_depth: u8,
}

impl Default for RtmpOutboundPolicy {
    fn default() -> Self {
        default_rtmp_outbound_policy()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpRtmpsPolicy {
    #[default]
    Disabled,
    Allowed,
    Required,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpRelayPolicy {
    #[serde(default = "default_rtmp_relay_queue_messages")]
    pub max_queue_messages: u64,
    #[serde(default = "default_rtmp_relay_queue_bytes")]
    pub max_queue_bytes: u64,
    #[serde(default = "default_rtmp_relay_buffer_ms")]
    pub buffer_ms: u64,
    #[serde(default = "default_rtmp_push_reconnect_ms")]
    pub push_reconnect_ms: u64,
    #[serde(default = "default_rtmp_pull_reconnect_ms")]
    pub pull_reconnect_ms: u64,
    #[serde(default = "default_rtmp_relay_dns_refresh_ms")]
    pub dns_refresh_ms: u64,
    #[serde(default = "default_rtmp_relay_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_rtmp_relay_handshake_timeout_ms")]
    pub handshake_timeout_ms: u64,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpCallbackConfig {
    #[serde(default)]
    pub on_connect: Option<String>,
    #[serde(default)]
    pub on_disconnect: Option<String>,
    #[serde(default)]
    pub on_publish: Option<String>,
    #[serde(default)]
    pub on_publish_done: Option<String>,
    #[serde(default)]
    pub on_play: Option<String>,
    #[serde(default)]
    pub on_play_done: Option<String>,
    #[serde(default)]
    pub on_done: Option<String>,
    #[serde(default)]
    pub on_update: Option<String>,
    #[serde(default)]
    pub notify_method: RtmpNotifyMethod,
    #[serde(default = "default_rtmp_callback_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_rtmp_callback_update_timeout_ms")]
    pub notify_update_timeout_ms: u64,
    #[serde(default)]
    pub notify_update_strict: bool,
    #[serde(default)]
    pub notify_relay_redirect: bool,
}

impl fmt::Debug for RtmpCallbackConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("RtmpCallbackConfig");
        for (name, value) in [
            ("on_connect", self.on_connect.as_ref()),
            ("on_disconnect", self.on_disconnect.as_ref()),
            ("on_publish", self.on_publish.as_ref()),
            ("on_publish_done", self.on_publish_done.as_ref()),
            ("on_play", self.on_play.as_ref()),
            ("on_play_done", self.on_play_done.as_ref()),
            ("on_done", self.on_done.as_ref()),
            ("on_update", self.on_update.as_ref()),
        ] {
            debug.field(name, &value.map(|_| "<redacted>"));
        }
        debug
            .field("notify_method", &self.notify_method)
            .field("timeout_ms", &self.timeout_ms)
            .field("notify_update_timeout_ms", &self.notify_update_timeout_ms)
            .field("notify_update_strict", &self.notify_update_strict)
            .field("notify_relay_redirect", &self.notify_relay_redirect)
            .finish()
    }
}

impl Default for RtmpCallbackConfig {
    fn default() -> Self {
        Self {
            on_connect: None,
            on_disconnect: None,
            on_publish: None,
            on_publish_done: None,
            on_play: None,
            on_play_done: None,
            on_done: None,
            on_update: None,
            notify_method: RtmpNotifyMethod::default(),
            timeout_ms: default_rtmp_callback_timeout_ms(),
            notify_update_timeout_ms: default_rtmp_callback_update_timeout_ms(),
            notify_update_strict: false,
            notify_relay_redirect: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RtmpNotifyMethod {
    Get,
    #[default]
    Post,
}

impl Default for RtmpRelayPolicy {
    fn default() -> Self {
        default_rtmp_relay_policy()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RtmpSessionCeilings {
    pub max_connections: u64,
    pub max_publishers: u64,
    pub max_viewers: u64,
}

impl Default for RtmpSessionCeilings {
    fn default() -> Self {
        default_rtmp_session_ceilings()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpPushTarget {
    pub host: String,
    #[serde(default = "default_rtmp_port")]
    pub port: u16,
    pub application: String,
    #[serde(default)]
    pub scheme: RtmpTransport,
    #[serde(default)]
    pub stream_name: Option<String>,
    #[serde(default)]
    pub tc_url: Option<String>,
    #[serde(default)]
    pub flash_version: Option<String>,
    #[serde(default)]
    pub credentials: Option<RtmpCredentialReference>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpPullTarget {
    pub host: String,
    #[serde(default = "default_rtmp_port")]
    pub port: u16,
    pub application: String,
    pub stream_name: String,
    #[serde(default)]
    pub scheme: RtmpTransport,
    #[serde(default)]
    pub tc_url: Option<String>,
    #[serde(default)]
    pub flash_version: Option<String>,
    #[serde(default)]
    pub credentials: Option<RtmpCredentialReference>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RtmpTransport {
    #[default]
    Rtmp,
    Rtmps,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpCredentialReference {
    pub username: String,
    pub secret_file: PathBuf,
}

impl fmt::Debug for RtmpCredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpCredentialReference")
            .field("username", &self.username)
            .field("secret_file", &"<redacted>")
            .finish()
    }
}

const fn default_rtmp_port() -> u16 {
    1_935
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpFanoutPolicy {
    pub max_subscribers: u64,
    pub max_queue_messages_per_subscriber: u64,
    pub max_queue_bytes_per_subscriber: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpVodPolicy {
    #[serde(default)]
    pub sources: Vec<RtmpVodSource>,
    #[serde(default = "default_rtmp_vod_sessions")]
    pub max_sessions: u64,
    #[serde(default = "default_rtmp_vod_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default = "default_rtmp_vod_duration_ms")]
    pub max_duration_ms: u64,
}

impl Default for RtmpVodPolicy {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            max_sessions: default_rtmp_vod_sessions(),
            max_file_bytes: default_rtmp_vod_file_bytes(),
            max_duration_ms: default_rtmp_vod_duration_ms(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RtmpVodSource {
    Local {
        name: String,
        root_directory: PathBuf,
    },
    Http {
        name: String,
        origin: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpHlsPolicy {
    pub root_directory: PathBuf,
    #[serde(default = "default_rtmp_hls_segment_duration_ms")]
    pub segment_duration_ms: u64,
    #[serde(default = "default_rtmp_hls_max_segment_duration_ms")]
    pub max_segment_duration_ms: u64,
    #[serde(default = "default_rtmp_hls_playlist_length_ms")]
    pub playlist_length_ms: u64,
    #[serde(default)]
    pub fragment_naming: RtmpHlsFragmentNaming,
    #[serde(default)]
    pub nested: bool,
    #[serde(default = "default_true")]
    pub cleanup: bool,
    #[serde(default)]
    pub variants: Vec<RtmpHlsVariant>,
    #[serde(default)]
    pub keys: Option<RtmpHlsKeyPolicy>,
    #[serde(default = "default_rtmp_hls_max_segment_bytes")]
    pub max_segment_bytes: u64,
    #[serde(default = "default_rtmp_hls_max_queue_messages")]
    pub max_queue_messages: u64,
    #[serde(default = "default_rtmp_hls_max_storage_bytes")]
    pub max_storage_bytes: u64,
    #[serde(default = "default_rtmp_hls_max_storage_files")]
    pub max_storage_files: u64,
    #[serde(default = "default_rtmp_hls_max_active_streams")]
    pub max_active_streams: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpHlsFragmentNaming {
    #[default]
    Sequential,
    Timestamp,
    System,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpHlsVariant {
    pub name: String,
    pub bandwidth: u64,
    #[serde(default)]
    pub codecs: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpHlsKeyPolicy {
    #[serde(default = "default_rtmp_hls_key_rotation_segments")]
    pub rotation_segments: u64,
    #[serde(default)]
    pub url_prefix: String,
}

fn default_rtmp_hls_key_rotation_segments() -> u64 {
    5
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpDashPolicy {
    pub root_directory: PathBuf,
    #[serde(default = "default_rtmp_dash_segment_duration_ms")]
    pub segment_duration_ms: u64,
    #[serde(default = "default_rtmp_dash_max_segment_duration_ms")]
    pub max_segment_duration_ms: u64,
    #[serde(default = "default_rtmp_hls_playlist_length_ms")]
    pub playlist_length_ms: u64,
    #[serde(default)]
    pub segment_naming: RtmpDashSegmentNaming,
    #[serde(default)]
    pub nested: bool,
    #[serde(default = "default_true")]
    pub cleanup: bool,
    #[serde(default = "default_rtmp_dash_max_segment_bytes")]
    pub max_segment_bytes: u64,
    #[serde(default = "default_rtmp_dash_max_queue_messages")]
    pub max_queue_messages: u64,
    #[serde(default = "default_rtmp_dash_max_storage_bytes")]
    pub max_storage_bytes: u64,
    #[serde(default = "default_rtmp_dash_max_storage_files")]
    pub max_storage_files: u64,
    #[serde(default = "default_rtmp_dash_max_active_streams")]
    pub max_active_streams: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpDashSegmentNaming {
    #[default]
    Sequential,
    Timestamp,
    System,
}

impl Default for RtmpFanoutPolicy {
    fn default() -> Self {
        default_rtmp_fanout_policy()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct RtmpRecorder {
    pub name: String,
    /// Omitted policies start recording continuously.
    #[serde(default)]
    pub start: RtmpRecorderStart,
    pub root_directory: PathBuf,
    /// Defaults to audio and video, without keyframe-only filtering.
    #[serde(default)]
    pub record_mask: RtmpRecordMask,
    /// Defaults to `.flv` and accepts only the bounded UTC subset used by `RecordingPathPolicy`.
    #[serde(default = "default_recorder_suffix_template")]
    pub suffix_template: String,
    /// Defaults to false.
    #[serde(default)]
    pub append_unix_seconds: bool,
    /// Resume the exact existing segment when it is a valid FLV stream.
    #[serde(default)]
    pub append: bool,
    /// Hold an exclusive advisory lock on the active recording file.
    #[serde(default)]
    pub lock: bool,
    /// Maximum bytes for one published recording. Null means unlimited.
    #[serde(default)]
    pub max_size: Option<u64>,
    /// Maximum audio/video frames for one recording. Null means unlimited.
    #[serde(default)]
    pub max_frames: Option<u64>,
    /// Retain bounded start/stop/failure notifications in recorder status.
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub timezone: RtmpRecorderTimezone,
    #[serde(default)]
    pub time_basis: RtmpRecorderTimeBasis,
    #[serde(default)]
    pub segment_naming: RtmpRecorderSegmentNaming,
    /// Defaults to null (no rotation).
    #[serde(default)]
    pub rotation_interval_ms: Option<u64>,
    /// Defaults to the recorder worker's 256-message queue bound.
    #[serde(default = "default_recorder_max_queue_messages")]
    pub max_queue_messages: u64,
    /// Defaults to the recorder worker's 8 MiB queue byte bound.
    #[serde(default = "default_recorder_max_queue_bytes")]
    pub max_queue_bytes: u64,
    /// Defaults to the recorder worker's 5-second shutdown timeout.
    #[serde(default = "default_recorder_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
    /// Omitted or explicit null means no byte quota for the normalized root directory.
    #[serde(default)]
    pub max_storage_bytes: Option<u64>,
    /// Omitted or explicit null means no file-count quota for the normalized root directory.
    #[serde(default)]
    pub max_storage_files: Option<u64>,
    /// Defaults to 8 active recorders per normalized root directory.
    #[serde(default = "default_recorder_max_active_recorders")]
    pub max_active_recorders: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RtmpRecorderTimezone {
    #[default]
    Utc,
    Iana(String),
}

impl Serialize for RtmpRecorderTimezone {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Utc => "utc",
            Self::Iana(name) => name,
        })
    }
}

impl<'de> Deserialize<'de> for RtmpRecorderTimezone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Ok(if name.eq_ignore_ascii_case("utc") {
            Self::Utc
        } else {
            Self::Iana(name)
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpRecorderTimeBasis {
    #[default]
    SegmentStart,
    SegmentEnd,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpRecorderSegmentNaming {
    #[default]
    SafeUnique,
    NginxCompatible,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpRecordMask {
    /// Include AAC and other audio tags in the recording.
    #[serde(default = "default_true")]
    pub audio: bool,
    /// Include AVC video tags in the recording.
    #[serde(default = "default_true")]
    pub video: bool,
    /// When video is enabled, retain keyframes but omit interframes.
    #[serde(default)]
    pub keyframes: bool,
}

impl Default for RtmpRecordMask {
    fn default() -> Self {
        Self {
            audio: true,
            video: true,
            keyframes: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtmpRecorderStart {
    #[default]
    Continuous,
    Manual,
}
