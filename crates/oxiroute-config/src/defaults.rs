use crate::model::{
    AlpnProtocol, CacheKeyComponent, ForwardHttpVersion, HttpRetryTrigger, HttpRoutePolicy,
    RtmpFanoutPolicy, RtmpOutboundPolicy, RtmpRelayPolicy, RtmpRtmpsPolicy, RtmpSessionCeilings,
};

pub(crate) const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_LUA_MEMORY_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_LUA_INSTRUCTIONS: u32 = 1_000_000;
pub(crate) const INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

const DEFAULT_UPSTREAM_IO_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_REQUEST_BODY_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_HEALTH_INTERVAL_MS: u64 = 10_000;
const DEFAULT_HEALTH_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_HEALTHY_THRESHOLD: u16 = 1;
const DEFAULT_UNHEALTHY_THRESHOLD: u16 = 3;
const DEFAULT_RTMP_OUTBOUND_CHUNK_SIZE: u32 = 4_096;
const DEFAULT_RTMP_MAX_SUBSCRIBERS: u64 = 1_024;
const DEFAULT_RTMP_FANOUT_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_RTMP_FANOUT_QUEUE_BYTES: u64 = 8 * 1_024 * 1_024;
const DEFAULT_RTMP_RELAY_BUFFER_MS: u64 = 5_000;
const DEFAULT_RTMP_PUSH_RECONNECT_MS: u64 = 3_000;
const DEFAULT_RTMP_PULL_RECONNECT_MS: u64 = 3_000;
const DEFAULT_RTMP_RELAY_CONNECT_TIMEOUT_MS: u64 = 500;
const DEFAULT_RTMP_RELAY_HANDSHAKE_TIMEOUT_MS: u64 = 2_000;
const DEFAULT_RTMP_MAX_CHAIN_DEPTH: u8 = 4;
const DEFAULT_RTMP_HLS_SEGMENT_DURATION_MS: u64 = 2_000;
const DEFAULT_RTMP_HLS_MAX_SEGMENT_DURATION_MS: u64 = 10_000;
const DEFAULT_RTMP_HLS_PLAYLIST_LENGTH_MS: u64 = 30_000;
const DEFAULT_RTMP_HLS_MAX_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_RTMP_HLS_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_RTMP_HLS_MAX_STORAGE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_RTMP_HLS_MAX_STORAGE_FILES: u64 = 10_000;
const DEFAULT_RTMP_HLS_MAX_ACTIVE_STREAMS: u64 = 1_024;
const DEFAULT_RTMP_DASH_SEGMENT_DURATION_MS: u64 = 5_000;
const DEFAULT_RTMP_DASH_MAX_SEGMENT_DURATION_MS: u64 = 15_000;
const DEFAULT_RTMP_DASH_MAX_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_RTMP_DASH_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_RTMP_DASH_MAX_STORAGE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_RTMP_DASH_MAX_STORAGE_FILES: u64 = 10_000;
const DEFAULT_RTMP_DASH_MAX_ACTIVE_STREAMS: u64 = 1_024;
const DEFAULT_RECORDER_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_RECORDER_MAX_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_RECORDER_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_RECORDER_MAX_ACTIVE_RECORDERS: u64 = 8;
const DEFAULT_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_DISK_CACHE_MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
const DEFAULT_CACHE_MAX_ENTRIES: u64 = 100_000;
const DEFAULT_DISK_CACHE_MAX_FILES: u64 = 1_000_000;
const DEFAULT_CACHE_MAX_OBJECT_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_CACHE_MAX_HEADER_BYTES: u64 = 64 * 1024;
const DEFAULT_CACHE_MAX_KEY_BYTES: u64 = 4 * 1024;
const DEFAULT_CACHE_MAX_TAG_BYTES: u64 = 256;
const DEFAULT_CACHE_MAX_TAGS_PER_OBJECT: u64 = 64;
const DEFAULT_CACHE_MAX_IN_FLIGHT_FILLS: u64 = 1_024;
const DEFAULT_CACHE_MAX_FOLLOWERS_PER_FILL: u64 = 128;
const DEFAULT_CACHE_TTL_MS: u64 = 60_000;
const DEFAULT_CACHE_GRACE_MS: u64 = 30_000;
const DEFAULT_CACHE_KEEP_MS: u64 = 300_000;
const DEFAULT_FORWARD_LIFETIME_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_FORWARD_MAX_CONNECTIONS: u64 = 10_000;
const DEFAULT_FORWARD_MAX_HEADER_BYTES: u64 = 64 * 1024;
const DEFAULT_FORWARD_RESOLVER_CACHE_ENTRIES: u64 = 4_096;
const DEFAULT_FORWARD_RESOLVER_CONCURRENT_QUERIES: u64 = 256;
const DEFAULT_FORWARD_RESOLVER_MAX_ADDRESSES: u64 = 16;
const DEFAULT_FORWARD_RESOLVER_MIN_TTL_MS: u64 = 1_000;
const DEFAULT_FORWARD_RESOLVER_MAX_TTL_MS: u64 = 300_000;
const DEFAULT_ACME_RETAINED_REVISIONS: u32 = 3;
const DEFAULT_ACME_RETENTION_DAYS: u32 = 30;
const DEFAULT_FORWARD_RESOLVER_NEGATIVE_TTL_MS: u64 = 30_000;
const DEFAULT_UDP_MAX_DATAGRAM_BYTES: u64 = 16 * 1024;
const DEFAULT_UDP_MAX_SESSIONS: u64 = 4_096;
const DEFAULT_UDP_MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_UDP_MAX_QUEUE_DATAGRAMS: u64 = 64;
const DEFAULT_UDP_MAX_QUEUE_BYTES: u64 = 1024 * 1024;
const DEFAULT_ACME_DNS01_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_PROXY_PROTOCOL_TIMEOUT_MS: u64 = 5_000;

