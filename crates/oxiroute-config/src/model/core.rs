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
    /// Whether plain HTTP/1.1 requests of fixed-response and plain proxy routes are served on the compiled fast
    /// lane, which skips the session machinery for requests it can prove the classic path answers the same way.
    /// Omitted means on; `false` sends every request through the classic path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_fast_lane: Option<bool>,
    /// How each listener worker orders the connections that are ready: `work_stealing` (the default) shares them
    /// between the workers of a listener; `aged_per_worker` gives every worker its own queue, served newest first
    /// and oldest first once a connection has waited `connection_age_ms`, by default a few passes over the ready connections;
    /// `auto` is `aged_per_worker` from two `listener_workers` up and `work_stealing` with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_scheduling: Option<ConnectionScheduling>,
    /// Longest a ready connection waits behind newer ones with `aged_per_worker`. Omitted means a multiple of the time one
    /// pass over the worker's ready connections takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_age_ms: Option<u64>,
    /// With `dynamic` (and `aged_per_worker`) a listener keeps only as many of its `listener_workers` workers
    /// active as the offered load needs, placing new connections on, and moving idle keep-alive HTTP/1 connections
    /// to, the active ones, and activates more under pressure; `static` (the default) spreads connections over all
    /// workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_scaling: Option<WorkerScaling>,
    /// Fewest workers kept active with `dynamic` scaling, at most `listener_workers`. Omitted means one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_active_workers: Option<u64>,
    /// Share of one worker's time the active workers are kept under, in percent, before another worker is
    /// activated. Omitted means 40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_busy_percent: Option<u64>,
    /// How long the load must fit in fewer workers before one is released, in milliseconds. Omitted means 5000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_down_delay_ms: Option<u64>,
    /// Threads that run the TLS handshakes of every TLS listener, away from the workers that serve connections.
    /// Omitted means one thread per 8 CPUs (1 to 4); `0` handshakes on the workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_handshake_threads: Option<u64>,
    /// Bounds on how long and how many replaced generations keep serving their in-flight work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement: Option<Retirement>,
    #[serde(default)]
    pub management: Option<Management>,
    /// Log verbosity in `RUST_LOG` syntax without regex filters: a level (`off`, `error`, `warn`,
    /// `info`, `debug`, `trace`) and `module::path=level` directives separated by commas. Omitted
    /// means the `RUST_LOG` environment variable, then errors only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_filter: Option<String>,
    /// Durable audit log of management operations: location and retention bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<Audit>,
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
    pub const MAX_CONNECTION_AGE_MS: u64 = 1_000;
    pub const MAX_MIN_ACTIVE_WORKERS: u64 = 256;
    pub const MIN_TARGET_BUSY_PERCENT: u64 = 10;
    pub const MAX_TARGET_BUSY_PERCENT: u64 = 90;
    pub const MIN_SCALE_DOWN_DELAY_MS: u64 = 100;
    pub const MAX_SCALE_DOWN_DELAY_MS: u64 = 3_600_000;
    pub const DEFAULT_MIN_ACTIVE_WORKERS: u64 = 1;
    pub const DEFAULT_TARGET_BUSY_PERCENT: u64 = 40;
    pub const DEFAULT_SCALE_DOWN_DELAY_MS: u64 = 5_000;
    pub const MAX_TLS_HANDSHAKE_THREADS: u64 = 64;
    pub const MAX_LOG_FILTER_BYTES: usize = 1024;

    /// Whether the HTTP/1.1 fast lane serves the requests it can: on unless `http_fast_lane` says `false`.
    #[must_use]
    pub fn fast_lane_enabled(&self) -> bool {
        self.http_fast_lane.unwrap_or(true)
    }

    /// The handshake threads for a host with `cpus` CPUs: the configured count, or one per 8 CPUs between 1 and 4.
    #[must_use]
    pub fn effective_tls_handshake_threads(&self, cpus: usize) -> usize {
        self.tls_handshake_threads.map_or_else(
            || cpus.div_ceil(8).clamp(1, 4),
            |threads| usize::try_from(threads).unwrap_or(usize::MAX),
        )
    }

    /// The scheduling the workers of a listener run: the configured one, with `auto` resolved from the worker count.
    #[must_use]
    pub fn effective_connection_scheduling(&self) -> ConnectionScheduling {
        match self.connection_scheduling {
            Some(ConnectionScheduling::AgedPerWorker) => ConnectionScheduling::AgedPerWorker,
            Some(ConnectionScheduling::Auto) if self.listener_workers.unwrap_or(1) >= 2 => {
                ConnectionScheduling::AgedPerWorker
            }
            _ => ConnectionScheduling::WorkStealing,
        }
    }

    /// Whether the workers of a listener serve per-worker queues.
    #[must_use]
    pub fn aged_per_worker(&self) -> bool {
        self.effective_connection_scheduling() == ConnectionScheduling::AgedPerWorker
    }

    /// Whether a listener keeps only the workers its load needs active.
    #[must_use]
    pub fn dynamic_worker_scaling(&self) -> bool {
        self.worker_scaling == Some(WorkerScaling::Dynamic)
    }

    #[must_use]
    pub fn effective_min_active_workers(&self) -> u64 {
        self.min_active_workers
            .unwrap_or(Self::DEFAULT_MIN_ACTIVE_WORKERS)
    }

    #[must_use]
    pub fn effective_target_busy_percent(&self) -> u64 {
        self.target_busy_percent
            .unwrap_or(Self::DEFAULT_TARGET_BUSY_PERCENT)
    }

    #[must_use]
    pub fn effective_scale_down_delay_ms(&self) -> u64 {
        self.scale_down_delay_ms
            .unwrap_or(Self::DEFAULT_SCALE_DOWN_DELAY_MS)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionScheduling {
    WorkStealing,
    AgedPerWorker,
    /// Per-worker queues when a listener has two workers or more, work stealing with one.
    Auto,
}

impl ConnectionScheduling {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkStealing => "work_stealing",
            Self::AgedPerWorker => "aged_per_worker",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerScaling {
    Static,
    Dynamic,
}

impl WorkerScaling {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
        }
    }
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

/// Where the audit log lives and how much of it is retained. Every omitted setting keeps its
/// default; `OXIROUTE_AUDIT_*` environment variables fill the omitted settings for deployments
/// that predate this block.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    /// Directory of the durable audit files. Omitted means an `audit` directory next to the
    /// management token file, or in-memory records when there is no token file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Records retained (default 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_records: Option<u64>,
    /// Largest single record in bytes (default 16384).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_record_bytes: Option<u64>,
    /// Size at which the active file rotates (default 1 MiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,
    /// Bytes retained across the active and rotated files (default 8 MiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_bytes: Option<u64>,
    /// Rotated files kept besides the active file (default 7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rotated_files: Option<u64>,
}

