use super::*;

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
        #[serde(default = "default_acme_retained_revisions")]
        retained_revisions: u32,
        #[serde(default = "default_acme_retention_days")]
        retention_days: u32,
        #[serde(default)]
        dns01: Option<AcmeDns01Config>,
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcmeDns01Config {
    pub provider: String,
    pub credential_file: PathBuf,
    #[serde(default = "default_acme_dns01_timeout_seconds")]
    pub timeout_seconds: u64,
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
    pub client_auth: TlsClientAuthPolicy,
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
            client_auth: TlsClientAuthPolicy::default(),
            session_cache: None,
            session_timeout_seconds: None,
            session_tickets: false,
            prefer_server_ciphers: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsClientAuthMode {
    #[default]
    Disabled,
    Optional,
    Required,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsClientAuthPolicy {
    #[serde(default)]
    pub mode: TlsClientAuthMode,
    #[serde(default)]
    pub ca_certificate_path: Option<PathBuf>,
    #[serde(default)]
    pub allowed_dns_names: Vec<String>,
}

impl Default for TlsClientAuthPolicy {
    fn default() -> Self {
        Self {
            mode: TlsClientAuthMode::Disabled,
            ca_certificate_path: None,
            allowed_dns_names: Vec::new(),
        }
    }
}

impl fmt::Debug for TlsClientAuthPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsClientAuthPolicy")
            .field("mode", &self.mode)
            .field(
                "ca_certificate_configured",
                &self.ca_certificate_path.is_some(),
            )
            .field("allowed_dns_name_count", &self.allowed_dns_names.len())
            .finish()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStoreKind {
    Memory,
    Disk,
}

#[derive(Clone, Copy, Debug)]
pub struct CacheStoreCommon<'a> {
    pub name: &'a str,
    pub max_bytes: u64,
    pub max_entries: u64,
    pub max_object_bytes: u64,
    pub max_header_bytes: u64,
    pub max_key_bytes: u64,
    pub max_tag_bytes: u64,
    pub max_tags_per_object: u64,
    pub max_in_flight_fills: u64,
    pub max_followers_per_fill: u64,
}

impl CacheStore {
    #[must_use]
    pub fn memory(name: impl Into<String>, max_bytes: u64) -> Self {
        Self::Memory {
            name: name.into(),
            max_bytes,
            max_entries: default_cache_max_entries(),
            max_object_bytes: default_cache_max_object_bytes().min(max_bytes),
            max_header_bytes: default_cache_max_header_bytes().min(max_bytes),
            max_key_bytes: default_cache_max_key_bytes(),
            max_tag_bytes: default_cache_max_tag_bytes(),
            max_tags_per_object: default_cache_max_tags_per_object(),
            max_in_flight_fills: default_cache_max_in_flight_fills(),
            max_followers_per_fill: default_cache_max_followers_per_fill(),
        }
    }

    #[must_use]
    pub fn disk(name: impl Into<String>, root_directory: PathBuf, max_bytes: u64) -> Self {
        Self::Disk {
            name: name.into(),
            root_directory,
            max_bytes,
            max_files: default_disk_cache_max_files(),
            max_object_bytes: default_cache_max_object_bytes().min(max_bytes),
            max_header_bytes: default_cache_max_header_bytes().min(max_bytes),
            max_key_bytes: default_cache_max_key_bytes(),
            max_tag_bytes: default_cache_max_tag_bytes(),
            max_tags_per_object: default_cache_max_tags_per_object(),
            max_in_flight_fills: default_cache_max_in_flight_fills(),
            max_followers_per_fill: default_cache_max_followers_per_fill(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> CacheStoreKind {
        match self {
            Self::Memory { .. } => CacheStoreKind::Memory,
            Self::Disk { .. } => CacheStoreKind::Disk,
        }
    }

    #[must_use]
    pub fn common(&self) -> CacheStoreCommon<'_> {
        match self {
            Self::Memory {
                name,
                max_bytes,
                max_entries,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
            } => CacheStoreCommon {
                name,
                max_bytes: *max_bytes,
                max_entries: *max_entries,
                max_object_bytes: *max_object_bytes,
                max_header_bytes: *max_header_bytes,
                max_key_bytes: *max_key_bytes,
                max_tag_bytes: *max_tag_bytes,
                max_tags_per_object: *max_tags_per_object,
                max_in_flight_fills: *max_in_flight_fills,
                max_followers_per_fill: *max_followers_per_fill,
            },
            Self::Disk {
                name,
                max_bytes,
                max_files,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
                ..
            } => CacheStoreCommon {
                name,
                max_bytes: *max_bytes,
                max_entries: *max_files,
                max_object_bytes: *max_object_bytes,
                max_header_bytes: *max_header_bytes,
                max_key_bytes: *max_key_bytes,
                max_tag_bytes: *max_tag_bytes,
                max_tags_per_object: *max_tags_per_object,
                max_in_flight_fills: *max_in_flight_fills,
                max_followers_per_fill: *max_followers_per_fill,
            },
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.common().name
    }

    #[must_use]
    pub fn root_directory(&self) -> Option<&Path> {
        match self {
            Self::Memory { .. } => None,
            Self::Disk { root_directory, .. } => Some(root_directory.as_path()),
        }
    }

    pub(crate) const fn root_directory_mut(&mut self) -> Option<&mut PathBuf> {
        match self {
            Self::Memory { .. } => None,
            Self::Disk { root_directory, .. } => Some(root_directory),
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
    pub passive_health: Option<PassiveHealthPolicy>,
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
pub struct PassiveHealthPolicy {
    #[serde(default)]
    pub observe: PassiveObserve,
    #[serde(default)]
    pub on_error: PassiveOnError,
    #[serde(default = "default_passive_error_limit")]
    pub error_limit: u16,
    #[serde(default)]
    pub mark_down: bool,
    #[serde(default)]
    pub mark_up: bool,
    #[serde(default = "default_passive_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    #[serde(default = "default_passive_max_backoff_ms")]
    pub max_backoff_ms: u64,
    #[serde(default = "default_passive_recovery_threshold")]
    pub recovery_threshold: u16,
}

impl Default for PassiveHealthPolicy {
    fn default() -> Self {
        Self {
            observe: PassiveObserve::default(),
            on_error: PassiveOnError::default(),
            error_limit: default_passive_error_limit(),
            mark_down: false,
            mark_up: false,
            initial_backoff_ms: default_passive_initial_backoff_ms(),
            max_backoff_ms: default_passive_max_backoff_ms(),
            recovery_threshold: default_passive_recovery_threshold(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PassiveObserve {
    Layer4,
    #[default]
    Layer7,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PassiveOnError {
    #[default]
    Count,
    Immediately,
    MarkDown,
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
    #[serde(rename = "3")]
    Http3,
}

impl HttpVersion {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http11 => "1.1",
            Self::Http2 => "2",
            Self::Http3 => "3",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpstreamAlgorithm {
    #[default]
    RoundRobin,
    WeightedRoundRobin {
        weights: Vec<u16>,
    },
    LeastConnections,
    First,
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
