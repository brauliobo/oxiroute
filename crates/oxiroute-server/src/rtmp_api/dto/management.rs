use schemars::JsonSchema;
use serde::Serialize;

use super::DecimalCounter;
use crate::config_coordinator::{ConfigRejection, RejectionDiagnostic};
use crate::{
    AdministrativeState, CacheSnapshot, EndpointHealthSnapshot, EndpointHealthState,
    GenerationStatus, HealthFailure, HealthOverride, HttpOperationCountSnapshot,
    HttpOperationResult, HttpOperationSnapshot, LatencyBucketSnapshot, LatencySnapshot,
    ListenerRuntimeState, ListenerSnapshot, PoolHealthSnapshot, ProxyProtocolCountSnapshot,
    ProxyProtocolResult, ProxyProtocolSnapshot, RuntimeSnapshot, TcpRelayCountSnapshot,
    TcpRelayResult, TcpRelaySnapshot,
};

#[derive(JsonSchema, Serialize)]
pub(crate) struct GenerationResponse {
    generation: GenerationStatusDto,
}

impl From<GenerationStatus> for GenerationResponse {
    fn from(status: GenerationStatus) -> Self {
        Self {
            generation: status.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationStatusDto {
    build_version: &'static str,
    disk_revision: Option<String>,
    candidate_revision: Option<String>,
    active_revision: Option<String>,
    previous_revision: Option<String>,
    quarantined_revision: Option<String>,
    generation: u64,
    active_accepting: bool,
    degraded: bool,
    last_failure: Option<&'static str>,
    last_rejection: Option<ConfigRejectionDto>,
    prepares: u64,
    activations: u64,
    config_rejections: u64,
    failures: u64,
    rollbacks: u64,
    retired_generations: Vec<RetiredGenerationDto>,
    max_retired_generations: usize,
    pending_apply: Option<PendingApplyDto>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetiredGenerationDto {
    revision: String,
    state: &'static str,
    age_seconds: u64,
    deadline_remaining_seconds: u64,
    in_flight: u64,
    in_flight_by_kind: std::collections::BTreeMap<&'static str, u64>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingApplyDto {
    revision: String,
    reason: &'static str,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigRejectionDto {
    at_unix_ms: u64,
    revision: Option<String>,
    diagnostics: Vec<RejectionDiagnostic>,
}

impl From<ConfigRejection> for ConfigRejectionDto {
    fn from(rejection: ConfigRejection) -> Self {
        Self {
            at_unix_ms: rejection.at_unix_ms,
            revision: rejection.revision,
            diagnostics: rejection.diagnostics,
        }
    }
}

impl From<GenerationStatus> for GenerationStatusDto {
    fn from(status: GenerationStatus) -> Self {
        Self {
            build_version: status.build_version,
            disk_revision: authored_revision(status.disk_revision),
            candidate_revision: effective_revision(status.candidate_revision),
            active_revision: effective_revision(status.active_revision),
            previous_revision: effective_revision(status.previous_revision),
            quarantined_revision: effective_revision(status.quarantined_revision),
            generation: status.generation,
            active_accepting: status.active_accepting,
            degraded: status.degraded,
            last_failure: status.last_failure,
            last_rejection: status.last_rejection.map(Into::into),
            prepares: status.prepares,
            activations: status.activations,
            config_rejections: status.config_rejections,
            failures: status.failures,
            rollbacks: status.rollbacks,
            retired_generations: status
                .retired_generations
                .into_iter()
                .map(Into::into)
                .collect(),
            max_retired_generations: status.max_retired_generations,
            pending_apply: status.pending_apply.map(Into::into),
        }
    }
}

impl From<crate::RetiredGenerationStatus> for RetiredGenerationDto {
    fn from(retired: crate::RetiredGenerationStatus) -> Self {
        Self {
            revision: retired.revision.as_str().to_owned(),
            state: retired.state.as_str(),
            age_seconds: retired.age_seconds,
            deadline_remaining_seconds: retired.deadline_remaining_seconds,
            in_flight: retired.in_flight,
            in_flight_by_kind: retired.in_flight_by_kind,
        }
    }
}

impl From<crate::PendingApply> for PendingApplyDto {
    fn from(pending: crate::PendingApply) -> Self {
        Self {
            revision: pending.revision.as_str().to_owned(),
            reason: pending.reason,
        }
    }
}

fn authored_revision(value: Option<crate::config_coordinator::AuthoredRevision>) -> Option<String> {
    value.map(|revision| revision.as_str().to_owned())
}

fn effective_revision(
    value: Option<crate::config_coordinator::EffectiveRevision>,
) -> Option<String> {
    value.map(|revision| revision.as_str().to_owned())
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct ListenerInventoryResponse {
    listeners: Vec<ListenerDto>,
}

impl From<RuntimeSnapshot> for ListenerInventoryResponse {
    fn from(snapshot: RuntimeSnapshot) -> Self {
        Self {
            listeners: snapshot.listeners.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct PoolInventoryResponse {
    pools: Vec<PoolDto>,
}

impl From<Vec<PoolHealthSnapshot>> for PoolInventoryResponse {
    fn from(pools: Vec<PoolHealthSnapshot>) -> Self {
        Self {
            pools: pools.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct ServerInventoryResponse {
    servers: Vec<ServerInventoryEntry>,
}

impl From<Vec<PoolHealthSnapshot>> for ServerInventoryResponse {
    fn from(pools: Vec<PoolHealthSnapshot>) -> Self {
        let servers = pools
            .into_iter()
            .flat_map(|pool| {
                let name = pool.name;
                pool.endpoints
                    .into_iter()
                    .map(move |server| ServerInventoryEntry {
                        pool: name.clone(),
                        server: server.into(),
                    })
            })
            .collect();
        Self { servers }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListenerDto {
    administrative_state: AdministrativeStateDto,
    name: String,
    protocol: String,
    bind: String,
    max_connections: Option<u64>,
    state: ListenerRuntimeStateDto,
    accepted_connections: DecimalCounter,
    rejected_connections: DecimalCounter,
    active_connections: u64,
    bytes_received: DecimalCounter,
    bytes_sent: DecimalCounter,
    http_operations: Option<HttpOperationDto>,
    tcp_relays: Option<TcpRelayDto>,
    proxy_protocol: Option<ProxyProtocolDto>,
    cache: Option<CacheDto>,
}

impl From<ListenerSnapshot> for ListenerDto {
    fn from(listener: ListenerSnapshot) -> Self {
        Self {
            administrative_state: listener.administrative_state.into(),
            name: listener.name,
            protocol: listener.protocol,
            bind: listener.bind,
            max_connections: listener.max_connections,
            state: listener.state.into(),
            accepted_connections: listener.accepted_connections.into(),
            rejected_connections: listener.rejected_connections.into(),
            active_connections: listener.active_connections,
            bytes_received: listener.bytes_received.into(),
            bytes_sent: listener.bytes_sent.into(),
            http_operations: listener.http_operations.map(Into::into),
            tcp_relays: listener.tcp_relays.map(Into::into),
            proxy_protocol: listener.proxy_protocol.map(Into::into),
            cache: listener.cache.map(Into::into),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpOperationDto {
    outcomes: Vec<HttpOperationCountDto>,
    latency: LatencyDto,
}

impl From<HttpOperationSnapshot> for HttpOperationDto {
    fn from(snapshot: HttpOperationSnapshot) -> Self {
        Self {
            outcomes: snapshot
                .outcomes
                .into_vec()
                .into_iter()
                .map(Into::into)
                .collect(),
            latency: snapshot.latency.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct HttpOperationCountDto {
    result: HttpOperationResultDto,
    count: DecimalCounter,
}

impl From<HttpOperationCountSnapshot> for HttpOperationCountDto {
    fn from(snapshot: HttpOperationCountSnapshot) -> Self {
        Self {
            result: snapshot.result.into(),
            count: snapshot.count.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct TcpRelayDto {
    outcomes: Vec<TcpRelayCountDto>,
    latency: LatencyDto,
}

impl From<TcpRelaySnapshot> for TcpRelayDto {
    fn from(snapshot: TcpRelaySnapshot) -> Self {
        Self {
            outcomes: snapshot
                .outcomes
                .into_vec()
                .into_iter()
                .map(Into::into)
                .collect(),
            latency: snapshot.latency.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct TcpRelayCountDto {
    result: TcpRelayResultDto,
    count: DecimalCounter,
}

impl From<TcpRelayCountSnapshot> for TcpRelayCountDto {
    fn from(snapshot: TcpRelayCountSnapshot) -> Self {
        Self {
            result: snapshot.result.into(),
            count: snapshot.count.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct ProxyProtocolDto {
    outcomes: Vec<ProxyProtocolCountDto>,
}

impl From<ProxyProtocolSnapshot> for ProxyProtocolDto {
    fn from(snapshot: ProxyProtocolSnapshot) -> Self {
        Self {
            outcomes: snapshot
                .outcomes
                .into_vec()
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct ProxyProtocolCountDto {
    result: ProxyProtocolResultDto,
    count: DecimalCounter,
}

impl From<ProxyProtocolCountSnapshot> for ProxyProtocolCountDto {
    fn from(snapshot: ProxyProtocolCountSnapshot) -> Self {
        Self {
            result: snapshot.result.into(),
            count: snapshot.count.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LatencyDto {
    buckets: Vec<LatencyBucketDto>,
    count: DecimalCounter,
    sum_ms: DecimalCounter,
}

impl From<LatencySnapshot> for LatencyDto {
    fn from(snapshot: LatencySnapshot) -> Self {
        Self {
            buckets: snapshot
                .buckets
                .into_vec()
                .into_iter()
                .map(Into::into)
                .collect(),
            count: snapshot.count.into(),
            sum_ms: snapshot.sum_ms.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct LatencyBucketDto {
    upper_bound_ms: Option<u64>,
    count: DecimalCounter,
}

impl From<LatencyBucketSnapshot> for LatencyBucketDto {
    fn from(snapshot: LatencyBucketSnapshot) -> Self {
        Self {
            upper_bound_ms: snapshot.upper_bound_ms,
            count: snapshot.count.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct CacheDto {
    hits: DecimalCounter,
    misses: DecimalCounter,
    admissions: DecimalCounter,
    evictions: DecimalCounter,
}

impl From<CacheSnapshot> for CacheDto {
    fn from(snapshot: CacheSnapshot) -> Self {
        Self {
            hits: snapshot.hits.into(),
            misses: snapshot.misses.into(),
            admissions: snapshot.admissions.into(),
            evictions: snapshot.evictions.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PoolDto {
    name: String,
    algorithm: &'static str,
    available_endpoints: usize,
    total_endpoints: usize,
    unavailable_selections: DecimalCounter,
    fail_open_selections: DecimalCounter,
    queued: u64,
    queued_total: DecimalCounter,
    queue_timeouts: DecimalCounter,
    queue_cancellations: DecimalCounter,
    endpoints: Vec<EndpointDto>,
}

impl From<PoolHealthSnapshot> for PoolDto {
    fn from(snapshot: PoolHealthSnapshot) -> Self {
        Self {
            name: snapshot.name,
            algorithm: snapshot.algorithm,
            available_endpoints: snapshot.available_endpoints,
            total_endpoints: snapshot.total_endpoints,
            unavailable_selections: snapshot.unavailable_selections.into(),
            fail_open_selections: snapshot.fail_open_selections.into(),
            queued: snapshot.queued,
            queued_total: snapshot.queued_total.into(),
            queue_timeouts: snapshot.queue_timeouts.into(),
            queue_cancellations: snapshot.queue_cancellations.into(),
            endpoints: snapshot.endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct ServerInventoryEntry {
    pool: String,
    server: EndpointDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointDto {
    active_connections: DecimalCounter,
    administrative_state: AdministrativeStateDto,
    address: String,
    checks_enabled: bool,
    checks_running: bool,
    configured_max_connections: Option<u64>,
    health_override: HealthOverrideDto,
    max_connections: Option<u64>,
    name: String,
    origin: crate::ServerOrigin,
    state: EndpointHealthStateDto,
    weight: u16,
    last_checked_at_unix_ms: Option<u64>,
    last_transition_at_unix_ms: Option<u64>,
    successful_checks: DecimalCounter,
    failed_checks: DecimalCounter,
    consecutive_successes: DecimalCounter,
    consecutive_failures: DecimalCounter,
    last_failure: Option<HealthFailureDto>,
    passive_ejected: bool,
    passive_failure_count: DecimalCounter,
    passive_consecutive_failures: DecimalCounter,
    passive_ejection_count: DecimalCounter,
    passive_ejection_reason: Option<HealthFailureDto>,
    passive_ejected_at_unix_ms: Option<u64>,
    passive_ejection_until_unix_ms: Option<u64>,
    passive_recovery_count: DecimalCounter,
    passive_last_recovery_at_unix_ms: Option<u64>,
}

impl From<EndpointHealthSnapshot> for EndpointDto {
    fn from(snapshot: EndpointHealthSnapshot) -> Self {
        Self {
            active_connections: snapshot.active_connections.into(),
            administrative_state: snapshot.administrative_state.into(),
            address: snapshot.address.to_string(),
            checks_enabled: snapshot.checks_enabled,
            checks_running: snapshot.checks_running,
            configured_max_connections: snapshot.configured_max_connections,
            health_override: snapshot.health_override.into(),
            max_connections: snapshot.max_connections,
            name: snapshot.name,
            origin: snapshot.origin,
            state: snapshot.state.into(),
            weight: snapshot.weight,
            last_checked_at_unix_ms: snapshot.last_checked_at_unix_ms,
            last_transition_at_unix_ms: snapshot.last_transition_at_unix_ms,
            successful_checks: snapshot.successful_checks.into(),
            failed_checks: snapshot.failed_checks.into(),
            consecutive_successes: snapshot.consecutive_successes.into(),
            consecutive_failures: snapshot.consecutive_failures.into(),
            last_failure: snapshot.last_failure.map(Into::into),
            passive_ejected: snapshot.passive_ejected,
            passive_failure_count: snapshot.passive_failure_count.into(),
            passive_consecutive_failures: snapshot.passive_consecutive_failures.into(),
            passive_ejection_count: snapshot.passive_ejection_count.into(),
            passive_ejection_reason: snapshot.passive_ejection_reason.map(Into::into),
            passive_ejected_at_unix_ms: snapshot.passive_ejected_at_unix_ms,
            passive_ejection_until_unix_ms: snapshot.passive_ejection_until_unix_ms,
            passive_recovery_count: snapshot.passive_recovery_count.into(),
            passive_last_recovery_at_unix_ms: snapshot.passive_last_recovery_at_unix_ms,
        }
    }
}

macro_rules! api_enum {
    ($name:ident from $domain:ty { $($source:path => $target:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        enum $name {
            $($target),+
        }

        impl From<$domain> for $name {
            fn from(value: $domain) -> Self {
                match value {
                    $($source => Self::$target),+
                }
            }
        }
    };
}

api_enum!(AdministrativeStateDto from AdministrativeState {
    AdministrativeState::Ready => Ready,
    AdministrativeState::Drain => Drain,
    AdministrativeState::Maintenance => Maintenance,
});

api_enum!(ListenerRuntimeStateDto from ListenerRuntimeState {
    ListenerRuntimeState::Configured => Configured,
    ListenerRuntimeState::Listening => Listening,
    ListenerRuntimeState::Stopped => Stopped,
    ListenerRuntimeState::Failed => Failed,
});

api_enum!(EndpointHealthStateDto from EndpointHealthState {
    EndpointHealthState::Unchecked => Unchecked,
    EndpointHealthState::Unknown => Unknown,
    EndpointHealthState::Healthy => Healthy,
    EndpointHealthState::Unhealthy => Unhealthy,
});

api_enum!(HealthOverrideDto from HealthOverride {
    HealthOverride::Auto => Auto,
    HealthOverride::Up => Up,
    HealthOverride::Down => Down,
});

api_enum!(HealthFailureDto from HealthFailure {
    HealthFailure::Timeout => Timeout,
    HealthFailure::ConnectFailed => ConnectFailed,
    HealthFailure::UnexpectedStatus => UnexpectedStatus,
    HealthFailure::ProtocolError => ProtocolError,
});

api_enum!(HttpOperationResultDto from HttpOperationResult {
    HttpOperationResult::Success => Success,
    HttpOperationResult::ClientError => ClientError,
    HttpOperationResult::ServerError => ServerError,
    HttpOperationResult::UpstreamError => UpstreamError,
    HttpOperationResult::Timeout => Timeout,
    HttpOperationResult::Cancelled => Cancelled,
    HttpOperationResult::InternalError => InternalError,
});

api_enum!(TcpRelayResultDto from TcpRelayResult {
    TcpRelayResult::Success => Success,
    TcpRelayResult::ConnectError => ConnectError,
    TcpRelayResult::ConnectTimeout => ConnectTimeout,
    TcpRelayResult::IdleTimeout => IdleTimeout,
    TcpRelayResult::LifetimeTimeout => LifetimeTimeout,
    TcpRelayResult::Cancelled => Cancelled,
    TcpRelayResult::IoError => IoError,
    TcpRelayResult::AccountingError => AccountingError,
    TcpRelayResult::ProxyProtocolError => ProxyProtocolError,
});

api_enum!(ProxyProtocolResultDto from ProxyProtocolResult {
    ProxyProtocolResult::Accepted => Accepted,
    ProxyProtocolResult::Sent => Sent,
    ProxyProtocolResult::Timeout => Timeout,
    ProxyProtocolResult::Cancelled => Cancelled,
    ProxyProtocolResult::Malformed => Malformed,
    ProxyProtocolResult::Unsupported => Unsupported,
    ProxyProtocolResult::Mismatch => Mismatch,
    ProxyProtocolResult::IoError => IoError,
});

#[cfg(test)]
mod tests;
