#[allow(clippy::wildcard_imports)]
use super::*;

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
    #[serde(default)]
    pub resilience: PoolResilience,
    /// Orders the requests that wait for capacity by class and tenant; absent keeps first come first served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<PoolQueuePolicy>,
    /// Opts the pool into servers that register themselves at runtime; absent means static only.
    #[serde(default)]
    pub registration: Option<PoolRegistration>,
    /// A synthetic request through the pool at a fixed interval, reported as metrics and by `check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary: Option<PoolCanary>,
}

/// A request a client could make, sent through the pool's own server selection.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolCanary {
    /// Absolute path of the `GET` request, query included.
    pub path: String,
    /// `Host` header the request carries; absent sends none.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default = "default_canary_expected_status")]
    pub expected_status: u16,
    #[serde(default = "default_canary_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "default_canary_timeout_ms")]
    pub timeout_ms: u64,
    /// A correct answer slower than this counts as slow; absent does not judge latency.
    #[serde(default)]
    pub max_latency_ms: Option<u64>,
}

/// How a saturated pool picks the next waiting request. Needs `queue_timeout_ms`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolQueuePolicy {
    /// Priority classes, highest priority first for `strict`; empty means a single class.
    #[serde(default)]
    pub classes: Vec<PoolQueueClass>,
    #[serde(default)]
    pub discipline: QueueDiscipline,
    /// Request header naming the class; ignored by routes that set `queue_class`. Clients control
    /// it unless a trusted layer in front overwrites it.
    #[serde(default)]
    pub class_header: Option<String>,
    /// Class of requests that name none or an unknown one; defaults to the last class.
    #[serde(default)]
    pub default_class: Option<String>,
    /// Request header naming the tenant that shares a class fairly with the other tenants.
    #[serde(default)]
    pub tenant_header: Option<String>,
    /// Distinct tenants tracked; further tenants share one `other` tenant.
    #[serde(default = "default_queue_max_tenants")]
    pub max_tenants: u16,
    /// With `strict`, a request waiting this long is served ahead of higher classes.
    #[serde(default)]
    pub aging_ms: Option<u64>,
}

impl Default for PoolQueuePolicy {
    fn default() -> Self {
        Self {
            classes: Vec::new(),
            discipline: QueueDiscipline::default(),
            class_header: None,
            default_class: None,
            tenant_header: None,
            max_tenants: default_queue_max_tenants(),
            aging_ms: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolQueueClass {
    pub name: String,
    /// Share of the capacity under `weighted`.
    #[serde(default = "default_queue_class_weight")]
    pub weight: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueDiscipline {
    /// Classes share the freed capacity in proportion to their weights.
    #[default]
    Weighted,
    /// A lower class is served only when no higher class waits.
    Strict,
}

/// Limits for servers that register themselves in a pool at runtime.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolRegistration {
    /// Registered servers the pool holds at most, besides its static servers.
    #[serde(default = "default_registration_max_servers")]
    pub max_servers: u16,
    #[serde(default = "default_registration_min_ttl_ms")]
    pub min_ttl_ms: u64,
    #[serde(default = "default_registration_default_ttl_ms")]
    pub default_ttl_ms: u64,
    #[serde(default = "default_registration_max_ttl_ms")]
    pub max_ttl_ms: u64,
    /// Addresses a registration may point at, as `address/prefix`; empty allows any non-special one.
    #[serde(default)]
    pub allowed_cidrs: Vec<String>,
    /// A registering TCP peer may only register its own address.
    #[serde(default = "default_registration_require_peer_address")]
    pub require_peer_address: bool,
}

const fn default_registration_max_servers() -> u16 {
    64
}

const fn default_registration_min_ttl_ms() -> u64 {
    2_000
}

const fn default_registration_default_ttl_ms() -> u64 {
    15_000
}

const fn default_registration_max_ttl_ms() -> u64 {
    3_600_000
}

const fn default_registration_require_peer_address() -> bool {
    true
}

impl Default for PoolRegistration {
    fn default() -> Self {
        Self {
            max_servers: default_registration_max_servers(),
            min_ttl_ms: default_registration_min_ttl_ms(),
            default_ttl_ms: default_registration_default_ttl_ms(),
            max_ttl_ms: default_registration_max_ttl_ms(),
            allowed_cidrs: Vec::new(),
            require_peer_address: default_registration_require_peer_address(),
        }
    }
}

impl PoolRegistration {
    /// Whether `ip` lies inside one of the allowed networks. Malformed entries never match.
    #[must_use]
    pub fn allows(&self, ip: IpAddr) -> bool {
        self.allowed_cidrs
            .iter()
            .any(|cidr| cidr_contains(cidr, ip) == Some(true))
    }
}

/// Whether `ip` is in `cidr` (`address/prefix`); `None` when `cidr` is malformed.
#[must_use]
pub fn cidr_contains(cidr: &str, ip: IpAddr) -> Option<bool> {
    let (network, prefix) = cidr.split_once('/')?;
    let network = network.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u32>().ok()?;
    let shared = |network: u128, ip: u128, bits: u32| {
        let shift = bits.checked_sub(prefix)?;
        Some(shift >= 128 || network >> shift == ip >> shift)
    };
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(ip)) => {
            shared(u32::from(network).into(), u32::from(ip).into(), 32)
        }
        (IpAddr::V6(network), IpAddr::V6(ip)) => shared(network.into(), ip.into(), 128),
        (IpAddr::V4(_), IpAddr::V6(_)) => (prefix <= 32).then_some(false),
        (IpAddr::V6(_), IpAddr::V4(_)) => (prefix <= 128).then_some(false),
    }
}

/// Process-wide registration settings: where registrations persist and who may reach the registry.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Directory that holds `registrations.json`; must not be the configuration directory.
    pub state_dir: PathBuf,
    /// Scoped bearer tokens (`<id> <sha256-hex> read|register pools=a,b`); required by TCP endpoints.
    #[serde(default)]
    pub tokens_file: Option<PathBuf>,
    /// Listeners that serve only the registry routes.
    #[serde(default)]
    pub endpoints: Vec<RegistryEndpoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryEndpoint {
    /// A unix socket whose file mode and group decide who may use `role`.
    Unix {
        path: PathBuf,
        #[serde(default = "default_registry_socket_mode")]
        mode: u16,
        role: RegistryRole,
        /// Pools a `register` socket may touch; empty means every registration pool.
        #[serde(default)]
        pools: Vec<String>,
    },
    /// A TCP listener for LAN workers: bearer tokens plus a peer-address allow list.
    Tcp {
        bind: SocketAddr,
        #[serde(default)]
        allow_cidrs: Vec<String>,
    },
}

const fn default_registry_socket_mode() -> u16 {
    0o660
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegistryRole {
    Read,
    Register,
}

/// Availability options of one pool. Every option defaults to the strict behaviour.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolResilience {
    /// Serve the least-bad member when health or ejection leaves none selectable.
    #[serde(default)]
    pub fail_open: bool,
    /// Passive ejection never leaves fewer selectable servers than this; defaults to 1 with
    /// `fail_open` and 0 without.
    #[serde(default)]
    pub min_members: Option<u16>,
    /// Randomizes each passive ejection duration by up to this many percent either way.
    #[serde(default)]
    pub ejection_jitter_percent: u8,
    /// A request that finds no selectable server waits this long for one before answering 503.
    #[serde(default)]
    pub queue_while_unavailable_ms: Option<u64>,
    /// `Retry-After` seconds sent with the 503 answered when no server can take a request.
    #[serde(default)]
    pub unavailable_retry_after_seconds: Option<u32>,
    /// Plain-text body of that 503; empty when absent.
    #[serde(default)]
    pub unavailable_body: Option<String>,
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
    /// Keeps an ejected server out of rotation until an active probe succeeds, instead of
    /// letting live requests test it when the ejection time ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readmission: Option<Readmission>,
    /// Takes a server out of rotation when too large a share of its recent requests fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker: Option<CircuitBreaker>,
    /// Ejects a server whose time to first byte is far above its peers on the same route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outlier_detection: Option<OutlierDetection>,
}

/// Latency outlier detection of a pool: servers are compared per route, never across routes whose
/// jobs differ in duration.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutlierDetection {
    /// A server is an outlier above this percentage of the median of its peers (300 is three times).
    #[serde(default = "default_outlier_factor_percent")]
    pub factor_percent: u32,
    /// Successful responses per route and server in one interval before a server is judged.
    #[serde(default = "default_outlier_min_samples")]
    pub min_samples: u32,
    #[serde(default = "default_outlier_interval_ms")]
    pub interval_ms: u64,
    /// Upper share of the pool that may be ejected as outliers at once (at least one server).
    #[serde(default = "default_outlier_max_ejected_percent")]
    pub max_ejected_percent: u8,
}

