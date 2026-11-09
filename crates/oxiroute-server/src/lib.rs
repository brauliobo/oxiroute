mod capsule;
pub mod cli;
pub mod config;
mod encoding;
mod forward_proxy;
mod generation;
mod health;
mod html;
mod http;
mod http3;
mod l4;
mod lifecycle_control;
mod listener;
mod observability;
mod registry;
mod routing;
mod rtmp;
mod runtime_policy;
mod secure_bearer;
mod service_plan;
mod sharded;
mod shutdown;
pub mod tls;
mod topology;
mod upstream_peer;
mod wire;

#[cfg(test)]
#[path = "../../../benchmarks/r3-routing-control/internal/mod.rs"]
mod r3_bench;

pub use config::coordinator as config_coordinator;
pub use config::events::ConfigEventFilter;
pub use config::watcher::{ConfigWatcher, ConfigWatcherOptions, ConfigWatcherStatus};
pub use forward_proxy::{
    ForwardAccessMetricsSnapshot, ForwardAccessResult, ForwardConnectionLifecycle,
    ForwardHttp1ServicePlan, ForwardHttp2ServiceApp, ForwardHttp2ServicePlan, ForwardProxyBody,
    challenge_response,
};
pub use generation::retirement::{
    PendingApply, RemoteRetired, RetiredGenerationStatus, RetiredState, RetirementCounters,
    WorkerNotice,
};
pub use generation::{
    GenerationAdmission, GenerationCandidate, GenerationError, GenerationManager,
    GenerationMutation, GenerationReference, GenerationRevision, GenerationStatus,
    PreparedGeneration, RuntimeGeneration, RuntimeReferenceKind,
};
pub use health::{HealthBuildError, HealthSupervisor};
pub use http::evacuation::{EvacuationReport, LeftRunning};
pub use http::inflight::live_requests;
pub use http::proxy::{HttpRequestContext, HttpReverseProxy};
pub use http::server_app::{HttpDownstreamPolicyApp, HttpListenerApp, MonitoredHttpApp};
pub use http3::Http3Runtime;
pub(crate) use http3::upstream_plan::{H3UpstreamBuildError, H3UpstreamError, H3UpstreamPlan};
pub use l4::proxy_protocol::{
    AcceptedProxyStream, MAX_V1_HEADER_BYTES, MAX_V2_HEADER_BYTES, MAX_V2_PAYLOAD_BYTES,
    ParsedProxyHeader, PrefixedStream, ProxyProtocolError, ProxyProtocolErrorKind,
    ProxyProtocolResult, ProxyProtocolTransport, accept_stream, encode_header, parse_header,
};
pub use l4::service::L4ServicePlan;
pub use l4::tcp::{
    RELAY_BUFFER_SIZE, RelayDirection, RelayFailure, RelayFailureKind, RelayOperation, RelayPolicy,
    RelayStats, TcpRelayCore, relay_streams, select_upstream_with_shutdown,
};
pub use l4::udp::{UdpRelayStats, UdpRuntime};
pub use listener::reservation::{ListenerReservation, ListenerReservations};
pub use listener::runtime::{AdmissionError, ListenerRuntime, TrafficLease};
pub use observability::monitoring::{
    ACCESS_RECORD_CAPACITY, AccessRecord, AcmeManagedCertificateSnapshot, CacheSnapshot,
    CertbotCertificateSnapshot, CertbotWatcherHealth, CertbotWatcherSnapshot, ComponentState,
    ComponentStatus, ConnectionGuard, DirectFileCertificateSnapshot, DirectFileWatcherSnapshot,
    HostSnapshot, HttpOperationCountSnapshot, HttpOperationResult, HttpOperationSnapshot,
    LatencyBucketSnapshot, LatencySnapshot, ListenerMetrics, ListenerRuntimeState,
    ListenerSnapshot, MetricsError, OPERATION_LATENCY_BUCKETS_MS, ObservedTransport,
    PoolSelectionSnapshot, ProcessConnectionGuard, ProcessRuntime, ProcessSnapshot,
    ProxyProtocolCountSnapshot, ProxyProtocolSnapshot, RescueSnapshot, RuntimeMetrics, RuntimeMode,
    RuntimeSnapshot, TcpRelayCountSnapshot, TcpRelayResult, TcpRelaySnapshot, TrafficSnapshot,
    TransportOperationCountSnapshot, TransportOperationSnapshot, TransportOutcome, WorkerReport,
};
pub use observability::operational_event::{
    WorkerEventPage, WorkerEventSnapshot, flush_audit, worker_event_page,
};
pub use observability::operational_event::{emit_certificate, emit_rtmp_access};
pub use observability::prometheus::PrometheusError;
pub use observability::stats::{HaproxyStatsApi, HaproxyStatsPage};
pub use routing::{
    AdministrativeState, EndpointHealthSnapshot, EndpointHealthState, EndpointLease, EndpointPool,
    HealthFailure, HealthOverride, PassiveFailurePolicy, PoolAdminError, PoolError,
    PoolHealthSnapshot, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint,
    ServerAdminState, ServerOrigin,
};
pub use rtmp::api::{ApiResponse, RtmpManagementApi, RtmpManagementHttpApp};
pub use service_plan::{
    HttpServicePlan, RtmpServicePlan, RuntimePlan, ServiceKind, ServicePlanError, ServiceSpec,
    runtime_plan, runtime_plan_with_passive_failure_policy, service_specs, validate_runtime_plan,
};
pub use tls::{
    AcmeDnsCleanupRecovery, AcmeManagedError, AcmeManagedOutcome, AcmeManagedPolicy,
    AcmeManagedReconciler, AcmeManagedStatus, ActiveCertificateGeneration,
    CertbotActivationDirection, CertbotCandidate, CertbotLineage, CertbotReconcileError,
    CertbotReconcileOutcome, CertbotReconciler, CertbotReconcilerStatus, CertbotWatcherConfig,
    CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherStatus, CertbotWatcherSupervisor,
    CertificateGeneration, CertificateMetadata, CertificatePublishError, CertificateValidity,
    FileReconcileError, FileReconcileOutcome, FileReconciler, FileReconcilerStatus,
    FileWatcherConfig, FileWatcherError, FileWatcherMonitor, FileWatcherStatus,
    FileWatcherSupervisor, PreparedTls, TlsAlpnChallenge, TlsAlpnChallengeError,
    TlsAlpnChallengeIdentity, TlsAlpnChallengeLease, TlsAlpnChallengeStore, TlsBuildError,
    TlsProfilePlan, UpstreamTlsPlan,
};
pub use topology::{
    TOPOLOGY_SCHEMA_VERSION, TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind,
    TopologySnapshot,
};

pub const MAX_HTTP_ATTEMPTS: usize = routing::MAX_RESOLVED_ENDPOINT_ADDRESSES * 4;
