pub mod config_coordinator;
mod health;
mod http_action;
mod http_proxy;
mod l4_service;
mod monitoring;
mod routing;
mod rtmp_api;
mod service_plan;
mod tcp_relay;
pub mod tls;
mod topology;
mod upstream_peer;
mod wire;

pub use health::{HealthBuildError, HealthSupervisor};
pub use http_proxy::{HttpListenerApp, HttpRequestContext, HttpReverseProxy, MonitoredHttpApp};
pub use l4_service::L4ServicePlan;
pub use monitoring::{
    CertbotCertificateSnapshot, CertbotWatcherHealth, CertbotWatcherSnapshot, ConnectionGuard,
    HostSnapshot, ListenerMetrics, ListenerRuntimeState, ListenerSnapshot, MetricsError,
    ProcessSnapshot, RuntimeMetrics, RuntimeSnapshot, TrafficSnapshot,
};
pub use routing::{
    EndpointHealthSnapshot, EndpointHealthState, EndpointLease, EndpointPool, HealthFailure,
    PoolError, PoolHealthSnapshot, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint,
};
pub use rtmp_api::{ApiResponse, RtmpManagementApi};
pub use service_plan::{
    HttpServicePlan, RtmpServicePlan, RuntimePlan, ServiceKind, ServicePlanError, ServiceSpec,
    runtime_plan, service_specs,
};
pub use tcp_relay::{
    RELAY_BUFFER_SIZE, RelayDirection, RelayFailure, RelayFailureKind, RelayOperation, RelayPolicy,
    RelayStats, TcpRelayCore, relay_streams,
};
pub use tls::{
    ActiveCertificateGeneration, CertbotActivationDirection, CertbotCandidate, CertbotLineage,
    CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler, CertbotReconcilerStatus,
    CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherStatus,
    CertbotWatcherSupervisor, CertificateGeneration, CertificateMetadata, CertificatePublishError,
    CertificateValidity, PreparedTls, TlsBuildError, TlsProfilePlan, UpstreamTlsPlan,
};
pub use topology::{
    TOPOLOGY_SCHEMA_VERSION, TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind,
    TopologySnapshot,
};

pub const MAX_HTTP_ATTEMPTS: usize = routing::MAX_RESOLVED_ENDPOINT_ADDRESSES * 3;
