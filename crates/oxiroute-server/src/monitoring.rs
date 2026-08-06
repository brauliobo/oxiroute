use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    error::Error,
    fmt, io,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

#[cfg(target_os = "linux")]
use std::fs;

use serde::Serialize;

use oxiroute_config::ListenerBind;

use crate::{
    AcmeManagedReconciler, AdministrativeState, CertbotReconciler, CertbotWatcherMonitor,
    FileReconciler, FileWatcherMonitor, PoolHealthSnapshot, ProxyProtocolResult, RoundRobinPool,
};

#[derive(Debug)]
pub enum MetricsError {
    DuplicateListener(String),
    ListenerNotFound(String),
    InvalidListenerField(&'static str),
    InvalidListenerBind {
        listener: String,
        detail: &'static str,
    },
    ConnectionLimitReached {
        listener: String,
        limit: u64,
    },
    ProcessConnectionLimitReached {
        limit: u64,
    },
    AdministrativeDrain {
        resource: &'static str,
        name: String,
    },
    CounterOverflow(&'static str),
    StatePoisoned(&'static str),
    UnsupportedPlatform(&'static str),
    Io {
        path: &'static str,
        source: io::Error,
    },
    InvalidData {
        resource: &'static str,
        detail: String,
    },
    SystemClockBeforeUnixEpoch,
    ValueOutOfRange(&'static str),
}

impl fmt::Display for MetricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateListener(name) => {
                write!(formatter, "listener `{name}` is already registered")
            }
            Self::ListenerNotFound(name) => {
                write!(formatter, "listener `{name}` is not registered")
            }
            Self::InvalidListenerField(field) => {
                write!(formatter, "listener {field} must not be empty")
            }
            Self::InvalidListenerBind { listener, detail } => {
                write!(
                    formatter,
                    "listener `{listener}` has an invalid bind: {detail}"
                )
            }
            Self::ConnectionLimitReached { listener, limit } => {
                write!(
                    formatter,
                    "listener `{listener}` reached its {limit}-connection limit"
                )
            }
            Self::ProcessConnectionLimitReached { limit } => {
                write!(formatter, "process reached its {limit}-connection limit")
            }
            Self::AdministrativeDrain { resource, name } => {
                write!(
                    formatter,
                    "{resource} `{name}` is not accepting new connections"
                )
            }
            Self::CounterOverflow(counter) => {
                write!(formatter, "metrics counter `{counter}` overflowed")
            }
            Self::StatePoisoned(state) => write!(formatter, "metrics state `{state}` is poisoned"),
            Self::UnsupportedPlatform(platform) => {
                write!(
                    formatter,
                    "process and host sampling is unsupported on {platform}"
                )
            }
            Self::Io { path, source } => write!(formatter, "failed to read `{path}`: {source}"),
            Self::InvalidData { resource, detail } => {
                write!(formatter, "invalid data in `{resource}`: {detail}")
            }
            Self::SystemClockBeforeUnixEpoch => {
                formatter.write_str("system clock predates the Unix epoch")
            }
            Self::ValueOutOfRange(value) => {
                write!(
                    formatter,
                    "sampled value `{value}` exceeds the supported range"
                )
            }
        }
    }
}

impl Error for MetricsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Fixed upper bounds for request and relay latency buckets, in milliseconds.
pub const OPERATION_LATENCY_BUCKETS_MS: &[u64] = &[
    1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    Healthy,
    Degraded,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl ComponentStatus {
    const fn healthy() -> Self {
        Self {
            state: ComponentState::Healthy,
            reason: None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    const fn unsupported(reason: &'static str) -> Self {
        Self {
            state: ComponentState::Unsupported,
            reason: Some(reason),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpOperationResult {
    Success,
    ClientError,
    ServerError,
    UpstreamError,
    Timeout,
    Cancelled,
    InternalError,
}

impl HttpOperationResult {
    const ALL: [Self; 7] = [
        Self::Success,
        Self::ClientError,
        Self::ServerError,
        Self::UpstreamError,
        Self::Timeout,
        Self::Cancelled,
        Self::InternalError,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ClientError => 1,
            Self::ServerError => 2,
            Self::UpstreamError => 3,
            Self::Timeout => 4,
            Self::Cancelled => 5,
            Self::InternalError => 6,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::UpstreamError => "upstream_error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::InternalError => "internal_error",
        }
    }

    #[must_use]
    pub const fn from_status(status: Option<u16>) -> Self {
        match status {
            Some(200..=399) => Self::Success,
            Some(400..=499) => Self::ClientError,
            Some(500..=599) => Self::ServerError,
            Some(_) => Self::InternalError,
            None => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpRelayResult {
    Success,
    ConnectError,
    ConnectTimeout,
    IdleTimeout,
    LifetimeTimeout,
    Cancelled,
    IoError,
    AccountingError,
    ProxyProtocolError,
}

impl TcpRelayResult {
    const ALL: [Self; 9] = [
        Self::Success,
        Self::ConnectError,
        Self::ConnectTimeout,
        Self::IdleTimeout,
        Self::LifetimeTimeout,
        Self::Cancelled,
        Self::IoError,
        Self::AccountingError,
        Self::ProxyProtocolError,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ConnectError => 1,
            Self::ConnectTimeout => 2,
            Self::IdleTimeout => 3,
            Self::LifetimeTimeout => 4,
            Self::Cancelled => 5,
            Self::IoError => 6,
            Self::AccountingError => 7,
            Self::ProxyProtocolError => 8,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ConnectError => "connect_error",
            Self::ConnectTimeout => "connect_timeout",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeTimeout => "lifetime_timeout",
            Self::Cancelled => "cancelled",
            Self::IoError => "io_error",
            Self::AccountingError => "accounting_error",
            Self::ProxyProtocolError => "proxy_protocol_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedTransport {
    Http,
    Rtmp,
    Forward,
    Cache,
    Tcp,
    Udp,
    H3,
    Acme,
}

impl ObservedTransport {
    const ALL: [Self; 8] = [
        Self::Http,
        Self::Rtmp,
        Self::Forward,
        Self::Cache,
        Self::Tcp,
        Self::Udp,
        Self::H3,
        Self::Acme,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Http => 0,
            Self::Rtmp => 1,
            Self::Forward => 2,
            Self::Cache => 3,
            Self::Tcp => 4,
            Self::Udp => 5,
            Self::H3 => 6,
            Self::Acme => 7,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Rtmp => "rtmp",
            Self::Forward => "forward",
            Self::Cache => "cache",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::H3 => "h3",
            Self::Acme => "acme",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportOutcome {
    Success,
    ClientError,
    ServerError,
    UpstreamError,
    Timeout,
    Rejected,
    Cancelled,
    InternalError,
    Degraded,
}

impl TransportOutcome {
    const ALL: [Self; 9] = [
        Self::Success,
        Self::ClientError,
        Self::ServerError,
        Self::UpstreamError,
        Self::Timeout,
        Self::Rejected,
        Self::Cancelled,
        Self::InternalError,
        Self::Degraded,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ClientError => 1,
            Self::ServerError => 2,
            Self::UpstreamError => 3,
            Self::Timeout => 4,
            Self::Rejected => 5,
            Self::Cancelled => 6,
            Self::InternalError => 7,
            Self::Degraded => 8,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::UpstreamError => "upstream_error",
            Self::Timeout => "timeout",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::InternalError => "internal_error",
            Self::Degraded => "degraded",
        }
    }

    const fn from_http(result: HttpOperationResult) -> Self {
        match result {
            HttpOperationResult::Success => Self::Success,
            HttpOperationResult::ClientError => Self::ClientError,
            HttpOperationResult::ServerError => Self::ServerError,
            HttpOperationResult::UpstreamError => Self::UpstreamError,
            HttpOperationResult::Timeout => Self::Timeout,
            HttpOperationResult::Cancelled => Self::Cancelled,
            HttpOperationResult::InternalError => Self::InternalError,
        }
    }

    const fn from_tcp(result: TcpRelayResult) -> Self {
        match result {
            TcpRelayResult::Success => Self::Success,
            TcpRelayResult::ConnectError
            | TcpRelayResult::ConnectTimeout
            | TcpRelayResult::IoError => Self::UpstreamError,
            TcpRelayResult::IdleTimeout | TcpRelayResult::LifetimeTimeout => Self::Timeout,
            TcpRelayResult::Cancelled => Self::Cancelled,
            TcpRelayResult::AccountingError => Self::InternalError,
            TcpRelayResult::ProxyProtocolError => Self::Rejected,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
const fn transport_outcome_index(outcome: TransportOutcome) -> u8 {
    outcome.index() as u8
}

pub const ACCESS_RECORD_CAPACITY: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessRecord {
    pub timestamp_unix_ms: u64,
    pub correlation_id: String,
    pub listener: String,
    pub transport: ObservedTransport,
    pub outcome: TransportOutcome,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub duration_ms: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_received: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_sent: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyBucketSnapshot {
    pub upper_bound_ms: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySnapshot {
    pub buckets: Box<[LatencyBucketSnapshot]>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub sum_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpOperationCountSnapshot {
    pub result: HttpOperationResult,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpOperationSnapshot {
    pub outcomes: Box<[HttpOperationCountSnapshot]>,
    pub latency: LatencySnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportOperationCountSnapshot {
    pub outcome: TransportOutcome,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportOperationSnapshot {
    pub transport: ObservedTransport,
    pub outcomes: Box<[TransportOperationCountSnapshot]>,
    pub latency: LatencySnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheEvent {
    Hit,
    Miss,
    Admission,
    Eviction,
}

impl CacheEvent {
    const ALL: [Self; 4] = [Self::Hit, Self::Miss, Self::Admission, Self::Eviction];

    const fn index(self) -> usize {
        match self {
            Self::Hit => 0,
            Self::Miss => 1,
            Self::Admission => 2,
            Self::Eviction => 3,
        }
    }
}

const fn cache_event_outcome(event: CacheEvent) -> TransportOutcome {
    match event {
        CacheEvent::Hit | CacheEvent::Admission | CacheEvent::Eviction => TransportOutcome::Success,
        CacheEvent::Miss => TransportOutcome::UpstreamError,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSnapshot {
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub hits: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub misses: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub admissions: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub evictions: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpRelayCountSnapshot {
    pub result: TcpRelayResult,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpRelaySnapshot {
    pub outcomes: Box<[TcpRelayCountSnapshot]>,
    pub latency: LatencySnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProtocolCountSnapshot {
    pub result: ProxyProtocolResult,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProtocolSnapshot {
    pub outcomes: Box<[ProxyProtocolCountSnapshot]>,
}

#[derive(Clone)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricsInner>,
}

/// Process execution mode reported by the management capability surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Direct,
    Supervised,
}

struct RuntimeMetricsInner {
    process_admission: Arc<ProcessAdmissionState>,
    process_runtime: ProcessRuntime,
    listeners: RwLock<HashMap<String, Arc<ListenerMetricsState>>>,
    upstream_pools: RwLock<Vec<Arc<RoundRobinPool>>>,
    certbot: RwLock<CertbotMonitoring>,
    acme_managed: RwLock<AcmeManagedMonitoring>,
    direct_files: RwLock<DirectFileMonitoring>,
    previous_cpu_sample: Mutex<Option<CpuSample>>,
    rtmp_recording_supported: AtomicBool,
    generation_started_at: Instant,
}

#[derive(Clone)]
pub struct ProcessRuntime {
    inner: Arc<ProcessRuntimeInner>,
}

struct ProcessRuntimeInner {
    admission: Arc<ProcessAdmissionState>,
    listeners: Mutex<HashMap<String, Arc<SharedListenerMetricsState>>>,
    transport_operations: Arc<TransportOperationsState>,
    access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
    mode: RuntimeMode,
    started_at: Instant,
}

impl ProcessRuntime {
    #[must_use]
    pub fn new(max_connections: Option<u64>) -> Self {
        Self::with_mode(max_connections, RuntimeMode::Direct)
    }

    /// Creates process metrics for a worker using authenticated listener adoption.
    #[must_use]
    pub fn supervised(max_connections: Option<u64>) -> Self {
        Self::with_mode(max_connections, RuntimeMode::Supervised)
    }

    fn with_mode(max_connections: Option<u64>, mode: RuntimeMode) -> Self {
        Self {
            inner: Arc::new(ProcessRuntimeInner {
                admission: Arc::new(ProcessAdmissionState::new(max_connections)),
                listeners: Mutex::new(HashMap::new()),
                transport_operations: Arc::new(TransportOperationsState::new()),
                access_records: Arc::new(Mutex::new(VecDeque::with_capacity(
                    ACCESS_RECORD_CAPACITY,
                ))),
                mode,
                started_at: Instant::now(),
            }),
        }
    }

    fn listener(
        &self,
        bind: &str,
        max_connections: Option<u64>,
    ) -> Result<Arc<SharedListenerMetricsState>, MetricsError> {
        let mut listeners = self
            .inner
            .listeners
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("process listeners"))?;
        Ok(Arc::clone(listeners.entry(bind.to_owned()).or_insert_with(
            || {
                Arc::new(SharedListenerMetricsState::new(
                    max_connections,
                    self.transport_operations(),
                    Arc::clone(&self.inner.access_records),
                ))
            },
        )))
    }

    fn activate_limit(&self, max_connections: Option<u64>) {
        self.inner.admission.set_limit(max_connections);
    }

    fn transport_operations(&self) -> Arc<TransportOperationsState> {
        Arc::clone(&self.inner.transport_operations)
    }

    fn access_records_snapshot(&self) -> Result<Vec<AccessRecord>, MetricsError> {
        Ok(self
            .inner
            .access_records
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("access records"))?
            .iter()
            .cloned()
            .collect())
    }

    #[must_use]
    pub fn mode(&self) -> RuntimeMode {
        self.inner.mode
    }
}

fn append_access_record(records: &Mutex<VecDeque<AccessRecord>>, record: AccessRecord) {
    let mut records = records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if records.len() == ACCESS_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record.clone());
    drop(records);
    if let Ok(value) = serde_json::to_value(record) {
        crate::logging::log_json("oxiroute::access", &value);
    }
}

#[derive(Default)]
struct CertbotMonitoring {
    reconcilers: Vec<Arc<CertbotReconciler>>,
    watcher: Option<CertbotWatcherMonitor>,
}

#[derive(Default)]
struct DirectFileMonitoring {
    reconcilers: Vec<Arc<FileReconciler>>,
    watcher: Option<FileWatcherMonitor>,
}

#[derive(Default)]
struct AcmeManagedMonitoring {
    reconcilers: Vec<Arc<AcmeManagedReconciler>>,
}

impl RuntimeMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_connections(None)
    }

    #[must_use]
    pub fn with_max_connections(max_connections: Option<u64>) -> Self {
        Self::for_process(ProcessRuntime::new(max_connections))
    }

    #[must_use]
    pub fn for_process(process_runtime: ProcessRuntime) -> Self {
        Self {
            inner: Arc::new(RuntimeMetricsInner {
                process_admission: Arc::clone(&process_runtime.inner.admission),
                process_runtime,
                listeners: RwLock::new(HashMap::new()),
                upstream_pools: RwLock::new(Vec::new()),
                certbot: RwLock::new(CertbotMonitoring::default()),
                acme_managed: RwLock::new(AcmeManagedMonitoring::default()),
                direct_files: RwLock::new(DirectFileMonitoring::default()),
                previous_cpu_sample: Mutex::new(None),
                rtmp_recording_supported: AtomicBool::new(false),
                generation_started_at: Instant::now(),
            }),
        }
    }

    /// Returns whether this process is serving directly or through the supervisor.
    #[must_use]
    pub fn supervision_mode(&self) -> RuntimeMode {
        self.inner.process_runtime.mode()
    }

    pub(crate) fn activate_limits(&self, max_connections: Option<u64>) {
        self.inner.process_runtime.activate_limit(max_connections);
        if let Ok(listeners) = self.inner.listeners.read() {
            for listener in listeners.values() {
                listener.shared.set_limit(listener.max_connections);
            }
        }
    }

    /// Registers the immutable upstream pools in canonical definition order.
    ///
    /// # Errors
    ///
    /// Returns an error when the pool registry is poisoned.
    pub fn register_upstream_pools(
        &self,
        pools: impl IntoIterator<Item = Arc<RoundRobinPool>>,
    ) -> Result<(), MetricsError> {
        let mut registered = self
            .inner
            .upstream_pools
            .write()
            .map_err(|_| MetricsError::StatePoisoned("upstream pools"))?;
        *registered = pools.into_iter().collect();
        Ok(())
    }

    /// Registers the process-lifetime Certbot reconcilers and watcher monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when the Certbot monitoring registry is poisoned.
    pub fn register_certbot_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<CertbotReconciler>>,
        watcher: Option<CertbotWatcherMonitor>,
    ) -> Result<(), MetricsError> {
        let mut certbot = self
            .inner
            .certbot
            .write()
            .map_err(|_| MetricsError::StatePoisoned("Certbot monitoring"))?;
        certbot.reconcilers = reconcilers.into_iter().collect();
        certbot.watcher = watcher;
        Ok(())
    }

    /// Registers the managed ACME reconcilers for status, API, and Prometheus snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed ACME monitoring registry is poisoned.
    pub fn register_acme_managed_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<AcmeManagedReconciler>>,
    ) -> Result<(), MetricsError> {
        let mut managed = self
            .inner
            .acme_managed
            .write()
            .map_err(|_| MetricsError::StatePoisoned("managed ACME monitoring"))?;
        managed.reconcilers = reconcilers.into_iter().collect();
        Ok(())
    }

    /// Registers the process-lifetime direct-file reconcilers and watcher monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when the direct-file monitoring registry is poisoned.
    pub fn register_direct_file_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<FileReconciler>>,
        watcher: Option<FileWatcherMonitor>,
    ) -> Result<(), MetricsError> {
        let mut direct_files = self
            .inner
            .direct_files
            .write()
            .map_err(|_| MetricsError::StatePoisoned("direct-file monitoring"))?;
        direct_files.reconcilers = reconcilers.into_iter().collect();
        direct_files.watcher = watcher;
        Ok(())
    }

    /// Registers a listener and returns its accounting handle.
    ///
    /// # Errors
    ///
    /// Returns an error when any field is empty, the listener name is already registered, or the
    /// listener registry is poisoned.
    pub fn register_listener(
        &self,
        name: impl Into<String>,
        protocol: impl Into<String>,
        bind: impl Into<String>,
        max_connections: impl Into<Option<u64>>,
    ) -> Result<ListenerMetrics, MetricsError> {
        let name = name.into();
        let protocol = protocol.into();
        let bind = bind.into();
        let max_connections = max_connections.into();
        validate_listener_field("name", &name)?;
        validate_listener_field("protocol", &protocol)?;
        validate_listener_field("bind", &bind)?;
        if max_connections == Some(0) {
            return Err(MetricsError::InvalidListenerField("max_connections"));
        }

        let mut listeners = self
            .inner
            .listeners
            .write()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        match listeners.entry(name.clone()) {
            Entry::Vacant(entry) => {
                let shared = self
                    .inner
                    .process_runtime
                    .listener(&bind, max_connections)?;
                let state = Arc::new(ListenerMetricsState::new(
                    name,
                    protocol,
                    bind,
                    max_connections,
                    shared,
                ));
                entry.insert(Arc::clone(&state));
                Ok(ListenerMetrics {
                    process: Arc::clone(&self.inner.process_admission),
                    state,
                })
            }
            Entry::Occupied(_) => Err(MetricsError::DuplicateListener(name)),
        }
    }

    /// Registers a canonical listener with a stable, transport-qualified bind identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the Unix socket path cannot be represented without loss, a field is
    /// invalid, the listener name is already registered, or the listener registry is poisoned.
    pub fn register_configured_listener(
        &self,
        name: impl Into<String>,
        protocol: impl Into<String>,
        bind: &ListenerBind,
        max_connections: Option<u64>,
    ) -> Result<ListenerMetrics, MetricsError> {
        let name = name.into();
        let bind = match bind {
            ListenerBind::Socket { address } => format!("socket:{address}"),
            ListenerBind::Udp { address } => format!("udp:{address}"),
            ListenerBind::Unix { path, .. } => {
                let path = path
                    .to_str()
                    .ok_or_else(|| MetricsError::InvalidListenerBind {
                        listener: name.clone(),
                        detail: "Unix socket path is not valid UTF-8",
                    })?;
                format!("unix:{path}")
            }
        };
        self.register_listener(name, protocol, bind, max_connections)
    }

    /// Returns the accounting handle for a registered listener.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener registry is poisoned.
    pub fn listener(&self, name: &str) -> Result<Option<ListenerMetrics>, MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        Ok(listeners.get(name).map(|state| ListenerMetrics {
            process: Arc::clone(&self.inner.process_admission),
            state: Arc::clone(state),
        }))
    }

    /// Accounts for a newly accepted connection on a named listener.
    ///
    /// The returned guard decrements the active connection count when dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener is unknown, the registry is poisoned, a configured
    /// connection cap is reached, or a counter would overflow.
    pub fn begin_connection(&self, listener_name: &str) -> Result<ConnectionGuard, MetricsError> {
        self.listener(listener_name)?
            .ok_or_else(|| MetricsError::ListenerNotFound(listener_name.to_owned()))?
            .begin_connection()
    }

    /// Acquires only process-wide connection capacity, for runtime-owned listeners without a
    /// canonical listener metrics identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cap is reached or an admission counter would overflow.
    pub fn begin_process_connection(&self) -> Result<ProcessConnectionGuard, MetricsError> {
        self.inner.process_admission.acquire()
    }

    /// Acquires process capacity for a local control-plane connection while bypassing a data-plane
    /// administrative drain.
    ///
    /// # Errors
    ///
    /// Returns an error when the process capacity is reached or a counter would overflow.
    pub fn begin_control_connection(&self) -> Result<ProcessConnectionGuard, MetricsError> {
        self.inner.process_admission.acquire_control()
    }

    pub fn set_process_administrative_state(&self, state: AdministrativeState) {
        self.inner
            .process_admission
            .administrative_state
            .store(state as u8, Ordering::Release);
    }

    /// Changes admission state for one exact listener.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener is unknown or the registry is poisoned.
    pub fn set_listener_administrative_state(
        &self,
        name: &str,
        state: AdministrativeState,
    ) -> Result<(), MetricsError> {
        let listener = self
            .listener(name)?
            .ok_or_else(|| MetricsError::ListenerNotFound(name.to_owned()))?;
        listener
            .state
            .shared
            .administrative_state
            .store(state as u8, Ordering::Release);
        Ok(())
    }

    /// Records whether successfully activated RTMP services own any recorder runtime.
    pub fn set_rtmp_recording_supported(&self, supported: bool) {
        self.inner
            .rtmp_recording_supported
            .store(supported, Ordering::Release);
    }

    #[must_use]
    pub(crate) fn rtmp_recording_supported(&self) -> bool {
        self.inner.rtmp_recording_supported.load(Ordering::Acquire)
    }

    /// Records one bounded transport outcome and latency sample.
    ///
    /// The transport and outcome values are closed enums so this registry cannot grow with a
    /// request URI, stream name, endpoint, or other unbounded request data.
    ///
    /// # Errors
    ///
    /// Returns an error if a counter or latency total would overflow.
    pub fn record_transport_operation(
        &self,
        transport: ObservedTransport,
        outcome: TransportOutcome,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.inner
            .process_runtime
            .inner
            .transport_operations
            .record(transport, outcome, Some(duration))
    }

    /// Samples process, host, traffic, and listener metrics.
    ///
    /// CPU percent is `None` on the first successful sample and whenever no aggregate host CPU tick
    /// elapsed since the previous sample.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state cannot be read, the system clock is invalid, aggregate
    /// counters overflow, the current platform is unsupported, or an operating-system metric cannot
    /// be read or parsed truthfully.
    pub fn snapshot(&self) -> Result<RuntimeSnapshot, MetricsError> {
        let (traffic, listeners) = self.counter_snapshots()?;
        let upstream_pools = self.upstream_pool_snapshots()?;
        let (mut certbot_certificates, certbot_watcher) = self.certbot_snapshots()?;
        certbot_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut acme_managed_certificates = self.acme_managed_snapshots()?;
        acme_managed_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let (mut direct_file_certificates, direct_file_watcher) = self.direct_file_snapshots()?;
        direct_file_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut previous_cpu_sample = self
            .inner
            .previous_cpu_sample
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("previous CPU sample"))?;
        let system = sample_system()?;
        let transport_operations = self
            .inner
            .process_runtime
            .inner
            .transport_operations
            .snapshots();
        let access_records = self.inner.process_runtime.access_records_snapshot()?;
        let cpu_percent = (system.process_status.state == ComponentState::Healthy
            && system.host.status.state == ComponentState::Healthy)
            .then(|| cpu_percent(previous_cpu_sample.as_ref(), &system.cpu))
            .transpose()?
            .flatten();
        let sampled_at_unix_ms = unix_time_ms()?;
        let uptime_ms = u64::try_from(
            self.inner
                .process_runtime
                .inner
                .started_at
                .elapsed()
                .as_millis(),
        )
        .map_err(|_| MetricsError::ValueOutOfRange("uptime milliseconds"))?;

        let SystemSample {
            process_status,
            resident_memory_bytes,
            virtual_memory_bytes,
            thread_count,
            open_file_descriptors,
            host,
            cpu,
        } = system;
        let snapshot = RuntimeSnapshot {
            sampled_at_unix_ms,
            uptime_ms,
            generation_age_ms: u64::try_from(
                self.inner.generation_started_at.elapsed().as_millis(),
            )
            .map_err(|_| MetricsError::ValueOutOfRange("generation age milliseconds"))?,
            process: ProcessSnapshot {
                active_connections: self.inner.process_admission.active.load(Ordering::Relaxed),
                administrative_state: AdministrativeState::from_u8(
                    self.inner
                        .process_admission
                        .administrative_state
                        .load(Ordering::Acquire),
                ),
                status: process_status,
                cpu_percent,
                max_connections: self.inner.process_admission.limit(),
                rejected_connections: self
                    .inner
                    .process_admission
                    .rejected
                    .load(Ordering::Relaxed),
                retry_attempts: self
                    .inner
                    .process_admission
                    .retry_attempts
                    .load(Ordering::Relaxed),
                resident_memory_bytes,
                virtual_memory_bytes,
                thread_count,
                open_file_descriptors,
            },
            host,
            traffic,
            listeners,
            upstream_pools,
            transport_operations,
            access_records,
            certbot_certificates,
            certbot_watcher,
            acme_managed_certificates,
            direct_file_certificates,
            direct_file_watcher,
        };
        if process_status.state == ComponentState::Healthy
            && snapshot.host.status.state == ComponentState::Healthy
        {
            *previous_cpu_sample = Some(cpu);
        }
        Ok(snapshot)
    }

    pub(crate) fn topology_health_snapshot(&self) -> Result<RuntimeHealthSnapshot, MetricsError> {
        let (_, listeners) = self.counter_snapshots()?;
        Ok(RuntimeHealthSnapshot {
            sampled_at_unix_ms: unix_time_ms()?,
            listeners,
            upstream_pools: self.upstream_pool_snapshots()?,
        })
    }

    fn upstream_pool_snapshots(&self) -> Result<Vec<PoolHealthSnapshot>, MetricsError> {
        Ok(self
            .inner
            .upstream_pools
            .read()
            .map_err(|_| MetricsError::StatePoisoned("upstream pools"))?
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect())
    }

    fn certbot_snapshots(
        &self,
    ) -> Result<
        (
            Vec<CertbotCertificateSnapshot>,
            Option<CertbotWatcherSnapshot>,
        ),
        MetricsError,
    > {
        let certbot = self
            .inner
            .certbot
            .read()
            .map_err(|_| MetricsError::StatePoisoned("Certbot monitoring"))?;
        let certificates = certbot
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                CertbotCertificateSnapshot {
                    name: status.certificate,
                    active_archive_revision: status.active_archive_revision,
                    active_content_revision: status.active_content_revision,
                    expires_at: status.not_after,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code.map(str::to_owned),
                }
            })
            .collect();
        let watcher = certbot.watcher.as_ref().map(|watcher| {
            let status = watcher.status();
            let health = if !status.running {
                CertbotWatcherHealth::Stopped
            } else if status.degraded {
                CertbotWatcherHealth::Degraded
            } else {
                CertbotWatcherHealth::Healthy
            };
            CertbotWatcherSnapshot {
                health,
                coalesced_events: status.coalesced_events,
                ignored_access_events: status.ignored_access_events,
                backend_errors: status.backend_errors,
                watch_recoveries: status.watch_recoveries,
                watch_refreshes: status.watch_refreshes,
                rescans: status.rescans,
                periodic_rescans: status.periodic_rescans,
                reconciliation_failures: status.reconciliation_failures,
            }
        });
        Ok((certificates, watcher))
    }

    fn direct_file_snapshots(
        &self,
    ) -> Result<
        (
            Vec<DirectFileCertificateSnapshot>,
            Option<DirectFileWatcherSnapshot>,
        ),
        MetricsError,
    > {
        let direct_files = self
            .inner
            .direct_files
            .read()
            .map_err(|_| MetricsError::StatePoisoned("direct-file monitoring"))?;
        let certificates = direct_files
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                DirectFileCertificateSnapshot {
                    name: status.certificate,
                    active_content_revision: status.active_content_revision,
                    expires_at: status.not_after,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code.map(str::to_owned),
                }
            })
            .collect();
        let watcher = direct_files.watcher.as_ref().map(|watcher| {
            let status = watcher.status();
            let health = if !status.running {
                CertbotWatcherHealth::Stopped
            } else if status.degraded {
                CertbotWatcherHealth::Degraded
            } else {
                CertbotWatcherHealth::Healthy
            };
            DirectFileWatcherSnapshot {
                health,
                coalesced_events: status.coalesced_events,
                ignored_access_events: status.ignored_access_events,
                backend_errors: status.backend_errors,
                watch_recoveries: status.watch_recoveries,
                watch_refreshes: status.watch_refreshes,
                rescans: status.rescans,
                periodic_rescans: status.periodic_rescans,
                reconciliation_failures: status.reconciliation_failures,
            }
        });
        Ok((certificates, watcher))
    }

    fn acme_managed_snapshots(&self) -> Result<Vec<AcmeManagedCertificateSnapshot>, MetricsError> {
        let managed = self
            .inner
            .acme_managed
            .read()
            .map_err(|_| MetricsError::StatePoisoned("managed ACME monitoring"))?;
        Ok(managed
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                AcmeManagedCertificateSnapshot {
                    name: status.certificate,
                    directory_url: status.directory_url,
                    disk_revision: status.disk_revision,
                    active_revision: status.active_revision,
                    expires_at: status.not_after,
                    not_before_unix_seconds: status.not_before_unix_seconds,
                    not_after_unix_seconds: status.not_after_unix_seconds,
                    next_action_unix_seconds: status.next_action_unix_seconds,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code,
                    renewal_information_status: status.renewal_information_status.into(),
                    dns_provider: status.dns_provider,
                    dns_provider_deployment: status.dns_provider_deployment.map(str::to_owned),
                    dns_provider_health: status.dns_provider_health.map(str::to_owned),
                    dns_cleanup_status: status.dns_cleanup_status.into(),
                }
            })
            .collect())
    }

    fn counter_snapshots(&self) -> Result<(TrafficSnapshot, Vec<ListenerSnapshot>), MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        let mut states: Vec<_> = listeners.values().cloned().collect();
        drop(listeners);
        states.sort_unstable_by(|left, right| left.name.cmp(&right.name));

        let mut traffic = TrafficSnapshot::default();
        let mut snapshots = Vec::with_capacity(states.len());
        for state in states {
            let snapshot = state.snapshot();
            add_total(
                &mut traffic.accepted_connections,
                snapshot.accepted_connections,
                "traffic.acceptedConnections",
            )?;
            add_total(
                &mut traffic.rejected_connections,
                snapshot.rejected_connections,
                "traffic.rejectedConnections",
            )?;
            add_total(
                &mut traffic.active_connections,
                snapshot.active_connections,
                "traffic.activeConnections",
            )?;
            add_total(
                &mut traffic.bytes_received,
                snapshot.bytes_received,
                "traffic.bytesReceived",
            )?;
            add_total(
                &mut traffic.bytes_sent,
                snapshot.bytes_sent,
                "traffic.bytesSent",
            )?;
            snapshots.push(snapshot);
        }
        Ok((traffic, snapshots))
    }
}

pub(crate) struct RuntimeHealthSnapshot {
    pub sampled_at_unix_ms: u64,
    pub listeners: Vec<ListenerSnapshot>,
    pub upstream_pools: Vec<PoolHealthSnapshot>,
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ListenerMetrics {
    process: Arc<ProcessAdmissionState>,
    state: Arc<ListenerMetricsState>,
}

impl ListenerMetrics {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.state.name
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.state.protocol
    }

    #[must_use]
    pub fn bind(&self) -> &str {
        &self.state.bind
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        AdministrativeState::from_u8(self.process.administrative_state.load(Ordering::Acquire))
            == AdministrativeState::Ready
            && AdministrativeState::from_u8(
                self.state
                    .shared
                    .administrative_state
                    .load(Ordering::Acquire),
            ) == AdministrativeState::Ready
    }

    /// Marks the listener socket as bound and available to the runtime.
    pub fn mark_listening(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Listening as u8, Ordering::Release);
    }

    /// Marks a listener that completed an orderly runtime shutdown.
    pub fn mark_stopped(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Stopped as u8, Ordering::Release);
    }

    /// Marks a listener whose runtime terminated unexpectedly.
    pub fn mark_failed(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Failed as u8, Ordering::Release);
    }

    /// Accounts for a newly accepted connection.
    ///
    /// The returned guard decrements the active connection count when dropped, including during
    /// unwinding.
    ///
    /// # Errors
    ///
    /// Returns an error if a configured connection cap is reached or the accepted or active
    /// connection counter would overflow.
    pub fn begin_connection(&self) -> Result<ConnectionGuard, MetricsError> {
        if AdministrativeState::from_u8(
            self.state
                .shared
                .administrative_state
                .load(Ordering::Acquire),
        ) != AdministrativeState::Ready
        {
            checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            )?;
            return Err(MetricsError::AdministrativeDrain {
                resource: "listener",
                name: self.state.name.clone(),
            });
        }
        checked_atomic_add(
            &self.state.shared.accepted_connections,
            1,
            "listener.acceptedConnections",
        )?;
        let process = match self.process.acquire() {
            Ok(process) => process,
            Err(error) => {
                checked_atomic_add(
                    &self.state.shared.rejected_connections,
                    1,
                    "listener.rejectedConnections",
                )?;
                return Err(error);
            }
        };
        let admission = self.state.shared.active_connections.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| {
                if self
                    .state
                    .shared
                    .limit()
                    .is_some_and(|limit| current >= limit)
                {
                    return None;
                }
                current.checked_add(1)
            },
        );
        if let Err(current) = admission {
            checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            )?;
            return Err(
                if let Some(limit) = self.state.shared.limit().filter(|limit| current >= *limit) {
                    MetricsError::ConnectionLimitReached {
                        listener: self.state.name.clone(),
                        limit,
                    }
                } else {
                    MetricsError::CounterOverflow("listener.activeConnections")
                },
            );
        }
        if AdministrativeState::from_u8(
            self.state
                .shared
                .administrative_state
                .load(Ordering::Acquire),
        ) != AdministrativeState::Ready
        {
            decrement_counter(&self.state.shared.active_connections);
            checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            )?;
            drop(process);
            return Err(MetricsError::AdministrativeDrain {
                resource: "listener",
                name: self.state.name.clone(),
            });
        }
        Ok(ConnectionGuard {
            process: Some(process),
            state: Arc::clone(&self.state),
            releases_active_connection: true,
            started_at: Instant::now(),
            correlation_id: crate::logging::next_correlation_id(),
            outcome: AtomicU8::new(transport_outcome_index(TransportOutcome::Success)),
            received: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        })
    }

    /// Returns a traffic-only handle for a connection admitted by the server runtime.
    ///
    /// This handle does not acquire or release connection capacity. The runtime-owned admission
    /// guard remains responsible for the active connection lifetime.
    #[must_use]
    pub fn traffic_accounting(&self) -> ConnectionGuard {
        ConnectionGuard {
            process: None,
            state: Arc::clone(&self.state),
            releases_active_connection: false,
            started_at: Instant::now(),
            correlation_id: String::new(),
            outcome: AtomicU8::new(transport_outcome_index(TransportOutcome::Success)),
            received: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        }
    }

    /// Adds bytes read across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.state.shared.bytes_received,
            bytes,
            "listener.bytesReceived",
        )
    }

