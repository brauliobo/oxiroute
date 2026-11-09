//! The wire shape of the stats page snapshot: one document for every client of the page listener.
//!
//! Conventions, all part of the contract:
//! - keys are camelCase;
//! - monotonic counters are decimal strings ([`Counter`]) so no client loses precision; gauges and
//!   timestamps are JSON numbers;
//! - a key is omitted when the proxy does not know the value. Nothing is ever reported as zero
//!   because it is unknown, so clients must treat an absent key as "not available".

use schemars::JsonSchema;
use serde::Serialize;

use crate::rtmp::api::dto::DecimalCounter;

mod certificate;
mod enums;
mod listener;
mod pool;
mod route;

pub(crate) use certificate::{CertificateStats, CertificateWatcherStats};
pub(crate) use enums::*;
pub(crate) use listener::{
    CacheStats, HttpStats, ListenerStats, OutcomeCount, ProxyProtocolStats, TcpRelayStats,
};
pub(crate) use pool::{
    AttemptStats, CanaryStats, CheckStats, ClassCount, EvacuationStats, InflightStats,
    LatencyStats, PassiveStats, PoolStats, QueueStats, ResponseStats, ServerStats, SessionStats,
    TrafficStats,
};
pub(crate) use route::{PoolSelectionCount, RescueCount, RouteStats, Selector};

pub(crate) const SCHEMA_VERSION: u32 = 1;

/// A monotonic counter as an unsigned decimal string.
pub(crate) type Counter = DecimalCounter;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatsSnapshot {
    /// Bumped on any incompatible change of this document.
    pub(crate) schema_version: u32,
    pub(crate) sampled_at_unix_ms: u64,
    /// How often a client should poll; never below one second, the server's own refresh floor.
    pub(crate) interval_hint_ms: u64,
    pub(crate) process: ProcessStats,
    pub(crate) generation: GenerationStats,
    pub(crate) bucket_bounds_ms: BucketBounds,
    pub(crate) listeners: Vec<ListenerStats>,
    pub(crate) routes: Vec<RouteStats>,
    pub(crate) pools: Vec<PoolStats>,
    pub(crate) certificates: Vec<CertificateStats>,
    pub(crate) certificate_watchers: Vec<CertificateWatcherStats>,
    pub(crate) warnings: Vec<WarningStats>,
}

/// What a client may do: serialised next to the cached snapshot because it depends on who asks.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatsDocument<'a> {
    /// True when this client may post administrative state changes: a loopback peer of a page with
    /// `admin = "localhost"`. The form then needs `generation.activeRevision`.
    pub(crate) admin: bool,
    #[serde(flatten)]
    pub(crate) snapshot: &'a StatsSnapshot,
}

/// Upper bounds, in milliseconds, of the cumulative buckets of every [`Histogram`]. The overflow
/// bucket is `count`.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BucketBounds {
    /// Request, header and relay latencies.
    pub(crate) operation: Vec<u64>,
    /// Time spent waiting in a pool queue.
    pub(crate) queue_wait: Vec<u64>,
}

/// A latency distribution. `buckets[i]` is the cumulative number of observations at or below the
/// i-th bound of the matching [`BucketBounds`] list; percentiles read from it are bucket upper bounds.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Histogram {
    pub(crate) count: Counter,
    pub(crate) sum_ms: Counter,
    pub(crate) buckets: Vec<Counter>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessStats {
    pub(crate) version: String,
    pub(crate) uptime_ms: u64,
    pub(crate) state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state_reason: Option<String>,
    pub(crate) administrative_state: AdministrativeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpu_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resident_memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) virtual_memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thread_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) open_file_descriptors: Option<u64>,
    pub(crate) active_connections: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_connections: Option<u64>,
    /// Connections accepted by the public listeners.
    pub(crate) accepted_connections: Counter,
    pub(crate) rejected_connections: Counter,
    /// Bytes read from clients by the public listeners.
    pub(crate) bytes_received: Counter,
    /// Bytes written to clients by the public listeners.
    pub(crate) bytes_sent: Counter,
    pub(crate) retry_attempts: Counter,
    pub(crate) inflight: ProcessInflight,
    pub(crate) host: HostStats,
}

/// Requests currently waiting for or served by an upstream server.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessInflight {
    pub(crate) upstream_requests: u64,
    pub(crate) oldest_upstream_age_ms: u64,
    /// Requests past their deadline.
    pub(crate) overdue: u64,
    /// Requests the watchdog has reported as stuck.
    pub(crate) stuck: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostStats {
    pub(crate) state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) load_average_1m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) load_average_5m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) load_average_15m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) available_memory_bytes: Option<u64>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerationStats {
    /// Number of the live generation.
    pub(crate) number: u64,
    pub(crate) age_ms: u64,
    pub(crate) accepting: bool,
    pub(crate) degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) active_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) candidate_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) previous_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disk_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) quarantined_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_rejection: Option<RejectionStats>,
    pub(crate) prepares: Counter,
    pub(crate) activations: Counter,
    pub(crate) config_rejections: Counter,
    pub(crate) failures: Counter,
    pub(crate) rollbacks: Counter,
    pub(crate) max_retired_generations: u64,
    /// Replaced generations that still drain their connections, oldest first.
    pub(crate) retired: Vec<RetiredGeneration>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RejectionStats {
    pub(crate) at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
    pub(crate) diagnostics: Vec<DiagnosticStats>,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct DiagnosticStats {
    pub(crate) code: String,
    pub(crate) stage: String,
    pub(crate) message: String,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetiredGeneration {
    pub(crate) revision: String,
    pub(crate) state: RetiredState,
    pub(crate) age_seconds: u64,
    pub(crate) deadline_remaining_seconds: u64,
    pub(crate) in_flight: u64,
    /// In-flight work by protocol; the key set is open.
    pub(crate) in_flight_by_kind: std::collections::BTreeMap<String, u64>,
}

/// A valid setting that cannot behave the way it reads.
#[derive(JsonSchema, Serialize)]
pub(crate) struct WarningStats {
    pub(crate) code: String,
    pub(crate) message: String,
}
