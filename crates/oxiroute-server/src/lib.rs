pub mod cli;
pub mod config_coordinator;
mod config_watcher;
mod encoding;
mod forward_proxy;
mod generation;
mod health;
mod http_action;
mod http_proxy;
mod http_server_app;
mod l4_service;
mod listener_reservation;
mod monitoring;
mod operational_event;
mod prometheus;
mod routing;
mod rtmp_api;
mod secure_bearer;
mod service_plan;
mod stats;
mod tcp_relay;
mod udp_relay;
pub mod tls;
mod topology;
mod upstream_peer;
mod wire;

pub use config_watcher::{ConfigWatcher, ConfigWatcherOptions, ConfigWatcherStatus};
pub use forward_proxy::{
    ForwardConnectionLifecycle, ForwardHttp1ServicePlan, ForwardHttp2ServiceApp,
    ForwardHttp2ServicePlan, ForwardProxyBody, challenge_response,
};
pub use generation::{
    GenerationAdmission, GenerationCandidate, GenerationError, GenerationManager,
    GenerationMutation, GenerationReference, GenerationRevision, GenerationStatus,
    PreparedGeneration, RuntimeGeneration, RuntimeReferenceKind,
};
pub use health::{HealthBuildError, HealthSupervisor};
pub use http_proxy::{HttpRequestContext, HttpReverseProxy};
pub use http_server_app::{HttpDownstreamPolicyApp, HttpListenerApp, MonitoredHttpApp};
pub use l4_service::L4ServicePlan;
pub use listener_reservation::{ListenerReservation, ListenerReservations};
pub use monitoring::{
    AcmeManagedCertificateSnapshot, CacheSnapshot, CertbotCertificateSnapshot,
    CertbotWatcherHealth, CertbotWatcherSnapshot, ConnectionGuard, DirectFileCertificateSnapshot,
    DirectFileWatcherSnapshot, HostSnapshot, HttpOperationCountSnapshot, HttpOperationResult,
    HttpOperationSnapshot, LatencyBucketSnapshot, LatencySnapshot, ListenerMetrics,
    ListenerRuntimeState, ListenerSnapshot, MetricsError, OPERATION_LATENCY_BUCKETS_MS,
    ProcessConnectionGuard, ProcessRuntime, ProcessSnapshot, RuntimeMetrics, RuntimeSnapshot,
    TcpRelayCountSnapshot, TcpRelayResult, TcpRelaySnapshot, TrafficSnapshot,
};
pub use operational_event::emit_certificate;
pub use prometheus::{PrometheusError, render_prometheus};
pub use routing::{
    AdministrativeState, EndpointHealthSnapshot, EndpointHealthState, EndpointLease, EndpointPool,
    HealthFailure, HealthOverride, PassiveFailurePolicy, PoolAdminError, PoolError,
    PoolHealthSnapshot, RoundRobinPool,
    Route, RouteError, RouteTable, RuntimeEndpoint,
};
pub use rtmp_api::{ApiResponse, RtmpManagementApi, RtmpManagementHttpApp};
pub use service_plan::{
    HttpServicePlan, RtmpServicePlan, RuntimePlan, ServiceKind, ServicePlanError, ServiceSpec,
    runtime_plan, runtime_plan_with_passive_failure_policy, service_specs,
};
pub use stats::{HaproxyStatsApi, HaproxyStatsPage};
pub use tcp_relay::{
    RELAY_BUFFER_SIZE, RelayDirection, RelayFailure, RelayFailureKind, RelayOperation, RelayPolicy,
    RelayStats, TcpRelayCore, relay_streams,
};
pub use udp_relay::UdpRuntime;
pub use tls::{
    AcmeManagedError, AcmeManagedOutcome, AcmeManagedPolicy, AcmeManagedReconciler,
    AcmeManagedStatus, ActiveCertificateGeneration, CertbotActivationDirection, CertbotCandidate,
    CertbotLineage, CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler,
    CertbotReconcilerStatus, CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor,
    CertbotWatcherStatus, CertbotWatcherSupervisor, CertificateGeneration, CertificateMetadata,
    CertificatePublishError, CertificateValidity, FileReconcileError, FileReconcileOutcome,
    FileReconciler, FileReconcilerStatus, FileWatcherConfig, FileWatcherError, FileWatcherMonitor,
    FileWatcherStatus, FileWatcherSupervisor, PreparedTls, TlsBuildError, TlsProfilePlan,
    UpstreamTlsPlan,
};
pub use topology::{
    TOPOLOGY_SCHEMA_VERSION, TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind,
    TopologySnapshot,
};

pub const MAX_HTTP_ATTEMPTS: usize = routing::MAX_RESOLVED_ENDPOINT_ADDRESSES * 4;