    /// Adds bytes written across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.shared.bytes_sent, bytes, "listener.bytesSent")
    }

    /// Accounts for a retry that the HTTP proxy handed back to Pingora.
    pub fn record_retry_attempt(&self) {
        self.process.retry_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one terminal reverse-proxy HTTP operation and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.state.shared.record_http_operation(result, duration)?;
        self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_http(result),
            Some(duration),
        )
    }

    pub(crate) fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        self.state.shared.record_cache_event(event)?;
        self.state.shared.transport_operations.record(
            ObservedTransport::Cache,
            cache_event_outcome(event),
            None,
        )
    }

    /// Records one terminal TCP relay and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.state.shared.record_tcp_relay(result, duration)?;
        self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_tcp(result),
            Some(duration),
        )
    }

    /// Records one redacted PROXY protocol result category.
    ///
    /// # Errors
    ///
    /// Returns an error if the result counter would overflow.
    pub fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.state.shared.record_proxy_protocol(result)
    }
}

pub struct ConnectionGuard {
    process: Option<ProcessConnectionGuard>,
    state: Arc<ListenerMetricsState>,
    releases_active_connection: bool,
    started_at: Instant,
    correlation_id: String,
    outcome: AtomicU8,
    received: AtomicU64,
    sent: AtomicU64,
}

