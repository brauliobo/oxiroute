#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(crate) struct RuntimeHealthSnapshot {
    pub sampled_at_unix_ms: u64,
    pub listeners: Vec<ListenerSnapshot>,
    pub upstream_pools: Vec<PoolHealthSnapshot>,
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
    pub(super) const fn from_u8(value: u8) -> Self {
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
