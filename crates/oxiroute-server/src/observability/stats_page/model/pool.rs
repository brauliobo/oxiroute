use schemars::JsonSchema;
use serde::Serialize;

use super::{
    AdministrativeState, CanaryResult, Counter, HealthFailure, HealthOverride, HealthState,
    Histogram, PoolStatus, ServerOrigin, ServerStatus,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PoolStats {
    pub(crate) name: String,
    pub(crate) algorithm: String,
    pub(crate) status: PoolStatus,
    pub(crate) available_servers: u64,
    pub(crate) total_servers: u64,
    /// Selections that found no available server.
    pub(crate) unavailable_selections: Counter,
    /// Selections that used an unavailable server because failing open was allowed.
    pub(crate) fail_open_selections: Counter,
    pub(crate) queue: QueueStats,
    pub(crate) inflight: InflightStats,
    /// Sum over the servers.
    pub(crate) traffic: TrafficStats,
    /// Sum over the servers.
    pub(crate) latency: LatencyStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canary: Option<CanaryStats>,
    pub(crate) servers: Vec<ServerStats>,
}

/// Requests waiting for server capacity. The proxy bounds the wait by time (`timeoutMs`), not by depth,
/// so there is no queue capacity.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueStats {
    pub(crate) current: u64,
    /// Waiting requests per queue class; absent for pools without classes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) by_class: Option<Vec<ClassCount>>,
    pub(crate) oldest_wait_ms: u64,
    /// Requests that ever waited.
    pub(crate) total: Counter,
    pub(crate) timeouts: Counter,
    pub(crate) cancellations: Counter,
    /// The configured limit of the wait; absent when the pool has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
    /// Time requests spent waiting.
    pub(crate) wait: Histogram,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct ClassCount {
    pub(crate) class: String,
    pub(crate) count: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InflightStats {
    pub(crate) requests: u64,
    pub(crate) oldest_age_ms: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LatencyStats {
    /// Until the response finished.
    pub(crate) request: Histogram,
    /// Until the response headers arrived.
    pub(crate) header: Histogram,
}

/// What the proxy counted for a server, or for a whole pool as the sum over its servers.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficStats {
    pub(crate) sessions: SessionStats,
    /// Bytes sent to the server.
    pub(crate) bytes_sent: Counter,
    /// Bytes received from the server.
    pub(crate) bytes_received: Counter,
    pub(crate) responses: ResponseStats,
    pub(crate) attempts: AttemptStats,
    pub(crate) evacuated: EvacuationStats,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionStats {
    pub(crate) current: u64,
    /// Every attempt made on the server, whatever its outcome.
    pub(crate) total: Counter,
}

/// Responses by status class; `none` counts attempts that ended without a response.
#[derive(JsonSchema, Serialize)]
pub(crate) struct ResponseStats {
    #[serde(rename = "1xx")]
    pub(crate) c1xx: Counter,
    #[serde(rename = "2xx")]
    pub(crate) c2xx: Counter,
    #[serde(rename = "3xx")]
    pub(crate) c3xx: Counter,
    #[serde(rename = "4xx")]
    pub(crate) c4xx: Counter,
    #[serde(rename = "5xx")]
    pub(crate) c5xx: Counter,
    pub(crate) none: Counter,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttemptStats {
    pub(crate) ok: Counter,
    pub(crate) connect_failed: Counter,
    pub(crate) timeout: Counter,
    pub(crate) protocol_error: Counter,
    /// Attempts retried because of the response status.
    pub(crate) status_retry: Counter,
    pub(crate) rescued: Counter,
    pub(crate) client_closed: Counter,
    pub(crate) evacuated: Counter,
}

/// What an evacuation did to the requests of a server.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvacuationStats {
    pub(crate) redispatched: Counter,
    pub(crate) left_running: Counter,
    pub(crate) forced: Counter,
    pub(crate) failed: Counter,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CanaryStats {
    /// Absent until the first run that was not skipped for capacity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<CanaryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<HealthFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) server: Option<String>,
    pub(crate) duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_attempt_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_success_unix_ms: Option<u64>,
    pub(crate) consecutive_failures: u64,
    pub(crate) interval_ms: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerStats {
    pub(crate) name: String,
    /// The configured upstream endpoint: socket address, `host:port` or Unix socket path.
    pub(crate) address: String,
    pub(crate) origin: ServerOrigin,
    pub(crate) status: ServerStatus,
    /// Whether the balancer may pick the server now.
    pub(crate) available: bool,
    pub(crate) administrative_state: AdministrativeState,
    pub(crate) health_override: HealthOverride,
    pub(crate) health: HealthState,
    pub(crate) weight: u16,
    pub(crate) configured_weight: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_connections: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) configured_max_connections: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_transition_at_unix_ms: Option<u64>,
    /// How long the server has been unhealthy; absent while it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) downtime_ms: Option<u64>,
    pub(crate) check: CheckStats,
    pub(crate) passive: PassiveStats,
    pub(crate) traffic: TrafficStats,
    pub(crate) latency: LatencyStats,
}

/// Active health checks. The proxy does not record how long a check took.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckStats {
    pub(crate) enabled: bool,
    pub(crate) running: bool,
    pub(crate) successful: Counter,
    pub(crate) failed: Counter,
    pub(crate) consecutive_successes: u64,
    pub(crate) consecutive_failures: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_checked_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_failure: Option<HealthFailure>,
}

/// Ejections driven by what real requests experienced.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PassiveStats {
    pub(crate) ejected: bool,
    pub(crate) failure_count: Counter,
    pub(crate) consecutive_failures: u64,
    pub(crate) ejection_count: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ejection_reason: Option<HealthFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ejected_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ejection_until_unix_ms: Option<u64>,
    pub(crate) recovery_count: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_recovery_at_unix_ms: Option<u64>,
}