impl ConnectionGuard {
    #[must_use]
    pub fn listener_name(&self) -> &str {
        &self.state.name
    }

    /// Adds bytes read from this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.state.shared.bytes_received,
            bytes,
            "listener.bytesReceived",
        )?;
        checked_atomic_add(&self.received, bytes, "access.bytesReceived")
    }

    /// Adds bytes written to this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.shared.bytes_sent, bytes, "listener.bytesSent")?;
        checked_atomic_add(&self.sent, bytes, "access.bytesSent")
    }

    /// Records one terminal TCP relay and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.state.shared.record_tcp_relay(result, duration)?;
        self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_tcp(result),
            Some(duration),
        )?;
        self.outcome.store(
            transport_outcome_index(TransportOutcome::from_tcp(result)),
            Ordering::Release,
        );
        Ok(())
    }

    /// Records one redacted PROXY protocol result category.
    ///
    /// # Errors
    ///
    /// Returns an error if the result counter would overflow.
    pub fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.state.shared.record_proxy_protocol(result)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if self.releases_active_connection {
            decrement_counter(&self.state.shared.active_connections);
            let duration_ms =
                u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            append_access_record(
                &self.state.shared.access_records,
                AccessRecord {
                    timestamp_unix_ms: unix_time_ms().unwrap_or(0),
                    correlation_id: self.correlation_id.clone(),
                    listener: crate::logging::redact_identifier(&self.state.name),
                    transport: transport_for_protocol(&self.state.protocol),
                    outcome: TransportOutcome::ALL
                        .get(self.outcome.load(Ordering::Acquire) as usize)
                        .copied()
                        .unwrap_or(TransportOutcome::InternalError),
                    duration_ms,
                    bytes_received: self.received.load(Ordering::Relaxed),
                    bytes_sent: self.sent.load(Ordering::Relaxed),
                },
            );
        }
        self.process.take();
    }
}