pub(crate) const MIN_HEALTH_INTERVAL_MS: u64 = 1_000;
pub(crate) const MAX_HEALTH_INTERVAL_MS: u64 = 86_400_000;
pub(crate) const MAX_HEALTH_TIMEOUT_MS: u64 = 30_000;
pub(crate) const MAX_HEALTH_THRESHOLD: u16 = 100;
pub(crate) const MAX_HEALTH_HOST_BYTES: usize = 255;
pub(crate) const MAX_HEALTH_PATH_BYTES: usize = 2_048;
pub(crate) const MAX_ACME_RETAINED_REVISIONS: u32 = 32;
pub(crate) const MAX_ACME_RETENTION_DAYS: u32 = 3_650;
pub(crate) const MAX_CERTIFICATES: usize = 256;
pub(crate) const MAX_CERTIFICATE_DNS_NAMES: usize = 100;
pub(crate) const MAX_ACME_CONTACTS: usize = 8;
pub(crate) const MAX_ACME_DNS_SUFFIXES: usize = 16;
pub(crate) const MAX_ACME_DIRECTORY_URL_BYTES: usize = 2_048;
pub(crate) const MAX_ACME_DNS01_PROVIDER_BYTES: usize = 64;
pub(crate) const MAX_ACME_DNS01_TIMEOUT_SECONDS: u64 = 600;
pub(crate) const MIN_SELF_SIGNED_VALIDITY_DAYS: u32 = 1;
pub(crate) const MAX_SELF_SIGNED_VALIDITY_DAYS: u32 = 30;
pub(crate) const MAX_TLS_PROFILES: usize = 256;
pub(crate) const MAX_FILE_PATH_BYTES: usize = 4_096;
pub(crate) const MAX_SERVER_NAME_BYTES: usize = 253;
pub(crate) const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;
pub(crate) const MAX_ENDPOINTS_PER_POOL: usize = 256;
pub(crate) const MAX_TOTAL_ENDPOINTS: usize = 1_024;
pub(crate) const MAX_UPSTREAM_WEIGHT: u16 = 100;
pub(crate) const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
pub(crate) const MAX_HTTP_RETRIES: u8 = 3;
pub(crate) const MAX_HTTP_METHODS_PER_ROUTE: usize = 16;
pub(crate) const MAX_HTTP_METHOD_BYTES: usize = 32;
pub(crate) const MAX_HTTP_AUTHORITY_BYTES: usize = 255;
pub(crate) const MAX_HTTP_HEADER_MUTATIONS: usize = 32;
pub(crate) const MAX_HTTP_LITERAL_HEADERS: usize = 32;
pub(crate) const MAX_HTTP_HEADER_NAME_BYTES: usize = 64;
pub(crate) const MAX_HTTP_HEADER_VALUE_BYTES: usize = 8 * 1024;
pub(crate) const MAX_HTTP_COOKIE_PATH_REWRITES: usize = 16;
pub(crate) const MAX_HTTP_COOKIE_PATH_BYTES: usize = 1024;
pub(crate) const MAX_HTTP_PROXY_PATH_BYTES: usize = 1024;
pub(crate) const MAX_HTTP_FIXED_RESPONSE_BODY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_HTTP_REDIRECT_LOCATION_BYTES: usize = 2048;
pub(crate) const MAX_STATS_PAGE_URI_BYTES: usize = 2048;
pub(crate) const MAX_STATS_PAGE_REFRESH_MS: u64 = 86_400_000;
pub(crate) const MAX_HTTP_ACCESS_REALM_BYTES: usize = 128;
pub(crate) const MAX_HTTP_STATIC_INDEX_FILES: usize = 8;
pub(crate) const MAX_HTTP_STATIC_INDEX_BYTES: usize = 255;
pub(crate) const MAX_HTTP_STATIC_FALLBACK_BYTES: usize = 1024;
pub(crate) const MAX_HTTP_STATIC_TRY_FILES: usize = 16;
pub(crate) const MAX_HTTP_STATIC_MIME_TYPES: usize = 2_048;
pub(crate) const MAX_HTTP_STATIC_ERROR_RESPONSES: usize = 16;
pub(crate) const MAX_HTTP_STATIC_ERROR_STATUSES: usize = 16;
pub(crate) const MAX_HTTP_MIME_TYPE_BYTES: usize = 128;
pub(crate) const MAX_HTTP_FILE_EXTENSION_BYTES: usize = 32;
pub(crate) const MAX_HTTP_COOKIE_ATTRIBUTE_RULES: usize = 16;
pub(crate) const MAX_HTTP_GZIP_TYPES: usize = 64;
pub(crate) const MAX_HTTP_TIMEOUT_MS: u64 = 86_400_000;
pub(crate) const MAX_PROXY_PROTOCOL_TIMEOUT_MS: u64 = 86_400_000;
pub(crate) const MAX_UDP_DATAGRAM_BYTES: u64 = 65_507;
pub(crate) const MAX_UDP_SESSIONS: u64 = 100_000;
pub(crate) const MAX_UDP_SESSION_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_UDP_QUEUE_DATAGRAMS: u64 = 4_096;
pub(crate) const MAX_UDP_QUEUE_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_RTMP_SERVICES: usize = 64;
pub(crate) const MAX_RTMP_APPLICATIONS_PER_SERVICE: usize = 256;
pub(crate) const MAX_RTMP_RECORDERS_PER_APPLICATION: usize = 8;
pub(crate) const MAX_TOTAL_RTMP_RECORDERS: usize = 256;
pub(crate) const MAX_RTMP_RECORDING_ROOTS: usize = 64;
pub(crate) const MAX_RTMP_HLS_OUTPUTS: usize = 64;
pub(crate) const MAX_RTMP_HLS_VARIANTS: usize = 16;
pub(crate) const MAX_RTMP_HLS_NAME_BYTES: usize = 128;
pub(crate) const MAX_RTMP_HLS_SEGMENT_DURATION_MS: u64 = 120_000;
pub(crate) const MAX_RTMP_HLS_PLAYLIST_LENGTH_MS: u64 = 86_400_000;
pub(crate) const MAX_RTMP_HLS_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_RTMP_HLS_QUEUE_MESSAGES: u64 = 65_536;
pub(crate) const MAX_RTMP_HLS_STORAGE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub(crate) const MAX_RTMP_HLS_STORAGE_FILES: u64 = 1_000_000;
pub(crate) const MAX_RTMP_HLS_ACTIVE_STREAMS: u64 = 100_000;
pub(crate) const MAX_RTMP_HLS_KEY_URL_PREFIX_BYTES: usize = 512;
pub(crate) const MAX_RTMP_HLS_KEY_ROTATION_SEGMENTS: u64 = 100_000;
pub(crate) const MAX_RTMP_DASH_OUTPUTS: usize = 64;
pub(crate) const MAX_RTMP_DASH_SEGMENT_DURATION_MS: u64 = 120_000;
pub(crate) const MAX_RTMP_DASH_PLAYLIST_LENGTH_MS: u64 = 86_400_000;
pub(crate) const MAX_RTMP_DASH_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_RTMP_DASH_QUEUE_MESSAGES: u64 = 65_536;
pub(crate) const MAX_RTMP_DASH_STORAGE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub(crate) const MAX_RTMP_DASH_STORAGE_FILES: u64 = 1_000_000;
pub(crate) const MAX_RTMP_DASH_ACTIVE_STREAMS: u64 = 100_000;
pub(crate) const MAX_RECORDING_SUFFIX_TEMPLATE_BYTES: usize = 128;
pub(crate) const MAX_RECORDER_ROTATION_INTERVAL_MS: u64 = (1 << 31) - 1;
pub(crate) const MAX_RECORDER_QUEUE_MESSAGES: u64 = 65_536;
pub(crate) const MAX_RECORDER_QUEUE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_RECORDER_SHUTDOWN_TIMEOUT_MS: u64 = 60_000;
pub(crate) const MAX_RECORDER_STORAGE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub(crate) const MAX_RECORDER_STORAGE_FILES: u64 = 1_000_000;
pub(crate) const MAX_RECORDER_ACTIVE_RECORDERS: u64 = 256;
pub(crate) const MAX_RECORDER_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub(crate) const MAX_RECORDER_FRAME_COUNT: u64 = 1_000_000_000;
pub(crate) const MAX_RTMP_OUTBOUND_CHUNK_SIZE: u32 = 1_048_576;
pub(crate) const MAX_RTMP_PUSH_TARGETS: usize = 16;
pub(crate) const MAX_RTMP_PULL_TARGETS: usize = 16;
pub(crate) const MAX_RTMP_APPLICATION_BYTES: usize = 255;
pub(crate) const MAX_RTMP_APPLICATION_NAME_BYTES: usize = 128;
pub(crate) const MAX_RTMP_ACCESS_RULES_PER_OPERATION: usize = 64;
pub(crate) const MAX_RTMP_TOKEN_PARAMETER_BYTES: usize = 32;
pub(crate) const MAX_RTMP_TOKEN_BYTES: usize = 128;
pub(crate) const MAX_RTMP_APPLICATION_CONNECTIONS: u64 = 100_000;
pub(crate) const MAX_RTMP_APPLICATION_PUBLISHERS: u64 = 10_000;
pub(crate) const MAX_RTMP_APPLICATION_VIEWERS: u64 = 1_000_000;
pub(crate) const DEFAULT_RTMP_APPLICATION_CONNECTIONS: u64 = 1_024;
pub(crate) const DEFAULT_RTMP_APPLICATION_PUBLISHERS: u64 = 256;
pub(crate) const DEFAULT_RTMP_APPLICATION_VIEWERS: u64 = 1_024;
pub(crate) const MAX_RTMP_SUBSCRIBERS: u64 = 1_000_000;
pub(crate) const MAX_RTMP_FANOUT_QUEUE_MESSAGES: u64 = 65_536;
pub(crate) const MAX_RTMP_FANOUT_QUEUE_BYTES: u64 = 1_024 * 1_024 * 1_024;
pub(crate) const MAX_RTMP_RELAY_BUFFER_MS: u64 = 60_000;
pub(crate) const MAX_RTMP_RECONNECT_MS: u64 = 300_000;
pub(crate) const MAX_RTMP_RELAY_TIMEOUT_MS: u64 = 30_000;
pub(crate) const DEFAULT_RTMP_CALLBACK_TIMEOUT_MS: u64 = 10_000;
pub(crate) const DEFAULT_RTMP_CALLBACK_UPDATE_TIMEOUT_MS: u64 = 30_000;
pub(crate) const MAX_RTMP_CALLBACK_URL_BYTES: usize = 2_048;
pub(crate) const MAX_RTMP_CHAIN_DEPTH: u8 = 16;
pub(crate) const MAX_RTMP_OUTBOUND_DOMAINS: usize = 64;
pub(crate) const MAX_RTMP_OUTBOUND_CIDRS: usize = 64;
pub(crate) const MAX_RTMP_CREDENTIAL_USERNAME_BYTES: usize = 128;
pub(crate) const MAX_RTMP_SECRET_FILE_BYTES: usize = 4_096;
pub(crate) const MAX_RTMP_VOD_SOURCES: usize = 16;
pub(crate) const MAX_RTMP_VOD_SOURCE_NAME_BYTES: usize = 128;
pub(crate) const MAX_RTMP_VOD_ORIGIN_BYTES: usize = 2_048;
pub(crate) const MAX_RTMP_VOD_SESSIONS: u64 = 1_024;
pub(crate) const MAX_RTMP_VOD_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_RTMP_VOD_DURATION_MS: u64 = 86_400_000;
pub(crate) const DEFAULT_RTMP_VOD_SESSIONS: u64 = 64;
pub(crate) const DEFAULT_RTMP_VOD_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const DEFAULT_RTMP_VOD_DURATION_MS: u64 = 21_600_000;
pub(crate) const MAX_CACHE_STORES: usize = 64;
pub(crate) const MAX_CACHE_STORE_BYTES: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
pub(crate) const MAX_CACHE_ENTRIES: u64 = 10_000_000;
pub(crate) const MAX_CACHE_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_CACHE_HEADER_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_CACHE_KEY_BYTES: u64 = 16 * 1024;
pub(crate) const MAX_CACHE_TAG_BYTES: u64 = 1024;
pub(crate) const MAX_CACHE_TAGS_PER_OBJECT: u64 = 256;
pub(crate) const MAX_CACHE_IN_FLIGHT_FILLS: u64 = 65_536;
pub(crate) const MAX_CACHE_FOLLOWERS_PER_FILL: u64 = 4_096;
pub(crate) const MAX_CACHE_METHODS: usize = 8;
pub(crate) const MAX_CACHE_KEY_COMPONENTS: usize = 32;
pub(crate) const MAX_CACHE_STATUS_TTLS: usize = 64;
pub(crate) const MAX_CACHE_PREDICATES: usize = 32;
pub(crate) const MAX_CACHE_STALE_TRIGGERS: usize = 8;
pub(crate) const MAX_CACHE_RETENTION_MS: u64 = 31_536_000_000;
pub(crate) const MAX_FORWARD_PROXY_SERVICES: usize = 64;
pub(crate) const MAX_FORWARD_DOMAINS: usize = 256;
pub(crate) const MAX_FORWARD_CIDRS: usize = 256;
pub(crate) const MAX_FORWARD_CONNECT_PORTS: usize = 64;
pub(crate) const MAX_FORWARD_TIMEOUT_MS: u64 = 86_400_000;
pub(crate) const MAX_FORWARD_CONNECTIONS: u64 = 1_000_000;
pub(crate) const MAX_FORWARD_HEADER_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_FORWARD_BODY_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_FORWARD_RESOLVER_CACHE_ENTRIES: u64 = 1_000_000;
pub(crate) const MAX_FORWARD_RESOLVER_CONCURRENT_QUERIES: u64 = 65_536;
pub(crate) const MAX_FORWARD_RESOLVER_ADDRESSES: u64 = 256;
pub(crate) const MAX_FORWARD_NAMESERVERS: usize = 8;
pub(crate) const MAX_FORWARD_ACCESS_RULES: usize = 256;
pub(crate) const MAX_FORWARD_ACCESS_CONDITIONS: usize = 64;
pub(crate) const MAX_FORWARD_ACCESS_MATCHERS: usize = 256;
pub(crate) const MAX_FORWARD_TIME_RANGES: usize = 256;

