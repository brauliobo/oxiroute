use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::defaults::{
    MAX_CERTIFICATE_DNS_NAMES, MAX_CERTIFICATES, MAX_ENDPOINTS_PER_POOL,
    MAX_RTMP_APPLICATIONS_PER_SERVICE, MAX_RTMP_RECORDERS_PER_APPLICATION,
    MAX_RTMP_RECORDING_ROOTS, MAX_RTMP_SERVICES, MAX_SOURCE_BYTES, MAX_TLS_PROFILES,
    MAX_TOTAL_ENDPOINTS, MAX_TOTAL_RTMP_RECORDERS, default_alpn, default_cache_grace_ms,
    default_cache_keep_ms, default_cache_key_components, default_cache_max_bytes,
    default_cache_max_entries, default_cache_max_followers_per_fill,
    default_cache_max_header_bytes, default_cache_max_in_flight_fills, default_cache_max_key_bytes,
    default_cache_max_object_bytes, default_cache_max_tag_bytes, default_cache_max_tags_per_object,
    default_cache_methods, default_cache_ttl_ms, default_connect_timeout_ms,
    default_disk_cache_max_bytes, default_disk_cache_max_files, default_forward_connect_ports,
    default_forward_http_versions, default_forward_lifetime_timeout_ms,
    default_forward_max_connections, default_forward_max_header_bytes,
    default_forward_resolver_cache_entries, default_forward_resolver_concurrent_queries,
    default_forward_resolver_max_addresses, default_forward_resolver_max_ttl_ms,
    default_forward_resolver_min_ttl_ms, default_forward_resolver_negative_ttl_ms,
    default_health_interval_ms, default_health_timeout_ms, default_healthy_threshold,
    default_http_access_header_name, default_http_redirect_status, default_http_retry_triggers,
    default_http_route_policy, default_http_static_index_files, default_idle_timeout_ms,
    default_max_request_body_bytes, default_recorder_max_active_recorders,
    default_recorder_max_queue_bytes, default_recorder_max_queue_messages,
    default_recorder_shutdown_timeout_ms, default_recorder_suffix_template,
    default_rtmp_callback_timeout_ms, default_rtmp_callback_update_timeout_ms,
    default_rtmp_fanout_policy, default_rtmp_max_chain_depth, default_rtmp_outbound_chunk_size,
    default_rtmp_outbound_policy, default_rtmp_pull_reconnect_ms, default_rtmp_push_reconnect_ms,
    default_rtmp_relay_buffer_ms, default_rtmp_relay_connect_timeout_ms,
    default_rtmp_relay_handshake_timeout_ms, default_rtmp_relay_policy,
    default_rtmp_relay_queue_bytes, default_rtmp_relay_queue_messages,
    default_rtmp_session_ceilings, default_rtmp_vod_duration_ms, default_rtmp_vod_file_bytes,
    default_rtmp_vod_sessions, default_self_signed_validity_days, default_true,
    default_udp_max_datagram_bytes, default_udp_max_queue_bytes, default_udp_max_queue_datagrams,
    default_udp_max_session_bytes, default_udp_max_sessions, default_unhealthy_threshold,
    default_upstream_io_timeout_ms,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    /// Aggregate process admission cap. Omitted or explicit null means unbounded.
    #[serde(default)]
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub management: Option<Management>,
    #[serde(default)]
    pub stats: Option<Stats>,
    #[serde(default)]
    pub certificates: Vec<Certificate>,
    #[serde(default)]
    pub tls_profiles: Vec<TlsProfile>,
    pub listeners: Vec<Listener>,
    #[serde(default)]
    pub cache_stores: Vec<CacheStore>,
    #[serde(default)]
    pub upstream_pools: Vec<UpstreamPool>,
    #[serde(default)]
    pub http_services: Vec<HttpService>,
    #[serde(default)]
    pub forward_proxy_services: Vec<ForwardProxyService>,
    #[serde(default)]
    pub rtmp_services: Vec<RtmpService>,
    #[serde(default)]
    pub l4_services: Vec<L4Service>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    pub name: String,
    pub dns_names: Vec<String>,
    pub source: CertificateSource,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateSource {
    Files {
        certificate_chain_path: PathBuf,
        private_key_path: PathBuf,
    },
    Certbot {
        live_directory_path: PathBuf,
        archive_directory_path: PathBuf,
    },
    AcmeManaged {
        directory_url: String,
        state_root: PathBuf,
        #[serde(default)]
        contacts: Vec<String>,
        terms_agreed: bool,
        #[serde(default)]
        challenge: AcmeChallengeType,
        #[serde(default)]
        key_type: AcmeKeyType,
        allowed_dns_suffixes: Vec<String>,
    },
    SelfSignedDevelopment {
        #[serde(default = "default_self_signed_validity_days")]
        validity_days: u32,
        #[serde(default)]
        key_type: SelfSignedKeyType,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcmeChallengeType {
    #[default]
    Http01,
    Dns01,
    TlsAlpn01,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcmeKeyType {
    #[default]
    EcdsaP256,
    #[serde(rename = "rsa_2048")]
    Rsa2048,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelfSignedKeyType {
    #[default]
    EcdsaP256,
    #[serde(rename = "rsa_2048")]
    Rsa2048,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsProfile {
    pub name: String,
    pub certificates: Vec<String>,
    pub default_certificate: String,
    #[serde(default)]
    pub min_version: TlsVersion,
    #[serde(default = "default_alpn")]
    pub alpn: Vec<AlpnProtocol>,
    #[serde(default)]
    pub policy: TlsPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsPolicy {
    #[serde(default)]
    pub cipher_list: Option<String>,
    #[serde(default)]
    pub dh_parameters_path: Option<PathBuf>,
    #[serde(default)]
    pub session_cache: Option<TlsSessionCache>,
    #[serde(default)]
    pub session_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub session_tickets: bool,
    #[serde(default = "default_true")]
    pub prefer_server_ciphers: bool,
}

impl Default for TlsPolicy {
    fn default() -> Self {
        Self {
            cipher_list: None,
            dh_parameters_path: None,
            session_cache: None,
            session_timeout_seconds: None,
            session_tickets: false,
            prefer_server_ciphers: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsSessionCache {
    pub name: String,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum TlsVersion {
    #[default]
    #[serde(rename = "1.2")]
    Tls12,
    #[serde(rename = "1.3")]
    Tls13,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum AlpnProtocol {
    #[serde(rename = "h3")]
    H3,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http/1.1")]
    Http11,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Management {
    pub bind: SocketAddr,
    #[serde(default)]
    pub ui_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    /// Every IPv4 or IPv6 address that serves `/stats` and `/metrics`.
    #[serde(default)]
    pub binds: Vec<SocketAddr>,
    /// Required for loopback `/stats`, `/api/v1/status`, and state-changing admin requests. The file
    /// contents are never rendered into status, stats, or metrics output.
    #[serde(default)]
    pub admin_token_file: Option<PathBuf>,
    /// Independent public, read-only HAProxy-compatible status pages.
    #[serde(default)]
    pub pages: Vec<StatsPage>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatsPage {
    pub bind: SocketAddr,
    pub uri_prefix: String,
    pub refresh_ms: u64,
    pub admin: StatsPageAdminPolicy,
    /// Concurrent connection cap. Omitted or explicit null means unbounded.
    #[serde(default)]
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub downstream_timeouts: DownstreamTimeoutPolicy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatsPageAdminPolicy {
    #[default]
    Disabled,
    Localhost,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    pub name: String,
    pub bind: ListenerBind,
    pub protocol: Protocol,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub tls_profile: Option<String>,
    /// Concurrent connection cap. Omitted or explicit null means unbounded.
    #[serde(default)]
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub downstream_timeouts: DownstreamTimeoutPolicy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DownstreamTimeoutPolicy {
    #[serde(default)]
    pub client_timeout_ms: Option<u64>,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub keepalive_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ListenerBind {
    Socket {
        address: SocketAddr,
    },
    Udp {
        address: SocketAddr,
    },
    Unix {
        path: PathBuf,
        /// Optional Unix permission bits applied when creating the socket.
        #[serde(default)]
        mode: Option<u16>,
    },
}

impl fmt::Display for ListenerBind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { address } => address.fmt(formatter),
            Self::Udp { address } => write!(formatter, "udp://{address}"),
            Self::Unix { path, .. } => path.display().fmt(formatter),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Http,
    Rtmp,
    Tcp,
    Udp,
    ForwardHttp1,
    ForwardHttp2,
    ForwardHttp3,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheStore {
    Memory {
        name: String,
        #[serde(default = "default_cache_max_bytes")]
        max_bytes: u64,
        #[serde(default = "default_cache_max_entries")]
        max_entries: u64,
        #[serde(default = "default_cache_max_object_bytes")]
        max_object_bytes: u64,
        #[serde(default = "default_cache_max_header_bytes")]
        max_header_bytes: u64,
        #[serde(default = "default_cache_max_key_bytes")]
        max_key_bytes: u64,
        #[serde(default = "default_cache_max_tag_bytes")]
        max_tag_bytes: u64,
        #[serde(default = "default_cache_max_tags_per_object")]
        max_tags_per_object: u64,
        #[serde(default = "default_cache_max_in_flight_fills")]
        max_in_flight_fills: u64,
        #[serde(default = "default_cache_max_followers_per_fill")]
        max_followers_per_fill: u64,
    },
    Disk {
        name: String,
        root_directory: PathBuf,
        #[serde(default = "default_disk_cache_max_bytes")]
        max_bytes: u64,
        #[serde(default = "default_disk_cache_max_files")]
        max_files: u64,
        #[serde(default = "default_cache_max_object_bytes")]
        max_object_bytes: u64,
        #[serde(default = "default_cache_max_header_bytes")]
        max_header_bytes: u64,
        #[serde(default = "default_cache_max_key_bytes")]
        max_key_bytes: u64,
        #[serde(default = "default_cache_max_tag_bytes")]
        max_tag_bytes: u64,
        #[serde(default = "default_cache_max_tags_per_object")]
        max_tags_per_object: u64,
        #[serde(default = "default_cache_max_in_flight_fills")]
        max_in_flight_fills: u64,
        #[serde(default = "default_cache_max_followers_per_fill")]
        max_followers_per_fill: u64,
    },
}

impl CacheStore {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Memory { name, .. } | Self::Disk { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamPool {
    pub name: String,
    #[serde(default)]
    pub servers: Vec<UpstreamServer>,
    /// Decode-only compatibility for pre-named canonical configurations.
    #[serde(default, skip_serializing)]
    pub endpoints: Vec<UpstreamEndpoint>,
    #[serde(default)]
    pub algorithm: UpstreamAlgorithm,
    #[serde(default)]
    pub health_check: Option<HealthCheck>,
    #[serde(default)]
    pub tls: Option<UpstreamTls>,
    #[serde(default)]
    pub http_versions: HttpVersionPolicy,
    #[serde(default)]
    pub queue_timeout_ms: Option<u64>,
    #[serde(default)]
    pub connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub server_timeout_ms: Option<u64>,
    #[serde(default)]
    pub connection_reuse: UpstreamConnectionReuse,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamServer {
    pub name: String,
    pub endpoint: UpstreamEndpoint,
    #[serde(default)]
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub dns_resolution: DnsResolutionPolicy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DnsResolutionPolicy {
    Startup,
    #[default]
    OnConnect,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamConnectionReuse {
    Never,
    #[default]
    Safe,
    Always,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpstreamEndpoint {
    Socket { address: SocketAddr },
    Dns { host: String, port: u16 },
    Unix { path: PathBuf },
}

impl fmt::Display for UpstreamEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { address } => address.fmt(formatter),
            Self::Dns { host, port } => write!(formatter, "{host}:{port}"),
            Self::Unix { path } => path.display().fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamTls {
    pub server_name: String,
    #[serde(default)]
    pub ca_certificate_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpVersionPolicy {
    #[serde(default)]
    pub min: HttpVersion,
    #[serde(default)]
    pub max: HttpVersion,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum HttpVersion {
    #[default]
    #[serde(rename = "1.1")]
    Http11,
    #[serde(rename = "2")]
    Http2,
}

impl HttpVersion {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http11 => "1.1",
            Self::Http2 => "2",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpstreamAlgorithm {
    RoundRobin,
    WeightedRoundRobin { weights: Vec<u16> },
    LeastConnections,
    First,
}

impl Default for UpstreamAlgorithm {
    fn default() -> Self {
        Self::RoundRobin
    }
}

impl Serialize for UpstreamAlgorithm {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::RoundRobin => serializer.serialize_str("round_robin"),
            Self::LeastConnections => serializer.serialize_str("least_connections"),
            Self::First => serializer.serialize_str("first"),
            Self::WeightedRoundRobin { weights } => {
                use serde::ser::SerializeStruct as _;

                let mut algorithm = serializer.serialize_struct("UpstreamAlgorithm", 2)?;
                algorithm.serialize_field("type", "weighted_round_robin")?;
                algorithm.serialize_field("weights", weights)?;
                algorithm.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for UpstreamAlgorithm {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        let value = UpstreamAlgorithmRepr::deserialize(deserializer)?;
        match value {
            UpstreamAlgorithmRepr::Name(name) => match name.as_str() {
                "round_robin" => Ok(Self::RoundRobin),
                "least_connections" => Ok(Self::LeastConnections),
                "first" => Ok(Self::First),
                "weighted_round_robin" => Ok(Self::WeightedRoundRobin {
                    weights: Vec::new(),
                }),
                _ => Err(D::Error::custom(format!(
                    "unknown upstream algorithm `{name}`"
                ))),
            },
            UpstreamAlgorithmRepr::Weighted(weighted) => {
                if weighted.kind != "weighted_round_robin" {
                    return Err(D::Error::custom(format!(
                        "unknown upstream algorithm `{}`",
                        weighted.kind
                    )));
                }
                Ok(Self::WeightedRoundRobin {
                    weights: weighted.weights,
                })
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UpstreamAlgorithmRepr {
    Name(String),
    Weighted(WeightedRoundRobinConfig),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeightedRoundRobinConfig {
    #[serde(rename = "type")]
    kind: String,
    weights: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthCheck {
    #[serde(rename = "type")]
    pub kind: HealthCheckType,
    #[serde(default = "default_health_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u16,
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u16,
    #[serde(default)]
    pub startup: HealthStartup,
    #[serde(default)]
    pub fast_interval_ms: Option<u64>,
    #[serde(default)]
    pub down_interval_ms: Option<u64>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub expected_status: Option<u16>,
    #[serde(default)]
    pub http_version: Option<HealthHttpVersion>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStartup {
    Healthy,
    Unhealthy,
    #[default]
    Checking,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum HealthHttpVersion {
    #[serde(rename = "1.0")]
    Http10,
    #[serde(rename = "1.1")]
    Http11,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthCheckType {
    Http,
    Tcp,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpService {
    pub name: String,
    pub routes: Vec<HttpRoute>,
    #[serde(default = "default_true")]
    pub automatic_response_headers: bool,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub upstream_io_timeout_ms: u64,
    /// Request body cap. Omitted configs default to 10 MiB; explicit null means unbounded.
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: Option<u64>,
    #[serde(default)]
    pub gzip: Option<HttpGzipPolicy>,
    #[serde(default)]
    pub access_log: Option<AccessLogPolicy>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRoute {
    /// Host precedence is exact authority, normalized exact/IP, normalized wildcard, then none.
    #[serde(default)]
    pub host: Option<HttpHostSelector>,
    /// Path precedence is exact, segment prefix, then raw prefix; longer prefixes win within kind.
    pub path: HttpPathSelector,
    /// A nonempty method set precedes an any-method route; source order resolves final ties.
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub access_policy: Option<HttpAccessPolicy>,
    #[serde(default = "default_http_route_policy")]
    pub policy: HttpRoutePolicy,
    pub action: HttpRouteAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRoutePolicy {
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: Option<u64>,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub read_timeout_ms: u64,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub write_timeout_ms: u64,
    #[serde(default)]
    pub request_buffering: bool,
    #[serde(default)]
    pub response_buffering: bool,
}

impl HttpRoutePolicy {
    pub(crate) const fn new() -> Self {
        Self {
            max_request_body_bytes: Some(10 * 1024 * 1024),
            connect_timeout_ms: 30_000,
            read_timeout_ms: 30_000,
            write_timeout_ms: 30_000,
            request_buffering: false,
            response_buffering: false,
        }
    }
}

impl Default for HttpRoutePolicy {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpGzipPolicy {
    pub level: u8,
    pub content_types: Vec<String>,
    #[serde(default = "default_http_gzip_min_length_bytes")]
    pub min_length_bytes: u64,
    #[serde(default)]
    pub min_http_version: HttpGzipMinimumVersion,
    #[serde(default)]
    pub disable_on_via: bool,
    #[serde(default = "default_true")]
    pub vary: bool,
}

impl Default for HttpGzipPolicy {
    fn default() -> Self {
        Self {
            level: 1,
            content_types: vec!["text/html".into()],
            min_length_bytes: default_http_gzip_min_length_bytes(),
            min_http_version: HttpGzipMinimumVersion::default(),
            disable_on_via: false,
            vary: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum HttpGzipMinimumVersion {
    #[default]
    #[serde(rename = "1.0")]
    Http10,
    #[serde(rename = "1.1")]
    Http11,
}

const fn default_http_gzip_min_length_bytes() -> u64 {
    20
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessLogPolicy {
    Disabled,
    File { path: PathBuf },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpHostSelector {
    NormalizedHost {
        value: String,
    },
    ExactAuthority {
        value: String,
    },
    AsciiCaseInsensitiveExactAuthority {
        value: String,
    },
    /// nginx `*.example.com`: matches one or more labels before the suffix.
    NginxLeadingWildcard {
        value: String,
    },
    /// nginx `.example.com`: matches the suffix itself and any leading labels.
    NginxLeadingDot {
        value: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpPathSelector {
    SegmentPrefix { value: String },
    RawPrefix { value: String },
    Exact { value: String },
    AsciiCaseInsensitiveExact { value: String },
}

impl HttpPathSelector {
    pub(crate) fn value_mut(&mut self) -> &mut String {
        match self {
            Self::SegmentPrefix { value }
            | Self::RawPrefix { value }
            | Self::Exact { value }
            | Self::AsciiCaseInsensitiveExact { value } => value,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpAccessPolicy {
    BearerTokenFile {
        token_file_path: PathBuf,
        #[serde(default = "default_http_access_header_name")]
        header_name: String,
        #[serde(default)]
        realm: Option<String>,
    },
    BasicHtpasswdFile {
        htpasswd_file_path: PathBuf,
        realm: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRouteAction {
    Proxy {
        upstream_pool: String,
        policy: HttpProxyPolicy,
    },
    FixedResponse {
        status: u16,
        #[serde(default)]
        body: String,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
    },
    Redirect {
        #[serde(default = "default_http_redirect_status")]
        status: u16,
        location: HttpRedirectLocation,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
    },
    StaticFiles {
        root_directory: PathBuf,
        #[serde(default)]
        path_mapping: HttpStaticPathMapping,
        #[serde(default = "default_http_static_index_files")]
        index_files: Vec<String>,
        #[serde(default)]
        internal_index_redirects: bool,
        #[serde(default)]
        directory_redirects: bool,
        #[serde(default)]
        spa_fallback: Option<PathBuf>,
        #[serde(default)]
        try_files: Vec<HttpStaticTryFile>,
        #[serde(default)]
        autoindex: bool,
        #[serde(default = "default_true")]
        autoindex_exact_size: bool,
        #[serde(default)]
        autoindex_local_time: bool,
        #[serde(default = "default_true", deserialize_with = "deserialize_strict_bool")]
        etag: bool,
        #[serde(default)]
        mime: HttpStaticMimePolicy,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
        #[serde(default)]
        error_responses: Vec<HttpStaticErrorResponse>,
    },
}

fn deserialize_strict_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct StrictBoolVisitor;

    impl serde::de::Visitor<'_> for StrictBoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a boolean for etag")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }
    }

    deserializer.deserialize_any(StrictBoolVisitor)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpStaticPathMapping {
    #[default]
    Root,
    Alias,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpStaticTryFile {
    RequestPath,
    RequestPathDirectory,
    Relative { path: PathBuf },
    Status { status: u16 },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpStaticMimePolicy {
    #[serde(default)]
    pub default_type: Option<String>,
    #[serde(default)]
    pub types: Vec<HttpMimeType>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpMimeType {
    pub extension: String,
    pub content_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpStaticErrorResponse {
    pub statuses: Vec<u16>,
    #[serde(default)]
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: Vec<HttpLiteralHeader>,
    #[serde(default)]
    pub internal_redirect: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpProxyPolicy {
    #[serde(default)]
    pub upstream_host: HttpUpstreamHost,
    #[serde(default)]
    pub request_headers: Vec<HttpRequestHeaderMutation>,
    #[serde(default)]
    pub response_headers: Vec<HttpResponseHeaderMutation>,
    #[serde(default)]
    pub response_cookie_path_rewrites: Vec<HttpCookiePathRewrite>,
    #[serde(default)]
    pub response_cookie_attributes: Vec<HttpCookieAttributePolicy>,
    #[serde(default)]
    pub retry: HttpRetryPolicy,
    #[serde(default)]
    pub cache: Option<Box<HttpCachePolicy>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCachePolicy {
    pub store: String,
    #[serde(default = "default_cache_methods")]
    pub methods: Vec<String>,
    #[serde(default = "default_cache_key_components")]
    pub key_components: Vec<CacheKeyComponent>,
    #[serde(default = "default_true")]
    pub use_origin_cache_control: bool,
    #[serde(default = "default_cache_ttl_ms")]
    pub default_ttl_ms: u64,
    #[serde(default)]
    pub status_ttls: Vec<CacheStatusTtl>,
    #[serde(default = "default_cache_grace_ms")]
    pub grace_ms: u64,
    #[serde(default = "default_cache_keep_ms")]
    pub keep_ms: u64,
    #[serde(default = "default_true")]
    pub revalidate: bool,
    #[serde(default = "default_true")]
    pub collapsed_forwarding: bool,
    #[serde(default)]
    pub stale_on: Vec<CacheStaleTrigger>,
    #[serde(default)]
    pub bypass_request: Vec<CachePredicate>,
    #[serde(default)]
    pub no_store_request: Vec<CachePredicate>,
    #[serde(default)]
    pub no_store_response: Vec<CachePredicate>,
    #[serde(default)]
    pub set_cookie_policy: CacheSetCookiePolicy,
    #[serde(default)]
    pub authorization_policy: CacheAuthorizationPolicy,
    #[serde(default)]
    pub vary_policy: CacheVaryPolicy,
    #[serde(default)]
    pub surrogate_tags: Option<CacheSurrogateTags>,
    #[serde(default)]
    pub purge_authorization: Option<CachePurgeAuthorization>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheKeyComponent {
    Scheme,
    NormalizedHost,
    PathAndQuery,
    Header { name: String },
    Cookie { name: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CacheStatusTtl {
    pub status: u16,
    pub ttl_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CacheStaleTrigger {
    ConnectFailure,
    ConnectTimeout,
    #[serde(rename = "origin_500")]
    Origin500,
    #[serde(rename = "origin_502")]
    Origin502,
    #[serde(rename = "origin_503")]
    Origin503,
    #[serde(rename = "origin_504")]
    Origin504,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CachePredicate {
    HeaderPresent { name: String },
    CookiePresent { name: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheSetCookiePolicy {
    #[default]
    Bypass,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheAuthorizationPolicy {
    #[default]
    Bypass,
    Cache,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheVaryPolicy {
    #[default]
    Respect,
    Ignore,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CacheSurrogateTags {
    pub response_header: String,
    #[serde(default = "default_cache_max_tags_per_object")]
    pub max_tags: u64,
    #[serde(default = "default_cache_max_tag_bytes")]
    pub max_tag_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CachePurgeAuthorization {
    BearerTokenFile { token_file_path: PathBuf },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpUpstreamHost {
    #[default]
    PreserveIncoming,
    NginxHost {
        fallback: String,
    },
    Endpoint {
        #[serde(default)]
        unix_fallback: Option<String>,
    },
    Literal {
        value: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRequestHeaderMutation {
    Set {
        name: String,
        value: HttpRequestHeaderValue,
    },
    Remove {
        name: String,
    },
}

impl HttpRequestHeaderMutation {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Set { name, .. } | Self::Remove { name } => name,
        }
    }

    pub(crate) fn name_mut(&mut self) -> &mut String {
        match self {
            Self::Set { name, .. } | Self::Remove { name } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRequestHeaderValue {
    Literal {
        value: String,
    },
    IncomingAuthority,
    NormalizedHost,
    NginxHost {
        fallback: String,
    },
    ClientIp,
    AppendedXForwardedFor {
        max_bytes: u64,
        #[serde(default)]
        except_source_cidrs: Vec<String>,
    },
    DownstreamScheme,
    IncomingHeader {
        name: String,
        max_bytes: u64,
    },
    SelectedUpstreamHost,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCookieAttributePolicy {
    pub name: String,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default)]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub same_site: Option<HttpSameSite>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpResponseHeaderMutation {
    Set {
        name: String,
        value: String,
        #[serde(default = "default_true")]
        always: bool,
    },
    Add {
        name: String,
        value: String,
        #[serde(default = "default_true")]
        always: bool,
    },
    Remove {
        name: String,
    },
}

impl HttpResponseHeaderMutation {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Set { name, .. } | Self::Add { name, .. } | Self::Remove { name } => name,
        }
    }

    pub(crate) fn name_mut(&mut self) -> &mut String {
        match self {
            Self::Set { name, .. } | Self::Add { name, .. } | Self::Remove { name } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCookiePathRewrite {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRetryPolicy {
    #[serde(default)]
    pub max_retries: u8,
    #[serde(default = "default_http_retry_triggers")]
    pub triggers: Vec<HttpRetryTrigger>,
    #[serde(default)]
    pub method_safety: HttpRetryMethodSafety,
    #[serde(default)]
    pub body_safety: HttpRetryBodySafety,
    #[serde(default)]
    pub target: HttpRetryTarget,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default)]
    pub final_redispatch: bool,
}

impl Default for HttpRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 0,
            triggers: default_http_retry_triggers(),
            method_safety: HttpRetryMethodSafety::default(),
            body_safety: HttpRetryBodySafety::default(),
            target: HttpRetryTarget::default(),
            delay_ms: 0,
            final_redispatch: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryTarget {
    SameServer,
    #[default]
    NextServer,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryTrigger {
    ConnectFailure,
    ConnectTimeout,
    RefusedStream,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryMethodSafety {
    #[default]
    GetHead,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryBodySafety {
    #[default]
    Empty,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpLiteralHeader {
    pub name: String,
    pub value: String,
    #[serde(default = "default_true")]
    pub always: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRedirectLocation {
    Literal {
        value: String,
    },
    RequestTemplate {
        value: String,
        #[serde(default)]
        nginx_host_fallback: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RtmpService {
    pub name: String,
    #[serde(default = "default_rtmp_outbound_chunk_size")]
    pub outbound_chunk_size: u32,
    #[serde(default)]
    pub access_log: Option<AccessLogPolicy>,
    #[serde(default = "default_rtmp_outbound_policy")]
    pub outbound_policy: RtmpOutboundPolicy,
    #[serde(default)]
    pub callbacks: RtmpCallbackConfig,
    pub applications: Vec<RtmpApplication>,
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

impl Default for RtmpFanoutPolicy {
    fn default() -> Self {
        default_rtmp_fanout_policy()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardProxyService {
    pub name: String,
    #[serde(default = "default_forward_http_versions")]
    pub enabled_versions: Vec<ForwardHttpVersion>,
    #[serde(default = "default_true")]
    pub allow_absolute_form: bool,
    #[serde(default = "default_true")]
    pub tls_required: bool,
    #[serde(default)]
    pub connect: ForwardConnectPolicy,
    #[serde(default)]
    pub auth: Option<ForwardProxyAuth>,
    #[serde(default)]
    pub access_policy: Option<ForwardAccessPolicy>,
    #[serde(default)]
    pub destination_policy: ForwardDestinationPolicy,
    #[serde(default)]
    pub header_policy: ForwardHeaderPolicy,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    #[serde(default = "default_forward_lifetime_timeout_ms")]
    pub lifetime_timeout_ms: u64,
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: Option<u64>,
    #[serde(default = "default_forward_max_header_bytes")]
    pub max_header_bytes: u64,
    #[serde(default = "default_forward_max_connections")]
    pub max_connections: u64,
    #[serde(default)]
    pub resolver: ForwardResolverPolicy,
    #[serde(default)]
    pub audit_mode: ForwardAuditMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ForwardHttpVersion {
    H1,
    H2,
    H3,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardConnectPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_forward_connect_ports")]
    pub allowed_ports: Vec<u16>,
}

impl Default for ForwardConnectPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_ports: default_forward_connect_ports(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ForwardProxyAuth {
    BearerTokenFile {
        token_file_path: PathBuf,
    },
    BasicHtpasswdFile {
        htpasswd_file_path: PathBuf,
        realm: String,
        #[serde(default)]
        credential_ttl_ms: Option<u64>,
        #[serde(default = "default_true")]
        username_case_sensitive: bool,
    },
    /// Reserved for a future listener TLS client-certificate verifier integration.
    MutualTls {
        client_ca_file_path: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardAccessPolicy {
    #[serde(default)]
    pub rules: Vec<ForwardAccessRule>,
    #[serde(default)]
    pub default_action: ForwardAccessAction,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardAccessRule {
    pub action: ForwardAccessAction,
    #[serde(default)]
    pub conditions: Vec<ForwardAccessCondition>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardAccessAction {
    Allow,
    #[default]
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ForwardAccessCondition {
    #[serde(default)]
    pub negated: bool,
    #[serde(flatten)]
    pub matcher: ForwardAccessMatcher,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ForwardAccessMatcher {
    All,
    Methods { methods: Vec<String> },
    SourceCidrs { cidrs: Vec<String> },
    DestinationPorts { ranges: Vec<ForwardPortRange> },
    Authenticated,
    DestinationLocal,
    DestinationLinkLocal,
    Manager,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardPortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardHeaderPolicy {
    #[serde(default)]
    pub forwarded_for: ForwardedForPolicy,
    #[serde(default)]
    pub via: ForwardViaPolicy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedForPolicy {
    #[default]
    Preserve,
    Delete,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardViaPolicy {
    #[default]
    Preserve,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardDestinationPolicy {
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
    pub allow_times: Vec<ForwardTimeRange>,
    #[serde(default)]
    pub deny_times: Vec<ForwardTimeRange>,
}

impl Default for ForwardDestinationPolicy {
    fn default() -> Self {
        Self {
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            allow_cidrs: Vec::new(),
            deny_cidrs: Vec::new(),
            deny_private: true,
            allow_times: Vec::new(),
            deny_times: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardTimeRange {
    pub days: Vec<ForwardWeekday>,
    pub start: String,
    pub end: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ForwardWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardResolverPolicy {
    #[serde(default)]
    pub nameservers: Vec<IpAddr>,
    #[serde(default = "default_forward_resolver_cache_entries")]
    pub max_cache_entries: u64,
    #[serde(default = "default_forward_resolver_concurrent_queries")]
    pub max_concurrent_queries: u64,
    #[serde(default = "default_forward_resolver_max_addresses")]
    pub max_addresses_per_name: u64,
    #[serde(default = "default_forward_resolver_min_ttl_ms")]
    pub min_ttl_ms: u64,
    #[serde(default = "default_forward_resolver_max_ttl_ms")]
    pub max_ttl_ms: u64,
    #[serde(default = "default_forward_resolver_negative_ttl_ms")]
    pub negative_ttl_ms: u64,
    #[serde(default = "default_true")]
    pub revalidate_on_connect: bool,
}

impl Default for ForwardResolverPolicy {
    fn default() -> Self {
        Self {
            nameservers: Vec::new(),
            max_cache_entries: default_forward_resolver_cache_entries(),
            max_concurrent_queries: default_forward_resolver_concurrent_queries(),
            max_addresses_per_name: default_forward_resolver_max_addresses(),
            min_ttl_ms: default_forward_resolver_min_ttl_ms(),
            max_ttl_ms: default_forward_resolver_max_ttl_ms(),
            negative_ttl_ms: default_forward_resolver_negative_ttl_ms(),
            revalidate_on_connect: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardAuditMode {
    Off,
    #[default]
    Metadata,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct L4Service {
    pub name: String,
    pub upstream_pool: String,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    #[serde(default)]
    pub lifetime_timeout_ms: Option<u64>,
    /// Optional bounded policy used when this service is attached to a UDP listener.
    #[serde(default)]
    pub udp: Option<UdpPolicy>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UdpPolicy {
    #[serde(default = "default_udp_max_datagram_bytes")]
    pub max_datagram_bytes: u64,
    #[serde(default = "default_udp_max_sessions")]
    pub max_sessions: u64,
    #[serde(default = "default_udp_max_session_bytes")]
    pub max_session_bytes: u64,
    #[serde(default = "default_udp_max_queue_datagrams")]
    pub max_queue_datagrams: u64,
    #[serde(default = "default_udp_max_queue_bytes")]
    pub max_queue_bytes: u64,
}

impl Default for UdpPolicy {
    fn default() -> Self {
        Self {
            max_datagram_bytes: default_udp_max_datagram_bytes(),
            max_sessions: default_udp_max_sessions(),
            max_session_bytes: default_udp_max_session_bytes(),
            max_queue_datagrams: default_udp_max_queue_datagrams(),
            max_queue_bytes: default_udp_max_queue_bytes(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Lua configuration failed: {0}")]
    Lua(#[from] mlua::Error),
    #[error("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")]
    SourceTooLarge,
    #[error("unsupported configuration version {0}; expected version 1")]
    UnsupportedVersion(u32),
    #[error("{namespace} at index {index} has a blank name")]
    BlankName {
        namespace: &'static str,
        index: usize,
    },
    #[error("{namespace} at index {index} has noncanonical name {name:?}")]
    InvalidName {
        namespace: &'static str,
        index: usize,
        name: String,
    },
    #[error("duplicate {namespace} name `{name}`")]
    DuplicateName {
        namespace: &'static str,
        name: String,
    },
    #[error("configuration exceeds the {MAX_CERTIFICATES}-certificate limit")]
    TooManyCertificates,
    #[error("certificate `{certificate}` must declare at least one DNS name")]
    EmptyCertificateDnsNames { certificate: String },
    #[error("certificate `{certificate}` exceeds the {MAX_CERTIFICATE_DNS_NAMES}-DNS-name limit")]
    TooManyCertificateDnsNames { certificate: String },
    #[error("certificate `{certificate}` has invalid DNS name `{dns_name}`")]
    InvalidCertificateDnsName {
        certificate: String,
        dns_name: String,
    },
    #[error("certificate `{certificate}` contains duplicate DNS name `{dns_name}`")]
    DuplicateCertificateDnsName {
        certificate: String,
        dns_name: String,
    },
    #[error("configuration exceeds the {MAX_TLS_PROFILES}-TLS-profile limit")]
    TooManyTlsProfiles,
    #[error("{kind} `{name}` has invalid `{field}`: {detail}")]
    InvalidFilePath {
        kind: &'static str,
        name: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("certificate `{certificate}` must use different chain and private-key paths")]
    DuplicateCertificatePaths { certificate: String },
    #[error("certificate `{certificate}` must use different Certbot live and archive directories")]
    DuplicateCertbotDirectories { certificate: String },
    #[error("managed ACME certificate `{certificate}` has an invalid HTTPS directory URL")]
    InvalidAcmeDirectoryUrl { certificate: String },
    #[error("managed ACME certificate `{certificate}` must explicitly agree to directory terms")]
    AcmeTermsNotAgreed { certificate: String },
    #[error("managed ACME certificate `{certificate}` uses an unsupported challenge type")]
    UnsupportedAcmeChallenge { certificate: String },
    #[error("managed ACME certificate `{certificate}` has invalid contacts")]
    InvalidAcmeContacts { certificate: String },
    #[error(
        "managed ACME certificate `{certificate}` must configure between one and sixteen DNS suffixes"
    )]
    InvalidAcmeDnsSuffixes { certificate: String },
    #[error("managed ACME certificate `{certificate}` contains a wildcard or IP identifier")]
    AcmeIdentifierUnsupported { certificate: String },
    #[error("managed ACME certificate `{certificate}` name must be a path-safe slug")]
    InvalidAcmeCertificateName { certificate: String },
    #[error(
        "managed ACME certificate `{certificate}` DNS name `{dns_name}` is outside its suffix policy"
    )]
    AcmeIdentifierOutsidePolicy {
        certificate: String,
        dns_name: String,
    },
    #[error(
        "development certificate `{certificate}` validity_days must be between {min} and {max}, got {value}"
    )]
    InvalidSelfSignedValidityDays {
        certificate: String,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("TLS profile `{profile}` references unknown certificate `{certificate}`")]
    UnknownTlsProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error("TLS profile `{profile}` must reference at least one certificate")]
    EmptyTlsProfileCertificates { profile: String },
    #[error("TLS profile `{profile}` references certificate `{certificate}` more than once")]
    DuplicateTlsProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` default certificate `{certificate}` is not in its certificate list"
    )]
    TlsProfileDefaultNotListed {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` assigns DNS name `{dns_name}` to both `{first_certificate}` and `{second_certificate}`"
    )]
    OverlappingTlsProfileDnsName {
        profile: String,
        dns_name: String,
        first_certificate: String,
        second_certificate: String,
    },
    #[error(
        "TLS profile `{profile}` has invalid ALPN policy; expected [http/1.1], [h2], [h2, http/1.1], or [h3]"
    )]
    InvalidTlsProfileAlpn { profile: String },
    #[error("TLS profile `{profile}` has invalid `{field}` policy: {detail}")]
    InvalidTlsProfilePolicy {
        profile: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("binds `{first_name}` ({first_bind}) and `{second_name}` ({second_bind}) overlap")]
    OverlappingBind {
        first_name: String,
        first_bind: Box<ListenerBind>,
        second_name: String,
        second_bind: Box<ListenerBind>,
    },
    #[error("{kind} `{name}` has an invalid zero port in `{field}`")]
    ZeroPort {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` must have a nonzero `{field}`")]
    ZeroLimit {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` exceeds the exact JSON integer limit in `{field}`")]
    LimitTooLarge {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{protocol:?} listener `{listener}` requires a service")]
    MissingListenerService {
        listener: String,
        protocol: Protocol,
    },
    #[error("{protocol:?} listener `{listener}` references unknown same-kind service `{service}`")]
    UnknownListenerService {
        listener: String,
        protocol: Protocol,
        service: String,
    },
    #[error("listener `{listener}` references unknown TLS profile `{profile}`")]
    UnknownListenerTlsProfile { listener: String, profile: String },
    #[error("{protocol:?} listener `{listener}` must not use TLS profile `{profile}`")]
    UnexpectedListenerTlsProfile {
        listener: String,
        protocol: Protocol,
        profile: String,
    },
    #[error("{protocol:?} listener `{listener}` has invalid transport: {detail}")]
    InvalidListenerTransport {
        listener: String,
        protocol: Protocol,
        detail: &'static str,
    },
    #[error(
        "listener `{listener}` has invalid Unix socket mode {mode:o}; expected permission bits from 001 through 777"
    )]
    InvalidListenerUnixMode { listener: String, mode: u16 },
    #[error("upstream pool `{pool}` must contain at least one endpoint")]
    EmptyUpstreamEndpoints { pool: String },
    #[error("upstream pool `{pool}` exceeds the {MAX_ENDPOINTS_PER_POOL}-endpoint limit")]
    TooManyUpstreamEndpoints { pool: String },
    #[error("configuration exceeds the {MAX_TOTAL_ENDPOINTS}-upstream-endpoint limit")]
    TooManyTotalUpstreamEndpoints,
    #[error("upstream pool `{pool}` contains duplicate endpoint `{endpoint}`")]
    DuplicateUpstreamEndpoint {
        pool: String,
        endpoint: UpstreamEndpoint,
    },
    #[error("upstream pool `{pool}` server `{server}` has invalid `{field}`: {detail}")]
    InvalidUpstreamServer {
        pool: String,
        server: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("upstream pool `{pool}` has invalid weighted round-robin weights: {detail}")]
    InvalidUpstreamWeights { pool: String, detail: &'static str },
    #[error("upstream pool `{pool}` exposes the loopback management endpoint `{endpoint}`")]
    ManagementUpstreamEndpoint { pool: String, endpoint: SocketAddr },
    #[error("upstream pool `{pool}` has invalid DNS endpoint `{host}`")]
    InvalidDnsEndpoint { pool: String, host: String },
    #[error("{kind} `{name}` has invalid Unix socket `{field}`: {detail}")]
    InvalidUnixPath {
        kind: &'static str,
        name: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("upstream pool `{pool}` has an invalid health check: {detail}")]
    InvalidHealthCheck { pool: String, detail: &'static str },
    #[error("upstream pool `{pool}` has invalid TLS server name `{server_name}`")]
    InvalidUpstreamTlsServerName { pool: String, server_name: String },
    #[error(
        "upstream pool `{pool}` has invalid HTTP version range {min}/{max}; expected 1.1/1.1, 1.1/2, or 2/2"
    )]
    InvalidHttpVersionRange {
        pool: String,
        min: &'static str,
        max: &'static str,
    },
    #[error("upstream pool `{pool}` enables HTTP/2 without TLS; plaintext h2c is not supported")]
    H2RequiresUpstreamTls { pool: String },
    #[error("upstream pool `{pool}` combines `health_check` with `tls`, which is not supported")]
    UnsupportedTlsHealthCheck { pool: String },
    #[error("listener `{listener}` cannot terminate TLS profile `{profile}` on a Unix socket")]
    UnsupportedUnixListenerTls { listener: String, profile: String },
    #[error("upstream pool `{pool}` cannot use TLS with a Unix endpoint")]
    UnsupportedUnixUpstreamTls { pool: String },
    #[error("upstream pool `{pool}` cannot health-check a Unix endpoint")]
    UnsupportedUnixHealthCheck { pool: String },
    #[error("HTTP service `{service}` must contain at least one route")]
    EmptyHttpRoutes { service: String },
    #[error("HTTP service `{service}` route {route} has invalid `{field}`: {detail}")]
    InvalidHttpRoute {
        service: String,
        route: usize,
        field: &'static str,
        detail: String,
    },
    #[error(
        "HTTP service `{service}` route {route} uses endpoint Host policy with a Unix endpoint but has no `unix_fallback`"
    )]
    HttpEndpointHostRequiresUnixFallback { service: String, route: usize },
    #[error("configuration exceeds the {MAX_RTMP_SERVICES}-RTMP-service limit")]
    TooManyRtmpServices,
    #[error("RTMP service `{service}` must contain at least one application")]
    EmptyRtmpApplications { service: String },
    #[error(
        "RTMP service `{service}` exceeds the {MAX_RTMP_APPLICATIONS_PER_SERVICE}-application limit"
    )]
    TooManyRtmpApplications { service: String },
    #[error(
        "RTMP application `{application}` in service `{service}` exceeds the {MAX_RTMP_RECORDERS_PER_APPLICATION}-recorder limit"
    )]
    TooManyRtmpRecorders {
        service: String,
        application: String,
    },
    #[error("configuration exceeds the {MAX_TOTAL_RTMP_RECORDERS}-RTMP-recorder limit")]
    TooManyTotalRtmpRecorders,
    #[error("configuration exceeds the {MAX_RTMP_RECORDING_ROOTS}-recording-root limit")]
    TooManyRtmpRecordingRoots,
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` requires `live = true`"
    )]
    RtmpRecorderRequiresLiveApplication {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` has invalid `{field}`: {detail}"
    )]
    InvalidRtmpRecorderPolicy {
        service: String,
        application: String,
        recorder: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("RTMP service `{service}` has invalid `{field}`: {detail}")]
    InvalidRtmpServicePolicy {
        service: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error(
        "RTMP application `{application}` in service `{service}` has invalid `{field}`: {detail}"
    )]
    InvalidRtmpApplicationPolicy {
        service: String,
        application: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error(
        "RTMP application `{application}` in service `{service}` has duplicate {operation} ACL rule `{network}`"
    )]
    DuplicateRtmpAccessRule {
        service: String,
        application: String,
        operation: &'static str,
        network: String,
    },
    #[error(
        "RTMP recorder `{recorder}` max_queue_bytes must not exceed max_storage_bytes in application `{application}` of service `{service}`"
    )]
    RtmpRecorderQueueExceedsStorage {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorders `{first_recorder}` and `{second_recorder}` use shared recording root `{root_directory}` and must use identical storage limits"
    )]
    RtmpRecorderStorageLimitsMismatch {
        root_directory: String,
        first_recorder: String,
        second_recorder: String,
    },
    #[error(
        "HTTP service `{service}` routes {first_route} and {duplicate_route} have equivalent matchers"
    )]
    DuplicateHttpRoute {
        service: String,
        first_route: usize,
        duplicate_route: usize,
    },
    #[error("HTTP service `{service}` route {route} references unknown upstream pool `{pool}`")]
    UnknownRouteUpstreamPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error("cache store `{store}` has invalid `{field}`: {detail}")]
    InvalidCacheStore {
        store: String,
        field: &'static str,
        detail: String,
    },
    #[error("HTTP service `{service}` route {route} references unknown cache store `{store}`")]
    UnknownCacheStore {
        service: String,
        route: usize,
        store: String,
    },
    #[error("HTTP service `{service}` route {route} has invalid cache `{field}`: {detail}")]
    InvalidCachePolicy {
        service: String,
        route: usize,
        field: &'static str,
        detail: String,
    },
    #[error("forward proxy service `{service}` has invalid `{field}`: {detail}")]
    InvalidForwardProxyService {
        service: String,
        field: &'static str,
        detail: String,
    },
    #[error("forward proxy listener `{listener}` has invalid configuration: {detail}")]
    InvalidForwardProxyListener { listener: String, detail: String },
    #[error("L4 service `{service}` references unknown upstream pool `{pool}`")]
    UnknownL4UpstreamPool { service: String, pool: String },
    #[error("L4 service `{service}` references TLS-enabled upstream pool `{pool}`")]
    TlsUpstreamPoolForL4Service { service: String, pool: String },
    #[error("L4 service `{service}` has invalid UDP policy `{field}`: {detail}")]
    InvalidL4UdpPolicy {
        service: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("management listener must use loopback, got `{0}`")]
    ManagementMustUseLoopback(SocketAddr),
    #[error(
        "statistics must configure between one and eight total unique IPv4/IPv6 listener binds"
    )]
    InvalidStatsBinds,
    #[error("statistics page {page} has invalid `{field}`: {detail}")]
    InvalidStatsPage {
        page: usize,
        field: &'static str,
        detail: &'static str,
    },
}