struct ProcessAdmissionState {
    active: AtomicU64,
    administrative_state: AtomicU8,
    limit: AtomicU64,
    rejected: AtomicU64,
    retry_attempts: AtomicU64,
}

impl ProcessAdmissionState {
    fn new(limit: Option<u64>) -> Self {
        Self {
            active: AtomicU64::new(0),
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            limit: AtomicU64::new(encode_limit(limit)),
            rejected: AtomicU64::new(0),
            retry_attempts: AtomicU64::new(0),
        }
    }

    fn limit(&self) -> Option<u64> {
        decode_limit(self.limit.load(Ordering::Acquire))
    }

    fn set_limit(&self, limit: Option<u64>) {
        self.limit.store(encode_limit(limit), Ordering::Release);
    }

    fn acquire(self: &Arc<Self>) -> Result<ProcessConnectionGuard, MetricsError> {
        self.acquire_inner(true)
    }

    fn acquire_control(self: &Arc<Self>) -> Result<ProcessConnectionGuard, MetricsError> {
        self.acquire_inner(false)
    }

    fn acquire_inner(
        self: &Arc<Self>,
        enforce_administrative_state: bool,
    ) -> Result<ProcessConnectionGuard, MetricsError> {
        if enforce_administrative_state
            && AdministrativeState::from_u8(self.administrative_state.load(Ordering::Acquire))
                != AdministrativeState::Ready
        {
            checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
            return Err(MetricsError::AdministrativeDrain {
                resource: "process",
                name: "oxiroute".into(),
            });
        }
        let admission = self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if (enforce_administrative_state
                    && AdministrativeState::from_u8(
                        self.administrative_state.load(Ordering::Acquire),
                    ) != AdministrativeState::Ready)
                    || self.limit().is_some_and(|limit| current >= limit)
                {
                    None
                } else {
                    current.checked_add(1)
                }
            });
        match admission {
            Ok(_) => {
                if enforce_administrative_state
                    && AdministrativeState::from_u8(
                        self.administrative_state.load(Ordering::Acquire),
                    ) != AdministrativeState::Ready
                {
                    decrement_counter(&self.active);
                    checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
                    return Err(MetricsError::AdministrativeDrain {
                        resource: "process",
                        name: "oxiroute".into(),
                    });
                }
                Ok(ProcessConnectionGuard {
                    state: Arc::clone(self),
                })
            }
            Err(current) => {
                checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
                Err(
                    if let Some(limit) = self.limit().filter(|limit| current >= *limit) {
                        MetricsError::ProcessConnectionLimitReached { limit }
                    } else {
                        MetricsError::CounterOverflow("process.activeConnections")
                    },
                )
            }
        }
    }
}