pub(crate) const fn default_true() -> bool {
    true
}

pub(crate) const fn default_rtmp_hls_segment_duration_ms() -> u64 {
    DEFAULT_RTMP_HLS_SEGMENT_DURATION_MS
}

pub(crate) const fn default_rtmp_hls_max_segment_duration_ms() -> u64 {
    DEFAULT_RTMP_HLS_MAX_SEGMENT_DURATION_MS
}

pub(crate) const fn default_rtmp_hls_playlist_length_ms() -> u64 {
    DEFAULT_RTMP_HLS_PLAYLIST_LENGTH_MS
}

pub(crate) const fn default_rtmp_hls_max_segment_bytes() -> u64 {
    DEFAULT_RTMP_HLS_MAX_SEGMENT_BYTES
}

pub(crate) const fn default_rtmp_hls_max_queue_messages() -> u64 {
    DEFAULT_RTMP_HLS_MAX_QUEUE_MESSAGES
}

pub(crate) const fn default_rtmp_hls_max_storage_bytes() -> u64 {
    DEFAULT_RTMP_HLS_MAX_STORAGE_BYTES
}

pub(crate) const fn default_rtmp_hls_max_storage_files() -> u64 {
    DEFAULT_RTMP_HLS_MAX_STORAGE_FILES
}

