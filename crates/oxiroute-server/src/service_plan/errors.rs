use crate::{HealthBuildError, PoolError, RouteError, TlsBuildError};

#[derive(Debug, thiserror::Error)]
pub enum ServicePlanError {
    #[error("runtime configuration is invalid: {0}")]
    InvalidConfig(#[source] Box<oxiroute_config::ConfigError>),
    #[error("TLS configuration cannot be prepared: {0}")]
    Tls(#[source] Box<TlsBuildError>),
    #[error("HTTP/3 upstream pool `{pool}` cannot be prepared: {source}")]
    H3Upstream {
        pool: String,
        source: Box<crate::H3UpstreamBuildError>,
    },
    #[error("upstream pool `{pool}` cannot be compiled: {source}")]
    Pool { pool: String, source: PoolError },
    #[error("upstream pool `{pool}` health check cannot be compiled: {source}")]
    Health {
        pool: String,
        source: Box<HealthBuildError>,
    },
    #[error("health-enabled configurations require `runtime_plan` so probes remain active")]
    HealthSupervisorRequired,
    #[error("HTTP service `{service}` route {route} has invalid method `{method}`")]
    InvalidMethod {
        service: String,
        route: usize,
        method: String,
    },
    #[error("HTTP service `{service}` route {route} cannot be compiled: {source}")]
    Route {
        service: String,
        route: usize,
        source: RouteError,
    },
    #[error("HTTP service `{service}` route {route} access policy failed secure preflight")]
    AccessPreflight { service: String, route: usize },
    #[error("HTTP service `{service}` route {route} static root failed secure preflight")]
    StaticPreflight { service: String, route: usize },
    #[error("HTTP service `{service}` access log failed secure preflight")]
    AccessLogPreflight { service: String },
    #[error("HTTP service `{service}` route {route} references unknown pool `{pool}`")]
    UnknownHttpPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error(
        "HTTP service `{service}` route {route} configures cache, but cache runtime is unavailable"
    )]
    CacheRuntimeUnavailable { service: String, route: usize },
    #[error("listener `{listener}` requires a configured service")]
    MissingListenerService { listener: String },
    #[error("HTTP listener `{listener}` references unknown service `{service}`")]
    UnknownHttpService { listener: String, service: String },
    #[error("TCP listener `{listener}` references unknown service `{service}`")]
    UnknownL4Service { listener: String, service: String },
    #[error("UDP listener `{listener}` references unknown service `{service}`")]
    UnknownUdpService { listener: String, service: String },
    #[error("RTMP listener `{listener}` references unknown service `{service}`")]
    UnknownRtmpService { listener: String, service: String },
    #[error("forward proxy runtime is not integrated for listener `{listener}`")]
    ForwardProxyRuntimeUnavailable { listener: String },
    #[error("forward proxy service `{service}` failed runtime preflight: {source}")]
    ForwardProxyPreflight {
        service: String,
        source: crate::forward_proxy::ForwardPlanError,
    },
    #[error("forward proxy listener `{listener}` references unknown service `{service}`")]
    UnknownForwardProxyService { listener: String, service: String },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` has an invalid runtime policy"
    )]
    InvalidRecorderPolicy {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` failed recording-root preflight"
    )]
    RecorderPreflight {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` could not start"
    )]
    RecorderStartup {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP exec profile `{profile}` in application `{application}` of service `{service}` has an invalid runtime policy"
    )]
    InvalidExecProfile {
        service: String,
        application: String,
        profile: String,
    },
    #[error(
        "RTMP HLS output in application `{application}` of service `{service}` failed media-root preflight"
    )]
    HlsPreflight {
        service: String,
        application: String,
    },
    #[error(
        "RTMP DASH output in application `{application}` of service `{service}` failed media-root preflight"
    )]
    DashPreflight {
        service: String,
        application: String,
    },
    #[error("RTMP auto-push for service `{service}` is unavailable")]
    AutoPushUnavailable { service: String },
    #[error(
        "RTMP push target {target} in application `{application}` of service `{service}` cannot be resolved safely"
    )]
    RtmpPushResolution {
        service: String,
        application: String,
        target: usize,
    },
    #[error(
        "RTMP push target {target} in application `{application}` of service `{service}` resolves to an active RTMP listener"
    )]
    RtmpPushDirectLoop {
        service: String,
        application: String,
        target: usize,
    },
    #[error(
        "RTMP pull target {target} in application `{application}` of service `{service}` cannot be resolved safely"
    )]
    RtmpPullResolution {
        service: String,
        application: String,
        target: usize,
    },
    #[error(
        "RTMP VOD source `{source_name}` in application `{application}` of service `{service}` failed secure preflight"
    )]
    RtmpVodPreflight {
        service: String,
        application: String,
        source_name: String,
    },
    #[error("RTMP callback `{field}` in {scope} of service `{service}` failed secure preflight")]
    RtmpCallbackPreflight {
        service: String,
        scope: String,
        field: &'static str,
    },
    #[error("listener `{listener}` references unknown TLS profile `{profile}`")]
    UnknownListenerTlsProfile { listener: String, profile: String },
    #[error("{protocol:?} listener `{listener}` must not use TLS profile `{profile}`")]
    UnexpectedListenerTlsProfile {
        listener: String,
        protocol: oxiroute_config::Protocol,
        profile: String,
    },
    #[error("L4 service `{service}` references unknown pool `{pool}`")]
    UnknownL4Pool { service: String, pool: String },
    #[error("L4 service `{service}` references TLS-enabled upstream pool `{pool}`")]
    TlsUpstreamPoolForL4Service { service: String, pool: String },
    #[error("runtime does not yet implement canonical policy `{policy}`")]
    RuntimePolicyUnavailable { policy: &'static str },
    #[error("RTMP runtime values cannot be prepared: {0}")]
    RtmpPreparation(#[source] Box<oxiroute_rtmp::RtmpPrepareError>),
    #[error("RTMP runtime resources cannot be prepared: {0}")]
    RtmpRuntimePreparation(#[source] oxiroute_rtmp::RtmpRuntimeSetError),
}

pub(crate) fn rtmp_preparation_error(source: oxiroute_rtmp::RtmpPrepareError) -> ServicePlanError {
    ServicePlanError::RtmpPreparation(Box::new(source))
}