pub struct ProcessConnectionGuard {
    state: Arc<ProcessAdmissionState>,
}

impl Drop for ProcessConnectionGuard {
    fn drop(&mut self) {
        decrement_counter(&self.state.active);
    }
}

struct ListenerMetricsState {
    name: String,
    protocol: String,
    bind: String,
    max_connections: Option<u64>,
    runtime_state: AtomicU8,
    shared: Arc<SharedListenerMetricsState>,
}

const HTTP_RESULT_COUNT: usize = HttpOperationResult::ALL.len();
const TCP_RESULT_COUNT: usize = TcpRelayResult::ALL.len();
const PROXY_PROTOCOL_RESULT_COUNT: usize = ProxyProtocolResult::ALL.len();
const LATENCY_BUCKET_COUNT: usize = OPERATION_LATENCY_BUCKETS_MS.len() + 1;

struct OperationMetricsState {
    http_results: [AtomicU64; HTTP_RESULT_COUNT],
    http_latency_buckets: [AtomicU64; LATENCY_BUCKET_COUNT],
    http_latency_count: AtomicU64,
    http_latency_sum_ms: AtomicU64,
    tcp_results: [AtomicU64; TCP_RESULT_COUNT],
    tcp_latency_buckets: [AtomicU64; LATENCY_BUCKET_COUNT],
    tcp_latency_count: AtomicU64,
    tcp_latency_sum_ms: AtomicU64,
    proxy_protocol_results: [AtomicU64; PROXY_PROTOCOL_RESULT_COUNT],
    cache_events: [AtomicU64; 4],
}

impl OperationMetricsState {
    fn new() -> Self {
        Self {
            http_results: std::array::from_fn(|_| AtomicU64::new(0)),
            http_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            http_latency_count: AtomicU64::new(0),
            http_latency_sum_ms: AtomicU64::new(0),
            tcp_results: std::array::from_fn(|_| AtomicU64::new(0)),
            tcp_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            tcp_latency_count: AtomicU64::new(0),
            tcp_latency_sum_ms: AtomicU64::new(0),
            proxy_protocol_results: std::array::from_fn(|_| AtomicU64::new(0)),
            cache_events: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        checked_atomic_add(&self.http_results[result.index()], 1, "http.operations")?;
        record_latency(
            duration,
            &self.http_latency_buckets,
            &self.http_latency_count,
            &self.http_latency_sum_ms,
            "http.latency",
        )
    }

    fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        checked_atomic_add(&self.tcp_results[result.index()], 1, "tcp.relays")?;
        record_latency(
            duration,
            &self.tcp_latency_buckets,
            &self.tcp_latency_count,
            &self.tcp_latency_sum_ms,
            "tcp.latency",
        )
    }

    fn http_snapshot(&self) -> Option<HttpOperationSnapshot> {
        let latency = latency_snapshot(
            &self.http_latency_buckets,
            &self.http_latency_count,
            &self.http_latency_sum_ms,
        );
        (latency.count > 0).then(|| HttpOperationSnapshot {
            outcomes: HttpOperationResult::ALL
                .into_iter()
                .zip(&self.http_results)
                .map(|(result, count)| HttpOperationCountSnapshot {
                    result,
                    count: count.load(Ordering::Relaxed),
                })
                .collect(),
            latency,
        })
    }

    fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        checked_atomic_add(&self.cache_events[event.index()], 1, "http.cache")
    }

    fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.proxy_protocol_results[result.index()],
            1,
            "proxy_protocol.results",
        )
    }

    fn proxy_protocol_snapshot(&self) -> Option<ProxyProtocolSnapshot> {
        let values = ProxyProtocolResult::ALL.map(|result| {
            (
                result,
                self.proxy_protocol_results[result.index()].load(Ordering::Relaxed),
            )
        });
        values
            .iter()
            .any(|(_, count)| *count > 0)
            .then(|| ProxyProtocolSnapshot {
                outcomes: values
                    .into_iter()
                    .map(|(result, count)| ProxyProtocolCountSnapshot { result, count })
                    .collect(),
            })
    }

    fn cache_snapshot(&self) -> Option<CacheSnapshot> {
        let values =
            CacheEvent::ALL.map(|event| self.cache_events[event.index()].load(Ordering::Relaxed));
        values
            .iter()
            .any(|value| *value > 0)
            .then_some(CacheSnapshot {
                hits: values[0],
                misses: values[1],
                admissions: values[2],
                evictions: values[3],
            })
    }

    fn tcp_snapshot(&self) -> Option<TcpRelaySnapshot> {
        let latency = latency_snapshot(
            &self.tcp_latency_buckets,
            &self.tcp_latency_count,
            &self.tcp_latency_sum_ms,
        );
        (latency.count > 0).then(|| TcpRelaySnapshot {
            outcomes: TcpRelayResult::ALL
                .into_iter()
                .zip(&self.tcp_results)
                .map(|(result, count)| TcpRelayCountSnapshot {
                    result,
                    count: count.load(Ordering::Relaxed),
                })
                .collect(),
            latency,
        })
    }
}

const TRANSPORT_OUTCOME_COUNT: usize = TransportOutcome::ALL.len();
const TRANSPORT_LATENCY_BUCKET_COUNT: usize = OPERATION_LATENCY_BUCKETS_MS.len() + 1;

struct TransportMetricState {
    outcomes: [AtomicU64; TRANSPORT_OUTCOME_COUNT],
    latency_buckets: [AtomicU64; TRANSPORT_LATENCY_BUCKET_COUNT],
    latency_count: AtomicU64,
    latency_sum_ms: AtomicU64,
}

impl TransportMetricState {
    fn new() -> Self {
        Self {
            outcomes: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_count: AtomicU64::new(0),
            latency_sum_ms: AtomicU64::new(0),
        }
    }

    fn record(
        &self,
        outcome: TransportOutcome,
        duration: Option<Duration>,
    ) -> Result<(), MetricsError> {
        checked_atomic_add(&self.outcomes[outcome.index()], 1, "transport.outcomes")?;
        duration.map_or(Ok(()), |duration| {
            record_latency(
                duration,
                &self.latency_buckets,
                &self.latency_count,
                &self.latency_sum_ms,
                "transport.latency",
            )
        })
    }

    fn snapshot(&self, transport: ObservedTransport) -> Option<TransportOperationSnapshot> {
        let latency = latency_snapshot(
            &self.latency_buckets,
            &self.latency_count,
            &self.latency_sum_ms,
        );
        self.outcomes_present().then(|| TransportOperationSnapshot {
            transport,
            outcomes: TransportOutcome::ALL
                .into_iter()
                .zip(&self.outcomes)
                .map(|(outcome, count)| TransportOperationCountSnapshot {
                    outcome,
                    count: count.load(Ordering::Relaxed),
                })
                .collect(),
            latency,
        })
    }