pub(crate) const fn default_rtmp_hls_max_active_streams() -> u64 {
    DEFAULT_RTMP_HLS_MAX_ACTIVE_STREAMS
}

pub(crate) const fn default_rtmp_dash_segment_duration_ms() -> u64 {
    DEFAULT_RTMP_DASH_SEGMENT_DURATION_MS
}

pub(crate) const fn default_rtmp_dash_max_segment_duration_ms() -> u64 {
    DEFAULT_RTMP_DASH_MAX_SEGMENT_DURATION_MS
}

pub(crate) const fn default_rtmp_dash_max_segment_bytes() -> u64 {
    DEFAULT_RTMP_DASH_MAX_SEGMENT_BYTES
}

pub(crate) const fn default_rtmp_dash_max_queue_messages() -> u64 {
    DEFAULT_RTMP_DASH_MAX_QUEUE_MESSAGES
}

pub(crate) const fn default_rtmp_dash_max_storage_bytes() -> u64 {
    DEFAULT_RTMP_DASH_MAX_STORAGE_BYTES
}

pub(crate) const fn default_rtmp_dash_max_storage_files() -> u64 {
    DEFAULT_RTMP_DASH_MAX_STORAGE_FILES
}

pub(crate) const fn default_rtmp_dash_max_active_streams() -> u64 {
    DEFAULT_RTMP_DASH_MAX_ACTIVE_STREAMS
}