impl Audit {
    pub const DEFAULT_MAX_RECORDS: u64 = 10_000;
    pub const DEFAULT_MAX_RECORD_BYTES: u64 = 16 * 1024;
    pub const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
    pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
    pub const DEFAULT_MAX_ROTATED_FILES: u64 = 7;
    pub const MAX_RECORDS: u64 = 100_000;
    pub const MAX_RECORD_BYTES: u64 = 64 * 1024;
    pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
    pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
    pub const MAX_ROTATED_FILES: u64 = 7;
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Management {
    pub bind: SocketAddr,
    #[serde(default)]
    pub ui_dir: Option<PathBuf>,
    /// File that holds the bearer token for the management API. The token itself is never part
    /// of the configuration. Omitted means `OXIROUTE_MANAGEMENT_TOKEN_FILE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
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
    /// HTTP/2 receive window of each request stream, in bytes (`http` and `forward_http2` listeners). It caps what a
    /// client may send on one stream before the proxy has read it. Omitted means 1 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_initial_stream_window: Option<u64>,
    /// HTTP/2 receive window of the whole connection, in bytes (`http` and `forward_http2` listeners). It caps the
    /// request data one connection can have buffered, whatever the number of streams. Omitted means 8 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_initial_connection_window: Option<u64>,
}

impl Listener {
    pub const MIN_H2_WINDOW: u64 = 65_535;
    pub const DEFAULT_H2_STREAM_WINDOW: u64 = 1 << 20;
    pub const DEFAULT_H2_CONNECTION_WINDOW: u64 = 8 << 20;
    pub const MAX_H2_STREAM_WINDOW: u64 = 16 << 20;
    pub const MAX_H2_CONNECTION_WINDOW: u64 = 64 << 20;

    /// The stream and connection receive windows this listener advertises over HTTP/2.
    ///
    /// # Panics
    ///
    /// A window that validation would have refused (above `u32::MAX`).
    #[must_use]
    pub fn effective_h2_windows(&self) -> H2Windows {
        let window = |configured: Option<u64>, default: u64| {
            u32::try_from(configured.unwrap_or(default)).expect("validated window fits u32")
        };
        H2Windows {
            stream: window(
                self.h2_initial_stream_window,
                Self::DEFAULT_H2_STREAM_WINDOW,
            ),
            connection: window(
                self.h2_initial_connection_window,
                Self::DEFAULT_H2_CONNECTION_WINDOW,
            ),
        }
    }
}

/// HTTP/2 receive windows in bytes, as advertised to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H2Windows {
    pub stream: u32,
    pub connection: u32,
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