    fn outcomes_present(&self) -> bool {
        self.outcomes
            .iter()
            .any(|count| count.load(Ordering::Relaxed) > 0)
    }
}

struct TransportOperationsState {
    metrics: [TransportMetricState; ObservedTransport::ALL.len()],
}

impl TransportOperationsState {
    fn new() -> Self {
        Self {
            metrics: std::array::from_fn(|_| TransportMetricState::new()),
        }
    }

    fn record(
        &self,
        transport: ObservedTransport,
        outcome: TransportOutcome,
        duration: Option<Duration>,
    ) -> Result<(), MetricsError> {
        self.metrics[transport.index()].record(outcome, duration)
    }

    fn snapshots(&self) -> Vec<TransportOperationSnapshot> {
        ObservedTransport::ALL
            .into_iter()
            .filter_map(|transport| self.metrics[transport.index()].snapshot(transport))
            .collect()
    }
}

fn event_transport_operations() -> &'static TransportOperationsState {
    static OPERATIONS: OnceLock<TransportOperationsState> = OnceLock::new();
    OPERATIONS.get_or_init(TransportOperationsState::new)
}

pub(crate) fn record_transport_event(transport: ObservedTransport, outcome: TransportOutcome) {
    let _ = event_transport_operations().record(transport, outcome, None);
}

pub(crate) fn transport_event_snapshots() -> Vec<TransportOperationSnapshot> {
    event_transport_operations().snapshots()
}

fn transport_for_protocol(protocol: &str) -> ObservedTransport {
    match protocol {
        "forward_http1" | "forward_http2" | "forward_http3" => ObservedTransport::Forward,
        "http3" => ObservedTransport::H3,
        "udp" => ObservedTransport::Udp,
        "tcp" => ObservedTransport::Tcp,
        _ => ObservedTransport::Http,
    }
}

struct SharedListenerMetricsState {
    administrative_state: AtomicU8,
    limit: AtomicU64,
    accepted_connections: AtomicU64,
    rejected_connections: AtomicU64,
    active_connections: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent: AtomicU64,
    operations: OperationMetricsState,
    transport_operations: Arc<TransportOperationsState>,
    access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
}

impl ListenerMetricsState {
    fn new(
        name: String,
        protocol: String,
        bind: String,
        max_connections: Option<u64>,
        shared: Arc<SharedListenerMetricsState>,
    ) -> Self {
        Self {
            name,
            protocol,
            bind,
            max_connections,
            runtime_state: AtomicU8::new(ListenerRuntimeState::Configured as u8),
            shared,
        }
    }

    fn snapshot(&self) -> ListenerSnapshot {
        ListenerSnapshot {
            administrative_state: AdministrativeState::from_u8(
                self.shared.administrative_state.load(Ordering::Acquire),
            ),
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            bind: self.bind.clone(),
            max_connections: self.shared.limit(),
            state: ListenerRuntimeState::from_u8(self.runtime_state.load(Ordering::Acquire)),
            accepted_connections: self.shared.accepted_connections.load(Ordering::Relaxed),
            rejected_connections: self.shared.rejected_connections.load(Ordering::Relaxed),
            active_connections: self.shared.active_connections.load(Ordering::Relaxed),
            bytes_received: self.shared.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.shared.bytes_sent.load(Ordering::Relaxed),
            http_operations: self.shared.operations.http_snapshot(),
            tcp_relays: self.shared.operations.tcp_snapshot(),
            proxy_protocol: self.shared.operations.proxy_protocol_snapshot(),
            cache: self.shared.operations.cache_snapshot(),
        }
    }
}

impl SharedListenerMetricsState {
    fn new(
        max_connections: Option<u64>,
        transport_operations: Arc<TransportOperationsState>,
        access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
    ) -> Self {
        Self {
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            limit: AtomicU64::new(encode_limit(max_connections)),
            accepted_connections: AtomicU64::new(0),
            rejected_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            operations: OperationMetricsState::new(),
            transport_operations,
            access_records,
        }
    }

    fn limit(&self) -> Option<u64> {
        decode_limit(self.limit.load(Ordering::Acquire))
    }

    fn set_limit(&self, limit: Option<u64>) {
        self.limit.store(encode_limit(limit), Ordering::Release);
    }

    fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.operations.record_http_operation(result, duration)
    }

    fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        self.operations.record_cache_event(event)
    }

    fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.operations.record_tcp_relay(result, duration)
    }

    fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.operations.record_proxy_protocol(result)
    }
}

const fn encode_limit(limit: Option<u64>) -> u64 {
    match limit {
        Some(limit) => limit,
        None => 0,
    }
}