pub(crate) const fn default_self_signed_validity_days() -> u32 {
    7
}

pub(crate) const fn default_acme_retained_revisions() -> u32 {
    DEFAULT_ACME_RETAINED_REVISIONS
}

pub(crate) const fn default_acme_retention_days() -> u32 {
    DEFAULT_ACME_RETENTION_DAYS
}

pub(crate) const fn default_upstream_io_timeout_ms() -> u64 {
    DEFAULT_UPSTREAM_IO_TIMEOUT_MS
}

pub(crate) const fn default_max_request_body_bytes() -> Option<u64> {
    const DEFAULT: Option<u64> = Some(DEFAULT_MAX_REQUEST_BODY_BYTES);
    DEFAULT
}

pub(crate) const fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}

pub(crate) const fn default_idle_timeout_ms() -> u64 {
    DEFAULT_IDLE_TIMEOUT_MS
}

pub(crate) const fn default_udp_max_datagram_bytes() -> u64 {
    DEFAULT_UDP_MAX_DATAGRAM_BYTES
}

pub(crate) const fn default_udp_max_sessions() -> u64 {
    DEFAULT_UDP_MAX_SESSIONS
}

pub(crate) const fn default_udp_max_session_bytes() -> u64 {
    DEFAULT_UDP_MAX_SESSION_BYTES
}

