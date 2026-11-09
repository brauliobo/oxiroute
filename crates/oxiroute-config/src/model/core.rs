#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigDraft {
    pub version: u32,
    /// Aggregate process admission cap. Omitted or explicit null means unbounded.
    #[serde(default)]
    pub max_connections: Option<u64>,
    /// Worker threads that serve each HTTP, forward HTTP/1, forward HTTP/2 and TCP listener.
    /// The workers share the listener's socket, pools and limits. Omitted means one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener_workers: Option<u64>,
    /// Bounds on how long and how many replaced generations keep serving their in-flight work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement: Option<Retirement>,
    #[serde(default)]
    pub management: Option<Management>,
    /// Runtime server registration: state directory, tokens, and registry-only endpoints.
    #[serde(default)]
    pub registry: Option<Registry>,
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

impl ConfigDraft {
    pub const MAX_LISTENER_WORKERS: u64 = 256;
}

/// Lifetime of a replaced generation. Its accept gate closes at once; work already admitted
/// continues until `drain_deadline_ms` has passed, then the generation is closed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Retirement {
    /// Defaults to three hours or the longest HTTP `upstream_io_timeout_ms`, whichever is larger.
    #[serde(default)]
    pub drain_deadline_ms: Option<u64>,
    /// Replaced generations that may drain at once; a further apply waits for a free slot.
    #[serde(default = "default_max_retired_generations")]
    pub max_retired_generations: u64,
}

impl Retirement {
    pub const DEFAULT_MAX_RETIRED_GENERATIONS: u64 = 4;
}

impl Default for Retirement {
    fn default() -> Self {
        Self {
            drain_deadline_ms: None,
            max_retired_generations: default_max_retired_generations(),
        }
    }
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
    /// Optional PROXY protocol header accepted before application data.
    #[serde(default)]
    pub proxy_protocol: Option<ProxyProtocolPolicy>,
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
    Http3,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocolVersion {
    V1,
    V2,
    #[default]
    Auto,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ProxyProtocolPolicy {
    pub version: ProxyProtocolVersion,
    #[serde(default = "default_proxy_protocol_timeout_ms")]
    pub timeout_ms: u64,
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
    /// Optional PROXY protocol header sent to the selected upstream.
    #[serde(default)]
    pub proxy_protocol: Option<ProxyProtocolPolicy>,
    /// Optional bounded policy used when this service is attached to a UDP listener.
    #[serde(default)]
    pub udp: Option<UdpPolicy>,
    /// Raises the generation drain deadline while this service is configured; never lowers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_deadline_ms: Option<u64>,
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