/// Failure-ratio circuit breaker of one server, distinct from consecutive-failure ejection.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreaker {
    /// Sliding window of recent outcomes.
    #[serde(default = "default_breaker_window_ms")]
    pub window_ms: u64,
    /// Requests the window needs before the ratio is judged.
    #[serde(default = "default_breaker_min_requests")]
    pub min_requests: u32,
    #[serde(default = "default_breaker_failure_ratio_percent")]
    pub failure_ratio_percent: u8,
    /// How long the server stays out before trial requests are allowed.
    #[serde(default = "default_breaker_open_ms")]
    pub open_ms: u64,
    /// Concurrent trial requests while half-open.
    #[serde(default = "default_breaker_half_open_max")]
    pub half_open_max: u16,
    /// Consecutive trial successes that close the breaker.
    #[serde(default = "default_breaker_half_open_successes")]
    pub half_open_successes: u16,
}

/// Probe-based re-admission of a passively ejected server.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Readmission {
    /// Consecutive successful probes that put the server back into rotation.
    #[serde(default = "default_readmission_successes")]
    pub successes: u16,
    #[serde(default = "default_readmission_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "default_readmission_timeout_ms")]
    pub timeout_ms: u64,
    /// An HTTP probe; absent means a TCP connect.
    #[serde(default)]
    pub probe: Option<ReadmissionProbe>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadmissionProbe {
    pub path: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub expected_status: Option<u16>,
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
            readmission: None,
            circuit_breaker: None,
            outlier_detection: None,
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
    /// SNI and certificate hostname; required with `verify = full`, absent sends no SNI.
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_verify")]
    pub verify: UpstreamTlsVerify,
    #[serde(default)]
    pub ca_certificate_path: Option<PathBuf>,
}

/// How the origin certificate is judged.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamTlsVerify {
    /// Chain and hostname are verified against the system roots or `ca_certificate_path`.
    #[default]
    Full,
    /// Encrypts only: any certificate is accepted, as nginx does without `proxy_ssl_verify`.
    None,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_verify(verify: &UpstreamTlsVerify) -> bool {
    *verify == UpstreamTlsVerify::Full
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
    PowerOfTwoChoices,
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
            Self::PowerOfTwoChoices => serializer.serialize_str("power_of_two_choices"),
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
                "power_of_two_choices" => Ok(Self::PowerOfTwoChoices),
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
