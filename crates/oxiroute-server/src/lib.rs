mod capsule;
pub mod cli;
pub mod config_coordinator;
mod config_watcher;
mod encoding;
mod forward_proxy;
mod generation;
mod generation_blueprint;
mod generation_compiler;
mod generation_health;
mod generation_resources;
mod health;
mod html;
mod http3;
mod http3_upstream;
mod http_action;
mod http_cache;
mod http_policy;
mod http_proxy;
mod http_server_app;
mod l4_service;
mod listener_inventory;
mod listener_reservation;
mod listener_runtime;
mod logging;
mod monitoring;
mod operational_event;
mod planning_errors;
mod planning_types;
mod prometheus;
mod proxy_protocol;
mod routing;
mod rtmp_api;
mod rtmp_generation_runtime;
mod rtmp_value_mapping;
mod rtmp_value_plan;
mod runtime_policy;
mod secure_bearer;
mod service_plan;
mod shutdown;
mod stats;
mod tcp_relay;
pub mod tls;
mod topology;
mod udp_relay;
mod upstream_peer;
mod wire;

pub use config_watcher::{ConfigWatcher, ConfigWatcherOptions, ConfigWatcherStatus};
pub use forward_proxy::{
    ForwardAccessMetricsSnapshot, ForwardAccessResult, ForwardConnectionLifecycle,
    ForwardHttp1ServicePlan, ForwardHttp2ServiceApp, ForwardHttp2ServicePlan, ForwardProxyBody,
    challenge_response,
};
pub use generation::{
    GenerationAdmission, GenerationCandidate, GenerationError, GenerationManager,
    GenerationMutation, GenerationReference, GenerationRevision, GenerationStatus,
    PreparedGeneration, RuntimeGeneration, RuntimeReferenceKind,
};
pub use health::{HealthBuildError, HealthSupervisor};
pub use http_proxy::{HttpRequestContext, HttpReverseProxy};
pub use http_server_app::{HttpDownstreamPolicyApp, HttpListenerApp, MonitoredHttpApp};
pub use http3::Http3Runtime;
pub(crate) use http3_upstream::{H3UpstreamBuildError, H3UpstreamError, H3UpstreamPlan};
pub use l4_service::L4ServicePlan;
pub use listener_reservation::{ListenerReservation, ListenerReservations};
pub use monitoring::{
    ACCESS_RECORD_CAPACITY, AccessRecord, AcmeManagedCertificateSnapshot, CacheSnapshot,
    CertbotCertificateSnapshot, CertbotWatcherHealth, CertbotWatcherSnapshot, ComponentState,
    ComponentStatus, ConnectionGuard, DirectFileCertificateSnapshot, DirectFileWatcherSnapshot,
    HostSnapshot, HttpOperationCountSnapshot, HttpOperationResult, HttpOperationSnapshot,
    LatencyBucketSnapshot, LatencySnapshot, ListenerMetrics, ListenerRuntimeState,
    ListenerSnapshot, MetricsError, OPERATION_LATENCY_BUCKETS_MS, ObservedTransport,
    ProcessConnectionGuard, ProcessRuntime, ProcessSnapshot, ProxyProtocolCountSnapshot,
    ProxyProtocolSnapshot, RuntimeMetrics, RuntimeMode, RuntimeSnapshot, TcpRelayCountSnapshot,
    TcpRelayResult, TcpRelaySnapshot, TrafficSnapshot, TransportOperationCountSnapshot,
    TransportOperationSnapshot, TransportOutcome,
};
pub use operational_event::{WorkerEventPage, WorkerEventSnapshot, worker_event_page};
pub use operational_event::{emit_certificate, emit_rtmp_access};
pub use prometheus::{PrometheusError, render_prometheus};
pub use proxy_protocol::{
    AcceptedProxyStream, MAX_V1_HEADER_BYTES, MAX_V2_HEADER_BYTES, MAX_V2_PAYLOAD_BYTES,
    ParsedProxyHeader, PrefixedStream, ProxyProtocolError, ProxyProtocolErrorKind,
    ProxyProtocolResult, ProxyProtocolTransport, accept_stream, encode_header, parse_header,
};
pub use routing::{
    AdministrativeState, EndpointHealthSnapshot, EndpointHealthState, EndpointLease, EndpointPool,
    HealthFailure, HealthOverride, PassiveFailurePolicy, PoolAdminError, PoolError,
    PoolHealthSnapshot, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint,
};
pub use rtmp_api::{ApiResponse, RtmpManagementApi, RtmpManagementHttpApp};
pub use service_plan::{
    HttpServicePlan, RtmpServicePlan, RuntimePlan, ServiceKind, ServicePlanError, ServiceSpec,
    runtime_plan, runtime_plan_with_passive_failure_policy, service_specs, validate_runtime_plan,
};
pub use stats::{HaproxyStatsApi, HaproxyStatsPage};
pub use tcp_relay::{
    RELAY_BUFFER_SIZE, RelayDirection, RelayFailure, RelayFailureKind, RelayOperation, RelayPolicy,
    RelayStats, TcpRelayCore, relay_streams, select_upstream_with_shutdown,
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
pub use udp_relay::{UdpRelayStats, UdpRuntime};

pub const MAX_HTTP_ATTEMPTS: usize = routing::MAX_RESOLVED_ENDPOINT_ADDRESSES * 4;