const fn decode_limit(limit: u64) -> Option<u64> {
    if limit == 0 { None } else { Some(limit) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ListenerRuntimeState {
    Configured,
    Listening,
    Stopped,
    Failed,
}

impl ListenerRuntimeState {
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Listening,
            2 => Self::Stopped,
            3 => Self::Failed,
            _ => Self::Configured,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub sampled_at_unix_ms: u64,
    pub uptime_ms: u64,
    pub generation_age_ms: u64,
    pub process: ProcessSnapshot,
    pub host: HostSnapshot,
    pub traffic: TrafficSnapshot,
    pub listeners: Vec<ListenerSnapshot>,
    pub upstream_pools: Vec<PoolHealthSnapshot>,
    pub transport_operations: Vec<TransportOperationSnapshot>,
    pub access_records: Vec<AccessRecord>,
    pub certbot_certificates: Vec<CertbotCertificateSnapshot>,
    pub certbot_watcher: Option<CertbotWatcherSnapshot>,
    pub acme_managed_certificates: Vec<AcmeManagedCertificateSnapshot>,
    pub direct_file_certificates: Vec<DirectFileCertificateSnapshot>,
    pub direct_file_watcher: Option<DirectFileWatcherSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcmeManagedCertificateSnapshot {
    pub name: String,
    pub directory_url: String,
    pub disk_revision: String,
    pub active_revision: String,
    pub expires_at: String,
    pub not_before_unix_seconds: Option<u64>,
    pub not_after_unix_seconds: Option<u64>,
    pub next_action_unix_seconds: Option<u64>,
    pub last_outcome: Option<String>,
    pub last_error_code: Option<String>,
    pub renewal_information_status: String,
    pub dns_provider: Option<String>,
    pub dns_provider_deployment: Option<String>,
    pub dns_provider_health: Option<String>,
    pub dns_cleanup_status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertbotCertificateSnapshot {
    pub name: String,
    pub active_archive_revision: u64,
    pub active_content_revision: String,
    pub expires_at: String,
    pub last_outcome: Option<String>,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectFileCertificateSnapshot {
    pub name: String,
    pub active_content_revision: String,
    pub expires_at: String,
    pub last_outcome: Option<String>,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CertbotWatcherHealth {
    Healthy,
    Degraded,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertbotWatcherSnapshot {
    pub health: CertbotWatcherHealth,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub coalesced_events: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub ignored_access_events: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub backend_errors: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub watch_recoveries: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub watch_refreshes: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub rescans: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub periodic_rescans: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub reconciliation_failures: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectFileWatcherSnapshot {
    pub health: CertbotWatcherHealth,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub coalesced_events: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub ignored_access_events: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub backend_errors: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub watch_recoveries: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub watch_refreshes: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub rescans: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub periodic_rescans: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub reconciliation_failures: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub active_connections: u64,
    pub administrative_state: AdministrativeState,
    pub status: ComponentStatus,
    pub cpu_percent: Option<f64>,
    pub max_connections: Option<u64>,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub rejected_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub retry_attempts: u64,
    pub resident_memory_bytes: Option<u64>,
    pub virtual_memory_bytes: Option<u64>,
    pub thread_count: Option<u64>,
    pub open_file_descriptors: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSnapshot {
    pub status: ComponentStatus,
    pub load_average_1m: Option<f64>,
    pub load_average_5m: Option<f64>,
    pub load_average_15m: Option<f64>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSnapshot {
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub accepted_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub rejected_connections: u64,
    pub active_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_received: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_sent: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerSnapshot {
    pub administrative_state: AdministrativeState,
    pub name: String,
    pub protocol: String,
    pub bind: String,
    pub max_connections: Option<u64>,
    pub state: ListenerRuntimeState,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub accepted_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub rejected_connections: u64,
    pub active_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_received: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    pub bytes_sent: u64,
    pub http_operations: Option<HttpOperationSnapshot>,
    pub tcp_relays: Option<TcpRelaySnapshot>,
    pub proxy_protocol: Option<ProxyProtocolSnapshot>,
    pub cache: Option<CacheSnapshot>,
}

fn validate_listener_field(field: &'static str, value: &str) -> Result<(), MetricsError> {
    if value.trim().is_empty() {
        return Err(MetricsError::InvalidListenerField(field));
    }
    Ok(())
}

fn checked_atomic_add(
    counter: &AtomicU64,
    value: u64,
    name: &'static str,
) -> Result<(), MetricsError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(value)
        })
        .map(|_| ())
        .map_err(|_| MetricsError::CounterOverflow(name))
}

fn record_latency(
    duration: Duration,
    buckets: &[AtomicU64; LATENCY_BUCKET_COUNT],
    count: &AtomicU64,
    sum_ms: &AtomicU64,
    name: &'static str,
) -> Result<(), MetricsError> {
    let duration_ms = u64::try_from(duration.as_millis())
        .map_err(|_| MetricsError::ValueOutOfRange("operation duration milliseconds"))?;
    let bucket = OPERATION_LATENCY_BUCKETS_MS
        .iter()
        .position(|upper_bound| duration_ms <= *upper_bound)
        .unwrap_or(OPERATION_LATENCY_BUCKETS_MS.len());
    for bucket_count in buckets.iter().skip(bucket) {
        checked_atomic_add(bucket_count, 1, name)?;
    }
    checked_atomic_add(count, 1, name)?;
    checked_atomic_add(sum_ms, duration_ms, name)
}

fn latency_snapshot(
    buckets: &[AtomicU64; LATENCY_BUCKET_COUNT],
    count: &AtomicU64,
    sum_ms: &AtomicU64,
) -> LatencySnapshot {
    let buckets = buckets
        .iter()
        .enumerate()
        .map(|(index, count)| LatencyBucketSnapshot {
            upper_bound_ms: OPERATION_LATENCY_BUCKETS_MS.get(index).copied(),
            count: count.load(Ordering::Relaxed),
        })
        .collect();
    LatencySnapshot {
        buckets,
        count: count.load(Ordering::Relaxed),
        sum_ms: sum_ms.load(Ordering::Relaxed),
    }
}

fn decrement_counter(counter: &AtomicU64) {
    let result = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_sub(1)
    });
    debug_assert!(result.is_ok(), "active connection counter underflowed");
}

fn add_total(total: &mut u64, value: u64, name: &'static str) -> Result<(), MetricsError> {
    *total = total
        .checked_add(value)
        .ok_or(MetricsError::CounterOverflow(name))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuSample {
    process_ticks: u64,
    system_ticks: u64,
    logical_cpu_count: u32,
}

struct SystemSample {
    process_status: ComponentStatus,
    resident_memory_bytes: Option<u64>,
    virtual_memory_bytes: Option<u64>,
    thread_count: Option<u64>,
    open_file_descriptors: Option<u64>,
    host: HostSnapshot,
    cpu: CpuSample,
}

#[cfg(target_os = "linux")]
fn sample_system() -> Result<SystemSample, MetricsError> {
    const PROCESS_STAT: &str = "/proc/self/stat";
    const PROCESS_STATUS: &str = "/proc/self/status";
    const SYSTEM_STAT: &str = "/proc/stat";
    const LOAD_AVERAGE: &str = "/proc/loadavg";
    const MEMORY_INFO: &str = "/proc/meminfo";

    let process_ticks = parse_process_stat(&read_proc(PROCESS_STAT)?)?;
    let mut cpu = parse_system_stat(&read_proc(SYSTEM_STAT)?)?;
    cpu.process_ticks = process_ticks;
    let status = parse_process_status(&read_proc(PROCESS_STATUS)?)?;
    let load_average = parse_load_average(&read_proc(LOAD_AVERAGE)?)?;
    let memory = parse_memory_info(&read_proc(MEMORY_INFO)?)?;

    Ok(SystemSample {
        process_status: ComponentStatus::healthy(),
        resident_memory_bytes: Some(status.resident_memory_bytes),
        virtual_memory_bytes: Some(status.virtual_memory_bytes),
        thread_count: Some(status.thread_count),
        open_file_descriptors: Some(count_open_file_descriptors()?),
        host: HostSnapshot {
            status: ComponentStatus::healthy(),
            load_average_1m: Some(load_average[0]),
            load_average_5m: Some(load_average[1]),
            load_average_15m: Some(load_average[2]),
            total_memory_bytes: Some(memory.total_memory_bytes),
            available_memory_bytes: Some(memory.available_memory_bytes),
        },
        cpu,
    })
}

#[cfg(not(target_os = "linux"))]
fn sample_system() -> Result<SystemSample, MetricsError> {
    Ok(unsupported_system_sample())
}

#[cfg(any(not(target_os = "linux"), test))]
fn unsupported_system_sample() -> SystemSample {
    #[cfg(not(target_os = "linux"))]
    let status = ComponentStatus::unsupported("platform_not_supported");
    #[cfg(target_os = "linux")]
    let status = ComponentStatus {
        state: ComponentState::Unsupported,
        reason: Some("platform_not_supported"),
    };
    SystemSample {
        process_status: status,
        resident_memory_bytes: None,
        virtual_memory_bytes: None,
        thread_count: None,
        open_file_descriptors: None,
        host: HostSnapshot {
            status,
            load_average_1m: None,
            load_average_5m: None,
            load_average_15m: None,
            total_memory_bytes: None,
            available_memory_bytes: None,
        },
        cpu: CpuSample {
            process_ticks: 0,
            system_ticks: 0,
            logical_cpu_count: 0,
        },
    }
}

#[cfg(target_os = "linux")]
fn read_proc(path: &'static str) -> Result<String, MetricsError> {
    fs::read_to_string(path).map_err(|source| MetricsError::Io { path, source })
}

#[cfg(target_os = "linux")]
fn count_open_file_descriptors() -> Result<u64, MetricsError> {
    const FILE_DESCRIPTORS: &str = "/proc/self/fd";

    let entries = fs::read_dir(FILE_DESCRIPTORS).map_err(|source| MetricsError::Io {
        path: FILE_DESCRIPTORS,
        source,
    })?;
    let mut count = 0_u64;
    for entry in entries {
        entry.map_err(|source| MetricsError::Io {
            path: FILE_DESCRIPTORS,
            source,
        })?;
        count = count
            .checked_add(1)
            .ok_or(MetricsError::CounterOverflow("process.openFileDescriptors"))?;
    }

    // read_dir itself owns one descriptor that is visible in /proc/self/fd during enumeration.
    count.checked_sub(1).ok_or_else(|| {
        invalid_data(
            FILE_DESCRIPTORS,
            "enumeration did not include its own directory descriptor",
        )
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct ProcessStatus {
    resident_memory_bytes: u64,
    virtual_memory_bytes: u64,
    thread_count: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct MemoryInfo {
    total_memory_bytes: u64,
    available_memory_bytes: u64,
}

#[cfg(target_os = "linux")]
fn parse_process_stat(input: &str) -> Result<u64, MetricsError> {
    const RESOURCE: &str = "/proc/self/stat";

    let command_start = input
        .find('(')
        .ok_or_else(|| invalid_data(RESOURCE, "missing process command"))?;
    let command_end = input
        .rfind(')')
        .filter(|end| *end > command_start)
        .ok_or_else(|| invalid_data(RESOURCE, "unterminated process command"))?;
    parse_u64(input[..command_start].trim(), RESOURCE, "pid")?;

    let fields: Vec<_> = input[command_end + 1..].split_whitespace().collect();
    if fields.len() < 13 {
        return Err(invalid_data(
            RESOURCE,
            "expected fields through process system CPU time",
        ));
    }
    if fields[0].chars().count() != 1 {
        return Err(invalid_data(RESOURCE, "invalid process state field"));
    }
    let user_ticks = parse_u64(fields[11], RESOURCE, "utime")?;
    let system_ticks = parse_u64(fields[12], RESOURCE, "stime")?;
    user_ticks
        .checked_add(system_ticks)
        .ok_or_else(|| invalid_data(RESOURCE, "process CPU tick total overflowed"))
}

#[cfg(target_os = "linux")]
fn parse_system_stat(input: &str) -> Result<CpuSample, MetricsError> {
    const RESOURCE: &str = "/proc/stat";

    let mut system_ticks = None;
    let mut logical_cpu_count = 0_u32;
    for line in input.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        if name == "cpu" {
            if system_ticks.is_some() {
                return Err(invalid_data(RESOURCE, "duplicate aggregate CPU row"));
            }
            let mut total = 0_u64;
            for _ in 0..8 {
                let value = fields.next().ok_or_else(|| {
                    invalid_data(RESOURCE, "aggregate CPU row has fewer than eight counters")
                })?;
                total = total
                    .checked_add(parse_u64(value, RESOURCE, "aggregate CPU counter")?)
                    .ok_or_else(|| invalid_data(RESOURCE, "aggregate CPU tick total overflowed"))?;
            }
            system_ticks = Some(total);
        } else if name.strip_prefix("cpu").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            logical_cpu_count = logical_cpu_count
                .checked_add(1)
                .ok_or_else(|| invalid_data(RESOURCE, "logical CPU count overflowed"))?;
        }
    }

    let system_ticks =
        system_ticks.ok_or_else(|| invalid_data(RESOURCE, "missing aggregate CPU row"))?;
    if logical_cpu_count == 0 {
        return Err(invalid_data(RESOURCE, "missing logical CPU rows"));
    }
    Ok(CpuSample {
        process_ticks: 0,
        system_ticks,
        logical_cpu_count,
    })
}

#[cfg(target_os = "linux")]
fn parse_process_status(input: &str) -> Result<ProcessStatus, MetricsError> {
    const RESOURCE: &str = "/proc/self/status";

    let mut resident_memory_bytes = None;
    let mut virtual_memory_bytes = None;
    let mut thread_count = None;
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "VmRSS" => set_once(
                &mut resident_memory_bytes,
                parse_kib(value, RESOURCE, "VmRSS")?,
                RESOURCE,
                "VmRSS",
            )?,
            "VmSize" => set_once(
                &mut virtual_memory_bytes,
                parse_kib(value, RESOURCE, "VmSize")?,
                RESOURCE,
                "VmSize",
            )?,
            "Threads" => set_once(
                &mut thread_count,
                parse_single_u64(value, RESOURCE, "Threads")?,
                RESOURCE,
                "Threads",
            )?,
            _ => {}
        }
    }

    Ok(ProcessStatus {
        resident_memory_bytes: required(resident_memory_bytes, RESOURCE, "VmRSS")?,
        virtual_memory_bytes: required(virtual_memory_bytes, RESOURCE, "VmSize")?,
        thread_count: required(thread_count, RESOURCE, "Threads")?,
    })
}

#[cfg(target_os = "linux")]
fn parse_load_average(input: &str) -> Result<[f64; 3], MetricsError> {
    const RESOURCE: &str = "/proc/loadavg";

    let mut fields = input.split_whitespace();
    Ok([
        parse_load_value(fields.next(), RESOURCE, "one-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "five-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "fifteen-minute load average")?,
    ])
}

#[cfg(target_os = "linux")]
fn parse_memory_info(input: &str) -> Result<MemoryInfo, MetricsError> {
    const RESOURCE: &str = "/proc/meminfo";

    let mut total_memory_bytes = None;
    let mut available_memory_bytes = None;
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "MemTotal" => set_once(
                &mut total_memory_bytes,
                parse_kib(value, RESOURCE, "MemTotal")?,
                RESOURCE,
                "MemTotal",
            )?,
            "MemAvailable" => set_once(
                &mut available_memory_bytes,
                parse_kib(value, RESOURCE, "MemAvailable")?,
                RESOURCE,
                "MemAvailable",
            )?,
            _ => {}
        }
    }

    Ok(MemoryInfo {
        total_memory_bytes: required(total_memory_bytes, RESOURCE, "MemTotal")?,
        available_memory_bytes: required(available_memory_bytes, RESOURCE, "MemAvailable")?,
    })
}

#[cfg(target_os = "linux")]
fn set_once<T>(
    destination: &mut Option<T>,
    value: T,
    resource: &'static str,
    field: &'static str,
) -> Result<(), MetricsError> {
    if destination.replace(value).is_some() {
        return Err(invalid_data(resource, format!("duplicate `{field}` field")));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn required<T>(
    value: Option<T>,
    resource: &'static str,
    field: &'static str,
) -> Result<T, MetricsError> {
    value.ok_or_else(|| invalid_data(resource, format!("missing `{field}` field")))
}

#[cfg(target_os = "linux")]
fn parse_kib(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    let mut fields = value.split_whitespace();
    let kib = fields
        .next()
        .ok_or_else(|| invalid_data(resource, format!("missing `{field}` value")))?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return Err(invalid_data(
            resource,
            format!("`{field}` must contain one value in kB"),
        ));
    }
    parse_u64(kib, resource, field)?
        .checked_mul(1024)
        .ok_or_else(|| invalid_data(resource, format!("`{field}` byte value overflowed")))
}

#[cfg(target_os = "linux")]
fn parse_single_u64(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    let mut fields = value.split_whitespace();
    let value = fields
        .next()
        .ok_or_else(|| invalid_data(resource, format!("missing `{field}` value")))?;
    if fields.next().is_some() {
        return Err(invalid_data(
            resource,
            format!("`{field}` must contain one integer"),
        ));
    }
    parse_u64(value, resource, field)
}

#[cfg(target_os = "linux")]
fn parse_u64(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    value.parse().map_err(|_| {
        invalid_data(
            resource,
            format!("`{field}` contains invalid integer `{value}`"),
        )
    })
}

#[cfg(target_os = "linux")]
fn parse_load_value(
    value: Option<&str>,
    resource: &'static str,
    field: &'static str,
) -> Result<f64, MetricsError> {
    let value = value.ok_or_else(|| invalid_data(resource, format!("missing {field}")))?;
    let parsed: f64 = value.parse().map_err(|_| {
        invalid_data(
            resource,
            format!("{field} contains invalid number `{value}`"),
        )
    })?;
    if !parsed.is_finite() || parsed.is_sign_negative() {
        return Err(invalid_data(
            resource,
            format!("{field} must be a finite non-negative number"),
        ));
    }
    Ok(parsed)
}

fn invalid_data(resource: &'static str, detail: impl Into<String>) -> MetricsError {
    MetricsError::InvalidData {
        resource,
        detail: detail.into(),
    }
}

#[allow(clippy::cast_precision_loss)]
fn cpu_percent(
    previous: Option<&CpuSample>,
    current: &CpuSample,
) -> Result<Option<f64>, MetricsError> {
    let Some(previous) = previous else {
        return Ok(None);
    };
    let process_delta = current
        .process_ticks
        .checked_sub(previous.process_ticks)
        .ok_or_else(|| invalid_data("/proc/self/stat", "process CPU ticks moved backwards"))?;
    let system_delta = current
        .system_ticks
        .checked_sub(previous.system_ticks)
        .ok_or_else(|| invalid_data("/proc/stat", "aggregate CPU ticks moved backwards"))?;
    if system_delta == 0 {
        return Ok(None);
    }

    Ok(Some(
        process_delta as f64 / system_delta as f64 * f64::from(current.logical_cpu_count) * 100.0,
    ))
}

fn unix_time_ms() -> Result<u64, MetricsError> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| MetricsError::SystemClockBeforeUnixEpoch)?;
    u64::try_from(duration.as_millis())
        .map_err(|_| MetricsError::ValueOutOfRange("Unix timestamp milliseconds"))
}

#[cfg(test)]
mod platform_tests {
    use super::*;

    #[test]
    fn unsupported_platform_fixture_has_no_fabricated_samples() {
        let sample = unsupported_system_sample();

        assert_eq!(sample.process_status.state, ComponentState::Unsupported);
        assert_eq!(sample.host.status.state, ComponentState::Unsupported);
        assert_eq!(sample.resident_memory_bytes, None);
        assert_eq!(sample.host.total_memory_bytes, None);
        assert_eq!(sample.host.load_average_1m, None);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linux_architecture_fixture_samples_process_and_host_metrics() {
        let sample = sample_system().expect("Linux process and host fixture");

        assert_eq!(sample.process_status.state, ComponentState::Healthy);
        assert_eq!(sample.host.status.state, ComponentState::Healthy);
        assert!(sample.resident_memory_bytes.is_some());
        assert!(sample.host.total_memory_bytes.is_some());
        assert!(sample.host.load_average_1m.is_some());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn process_uptime_admission_and_listener_capacity_survive_generation_overlap() {
        let process = ProcessRuntime::new(Some(2));
        let first = RuntimeMetrics::for_process(process.clone());
        let first_listener = first
            .register_listener("old-name", "tcp", "socket:127.0.0.1:8080", Some(1))
            .expect("first listener");
        first.activate_limits(Some(2));
        let held = first_listener.begin_connection().expect("held connection");
        let first_uptime = first.snapshot().expect("first snapshot").uptime_ms;

        let second = RuntimeMetrics::for_process(process);
        let second_listener = second
            .register_listener("new-name", "tcp", "socket:127.0.0.1:8080", Some(1))
            .expect("second listener");
        second.activate_limits(Some(2));

        assert!(matches!(
            second_listener.begin_connection(),
            Err(MetricsError::ConnectionLimitReached { limit: 1, .. })
        ));
        assert!(second.snapshot().expect("second snapshot").uptime_ms >= first_uptime);
        second.set_process_administrative_state(AdministrativeState::Drain);
        assert!(!first_listener.accepting());
        drop(held);
    }

    #[test]
    fn listener_rejection_rolls_back_process_admission() {
        let runtime = RuntimeMetrics::with_max_connections(Some(2));
        let listener = runtime
            .register_listener("http", "http", "socket:127.0.0.1:8080", Some(1))
            .expect("listener");
        let admitted = listener.begin_connection().expect("first connection");
        assert!(matches!(
            listener.begin_connection(),
            Err(MetricsError::ConnectionLimitReached { limit: 1, .. })
        ));

        let process_only = runtime
            .begin_process_connection()
            .expect("listener rejection released process slot");
        assert!(matches!(
            runtime.begin_process_connection(),
            Err(MetricsError::ProcessConnectionLimitReached { limit: 2 })
        ));
        drop(process_only);
        drop(admitted);

        let listener = listener.state.snapshot();
        assert_eq!(listener.accepted_connections, 2);
        assert_eq!(listener.rejected_connections, 1);
        assert_eq!(listener.active_connections, 0);
        assert_eq!(
            runtime
                .inner
                .process_admission
                .active
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            runtime
                .inner
                .process_admission
                .rejected
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn parses_process_cpu_ticks_when_command_contains_parentheses() {
        let stat = "42 (worker ) name) S 1 2 3 4 5 6 7 8 9 10 120 30\n";

        assert_eq!(parse_process_stat(stat).expect("process stat"), 150);
    }

    #[test]
    fn parses_process_status_units() {
        let status = "Name:\toxiroute\nVmSize:\t2048 kB\nVmRSS:\t512 kB\nThreads:\t7\n";

        assert_eq!(
            parse_process_status(status).expect("process status"),
            ProcessStatus {
                resident_memory_bytes: 512 * 1024,
                virtual_memory_bytes: 2048 * 1024,
                thread_count: 7,
            }
        );
    }

    #[test]
    fn rejects_process_status_with_missing_metrics() {
        let error = parse_process_status("VmRSS:\t512 kB\nThreads:\t7\n")
            .expect_err("missing virtual memory must fail");

        assert!(error.to_string().contains("VmSize"));
    }

    #[test]
    fn parses_system_cpu_ticks_without_double_counting_guest_fields() {
        let stat = "cpu  1 2 3 4 5 6 7 8 100 200\ncpu0 1 2 3 4 5 6 7 8 50 100\ncpu1 0 0 0 0 0 0 0 0 0 0\nintr 12\n";

        assert_eq!(
            parse_system_stat(stat).expect("system stat"),
            CpuSample {
                process_ticks: 0,
                system_ticks: 36,
                logical_cpu_count: 2,
            }
        );
    }

    #[test]
    fn parses_load_and_memory_fixtures() {
        let load_average = parse_load_average("0.25 1.50 2.75 1/100 42\n").expect("load average");

        assert!((load_average[0] - 0.25).abs() < f64::EPSILON);
        assert!((load_average[1] - 1.5).abs() < f64::EPSILON);
        assert!((load_average[2] - 2.75).abs() < f64::EPSILON);
        assert_eq!(
            parse_memory_info("MemTotal: 4096 kB\nMemFree: 100 kB\nMemAvailable: 1024 kB\n")
                .expect("memory info"),
            MemoryInfo {
                total_memory_bytes: 4096 * 1024,
                available_memory_bytes: 1024 * 1024,
            }
        );
    }

    #[test]
    fn computes_cpu_percent_from_process_and_aggregate_ticks() {
        let previous = CpuSample {
            process_ticks: 100,
            system_ticks: 10_000,
            logical_cpu_count: 4,
        };
        let current = CpuSample {
            process_ticks: 125,
            system_ticks: 10_100,
            logical_cpu_count: 4,
        };

        assert_eq!(
            cpu_percent(Some(&previous), &current).expect("CPU percent"),
            Some(100.0)
        );
        assert_eq!(cpu_percent(None, &current).expect("first sample"), None);
    }
}