pub(crate) const fn default_udp_max_queue_datagrams() -> u64 {
    DEFAULT_UDP_MAX_QUEUE_DATAGRAMS
}

pub(crate) const fn default_udp_max_queue_bytes() -> u64 {
    DEFAULT_UDP_MAX_QUEUE_BYTES
}

pub(crate) const fn default_acme_dns01_timeout_seconds() -> u64 {
    DEFAULT_ACME_DNS01_TIMEOUT_SECONDS
}

pub(crate) const fn default_proxy_protocol_timeout_ms() -> u64 {
    DEFAULT_PROXY_PROTOCOL_TIMEOUT_MS
}

pub(crate) const fn default_health_interval_ms() -> u64 {
    DEFAULT_HEALTH_INTERVAL_MS
}

pub(crate) const fn default_health_timeout_ms() -> u64 {
    DEFAULT_HEALTH_TIMEOUT_MS
}

pub(crate) const fn default_healthy_threshold() -> u16 {
    DEFAULT_HEALTHY_THRESHOLD
}

pub(crate) const fn default_unhealthy_threshold() -> u16 {
    DEFAULT_UNHEALTHY_THRESHOLD
}

pub(crate) const fn default_http_route_policy() -> HttpRoutePolicy {
    HttpRoutePolicy::new()
}

pub(crate) const fn default_rtmp_outbound_chunk_size() -> u32 {
    DEFAULT_RTMP_OUTBOUND_CHUNK_SIZE
}

pub(crate) const fn default_rtmp_fanout_policy() -> RtmpFanoutPolicy {
    RtmpFanoutPolicy {
        max_subscribers: DEFAULT_RTMP_MAX_SUBSCRIBERS,
        max_queue_messages_per_subscriber: DEFAULT_RTMP_FANOUT_QUEUE_MESSAGES,
        max_queue_bytes_per_subscriber: DEFAULT_RTMP_FANOUT_QUEUE_BYTES,
    }
}

pub(crate) const fn default_rtmp_session_ceilings() -> RtmpSessionCeilings {
    RtmpSessionCeilings {
        max_connections: DEFAULT_RTMP_APPLICATION_CONNECTIONS,
        max_publishers: DEFAULT_RTMP_APPLICATION_PUBLISHERS,
        max_viewers: DEFAULT_RTMP_APPLICATION_VIEWERS,
    }
}

pub(crate) const fn default_rtmp_relay_policy() -> RtmpRelayPolicy {
    RtmpRelayPolicy {
        max_queue_messages: DEFAULT_RTMP_FANOUT_QUEUE_MESSAGES,
        max_queue_bytes: DEFAULT_RTMP_FANOUT_QUEUE_BYTES,
        buffer_ms: DEFAULT_RTMP_RELAY_BUFFER_MS,
        push_reconnect_ms: DEFAULT_RTMP_PUSH_RECONNECT_MS,
        pull_reconnect_ms: DEFAULT_RTMP_PULL_RECONNECT_MS,
        connect_timeout_ms: DEFAULT_RTMP_RELAY_CONNECT_TIMEOUT_MS,
        handshake_timeout_ms: DEFAULT_RTMP_RELAY_HANDSHAKE_TIMEOUT_MS,
    }
}

pub(crate) const fn default_rtmp_relay_queue_messages() -> u64 {
    DEFAULT_RTMP_FANOUT_QUEUE_MESSAGES
}

pub(crate) const fn default_rtmp_relay_queue_bytes() -> u64 {
    DEFAULT_RTMP_FANOUT_QUEUE_BYTES
}

pub(crate) const fn default_rtmp_relay_buffer_ms() -> u64 {
    DEFAULT_RTMP_RELAY_BUFFER_MS
}

pub(crate) const fn default_rtmp_push_reconnect_ms() -> u64 {
    DEFAULT_RTMP_PUSH_RECONNECT_MS
}

pub(crate) const fn default_rtmp_pull_reconnect_ms() -> u64 {
    DEFAULT_RTMP_PULL_RECONNECT_MS
}

pub(crate) const fn default_rtmp_relay_connect_timeout_ms() -> u64 {
    DEFAULT_RTMP_RELAY_CONNECT_TIMEOUT_MS
}

pub(crate) const fn default_rtmp_relay_handshake_timeout_ms() -> u64 {
    DEFAULT_RTMP_RELAY_HANDSHAKE_TIMEOUT_MS
}

pub(crate) const fn default_rtmp_callback_timeout_ms() -> u64 {
    DEFAULT_RTMP_CALLBACK_TIMEOUT_MS
}

pub(crate) const fn default_rtmp_callback_update_timeout_ms() -> u64 {
    DEFAULT_RTMP_CALLBACK_UPDATE_TIMEOUT_MS
}

pub(crate) const fn default_rtmp_max_chain_depth() -> u8 {
    DEFAULT_RTMP_MAX_CHAIN_DEPTH
}

pub(crate) const fn default_rtmp_outbound_policy() -> RtmpOutboundPolicy {
    RtmpOutboundPolicy {
        allow_domains: Vec::new(),
        deny_domains: Vec::new(),
        allow_cidrs: Vec::new(),
        deny_cidrs: Vec::new(),
        deny_private: true,
        rtmps: RtmpRtmpsPolicy::Disabled,
        max_chain_depth: DEFAULT_RTMP_MAX_CHAIN_DEPTH,
    }
}

pub(crate) const fn default_rtmp_vod_sessions() -> u64 {
    DEFAULT_RTMP_VOD_SESSIONS
}

pub(crate) const fn default_rtmp_vod_file_bytes() -> u64 {
    DEFAULT_RTMP_VOD_FILE_BYTES
}

pub(crate) const fn default_rtmp_vod_duration_ms() -> u64 {
    DEFAULT_RTMP_VOD_DURATION_MS
}

pub(crate) fn default_alpn() -> Vec<AlpnProtocol> {
    vec![AlpnProtocol::Http11]
}

pub(crate) fn default_recorder_suffix_template() -> String {
    ".flv".into()
}

pub(crate) const fn default_recorder_max_queue_messages() -> u64 {
    DEFAULT_RECORDER_MAX_QUEUE_MESSAGES
}

pub(crate) const fn default_recorder_max_queue_bytes() -> u64 {
    DEFAULT_RECORDER_MAX_QUEUE_BYTES
}

pub(crate) const fn default_recorder_shutdown_timeout_ms() -> u64 {
    DEFAULT_RECORDER_SHUTDOWN_TIMEOUT_MS
}

pub(crate) const fn default_recorder_max_active_recorders() -> u64 {
    DEFAULT_RECORDER_MAX_ACTIVE_RECORDERS
}

pub(crate) fn default_http_access_header_name() -> String {
    "authorization".into()
}

pub(crate) fn default_http_retry_triggers() -> Vec<HttpRetryTrigger> {
    vec![
        HttpRetryTrigger::ConnectFailure,
        HttpRetryTrigger::ConnectTimeout,
        HttpRetryTrigger::RefusedStream,
    ]
}

pub(crate) const fn default_http_redirect_status() -> u16 {
    302
}

pub(crate) fn default_http_static_index_files() -> Vec<String> {
    vec!["index.html".into()]
}

pub(crate) const fn default_cache_max_bytes() -> u64 {
    DEFAULT_CACHE_MAX_BYTES
}

pub(crate) const fn default_disk_cache_max_bytes() -> u64 {
    DEFAULT_DISK_CACHE_MAX_BYTES
}

pub(crate) const fn default_cache_max_entries() -> u64 {
    DEFAULT_CACHE_MAX_ENTRIES
}

pub(crate) const fn default_disk_cache_max_files() -> u64 {
    DEFAULT_DISK_CACHE_MAX_FILES
}

pub(crate) const fn default_cache_max_object_bytes() -> u64 {
    DEFAULT_CACHE_MAX_OBJECT_BYTES
}

pub(crate) const fn default_cache_max_header_bytes() -> u64 {
    DEFAULT_CACHE_MAX_HEADER_BYTES
}

pub(crate) const fn default_cache_max_key_bytes() -> u64 {
    DEFAULT_CACHE_MAX_KEY_BYTES
}

pub(crate) const fn default_cache_max_tag_bytes() -> u64 {
    DEFAULT_CACHE_MAX_TAG_BYTES
}

pub(crate) const fn default_cache_max_tags_per_object() -> u64 {
    DEFAULT_CACHE_MAX_TAGS_PER_OBJECT
}

pub(crate) const fn default_cache_max_in_flight_fills() -> u64 {
    DEFAULT_CACHE_MAX_IN_FLIGHT_FILLS
}

pub(crate) const fn default_cache_max_followers_per_fill() -> u64 {
    DEFAULT_CACHE_MAX_FOLLOWERS_PER_FILL
}

pub(crate) fn default_cache_methods() -> Vec<String> {
    vec!["GET".into(), "HEAD".into()]
}

pub(crate) fn default_cache_key_components() -> Vec<CacheKeyComponent> {
    vec![
        CacheKeyComponent::Scheme,
        CacheKeyComponent::NormalizedHost,
        CacheKeyComponent::PathAndQuery,
    ]
}

pub(crate) const fn default_cache_ttl_ms() -> u64 {
    DEFAULT_CACHE_TTL_MS
}

pub(crate) const fn default_cache_grace_ms() -> u64 {
    DEFAULT_CACHE_GRACE_MS
}

pub(crate) const fn default_cache_keep_ms() -> u64 {
    DEFAULT_CACHE_KEEP_MS
}

pub(crate) fn default_forward_http_versions() -> Vec<ForwardHttpVersion> {
    vec![ForwardHttpVersion::H1]
}

pub(crate) fn default_forward_connect_ports() -> Vec<u16> {
    vec![443]
}

pub(crate) const fn default_forward_lifetime_timeout_ms() -> u64 {
    DEFAULT_FORWARD_LIFETIME_TIMEOUT_MS
}

pub(crate) const fn default_forward_max_connections() -> u64 {
    DEFAULT_FORWARD_MAX_CONNECTIONS
}

pub(crate) const fn default_forward_max_header_bytes() -> u64 {
    DEFAULT_FORWARD_MAX_HEADER_BYTES
}

pub(crate) const fn default_forward_resolver_cache_entries() -> u64 {
    DEFAULT_FORWARD_RESOLVER_CACHE_ENTRIES
}

pub(crate) const fn default_forward_resolver_concurrent_queries() -> u64 {
    DEFAULT_FORWARD_RESOLVER_CONCURRENT_QUERIES
}

pub(crate) const fn default_forward_resolver_max_addresses() -> u64 {
    DEFAULT_FORWARD_RESOLVER_MAX_ADDRESSES
}

pub(crate) const fn default_forward_resolver_min_ttl_ms() -> u64 {
    DEFAULT_FORWARD_RESOLVER_MIN_TTL_MS
}

pub(crate) const fn default_forward_resolver_max_ttl_ms() -> u64 {
    DEFAULT_FORWARD_RESOLVER_MAX_TTL_MS
}

pub(crate) const fn default_forward_resolver_negative_ttl_ms() -> u64 {
    DEFAULT_FORWARD_RESOLVER_NEGATIVE_TTL_MS
}
