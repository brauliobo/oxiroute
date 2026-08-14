use http::{HeaderName, Method};
use std::path::PathBuf;

use oxiroute_cache::{CacheConfig, CacheTimeline, DiskCacheConfig};
use oxiroute_config::{
    AccessLogPolicy, CachePurgeAuthorization, DownstreamTimeoutPolicy, HttpVersion, ListenerBind,
    Protocol, ProxyProtocolPolicy, UdpPolicy, UpstreamConnectionReuse,
};
use oxiroute_rtmp::RtmpServicePlan;

use crate::{
    PassiveFailurePolicy, Route,
    health::HealthCheckBlueprint,
    http_action::{
        FixedResponsePlan, HttpGzipPlan, ProxyPolicyPlan, RedirectPlan, RoutePolicyPlan,
        StaticFilesBlueprint,
    },
};

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CachePolicyBlueprint {
    pub(crate) store: usize,
    pub(crate) timeline: CacheTimeline,
    pub(crate) methods: Box<[Method]>,
    pub(crate) revalidate: bool,
    pub(crate) surrogate_header: Option<HeaderName>,
    pub(crate) surrogate_limits: Option<(usize, usize)>,
    pub(crate) purge_authorization: Option<CachePurgeAuthorization>,
}

#[derive(Clone)]
pub(crate) enum CacheStoreBlueprint {
    Memory {
        name: String,
        config: CacheConfig,
    },
    Disk {
        name: String,
        root: PathBuf,
        config: DiskCacheConfig,
    },
}

impl CacheStoreBlueprint {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Memory { name, .. } | Self::Disk { name, .. } => name,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ListenerBlueprint {
    pub(crate) name: String,
    pub(crate) bind: ListenerBind,
    pub(crate) protocol: Protocol,
    pub(crate) service: ServiceReference,
    pub(crate) tls_profile: Option<usize>,
    pub(crate) proxy_protocol: Option<ProxyProtocolPolicy>,
    pub(crate) max_connections: Option<u64>,
    pub(crate) downstream_timeouts: DownstreamTimeoutPolicy,
}

#[derive(Clone)]
pub(crate) struct PoolBlueprint {
    pub(crate) name: String,
    pub(crate) endpoints: Box<[EndpointBlueprint]>,
    pub(crate) health: Option<HealthCheckBlueprint>,
    pub(crate) passive_health: PassiveFailurePolicy,
    pub(crate) upstream_tls: Option<crate::tls::UpstreamTlsBlueprint>,
    pub(crate) min_http_version: HttpVersion,
    pub(crate) queue_timeout: Option<std::time::Duration>,
    pub(crate) connect_timeout: Option<std::time::Duration>,
    pub(crate) server_timeout: Option<std::time::Duration>,
    pub(crate) connection_reuse: UpstreamConnectionReuse,
    pub(crate) construction: crate::routing::PoolConstructionBlueprint,
}

#[derive(Clone)]
pub(crate) struct EndpointBlueprint {
    pub(crate) name: String,
    pub(crate) endpoint: crate::RuntimeEndpoint,
    pub(crate) startup_dns: Option<(String, u16)>,
    pub(crate) max_connections: Option<u64>,
}

pub(crate) struct HttpServiceBlueprint {
    pub(crate) name: String,
    pub(crate) routes: Box<[HttpRouteBlueprint]>,
    pub(crate) automatic_response_headers: bool,
    pub(crate) upstream_io_timeout: std::time::Duration,
    pub(crate) max_request_body_bytes: Option<u64>,
    pub(crate) gzip: Option<HttpGzipPlan>,
    pub(crate) access_log: Option<AccessLogPolicy>,
    pub(crate) route_table: crate::RouteTable,
}

pub(crate) struct HttpRouteBlueprint {
    pub(crate) route: Route,
    pub(crate) access: Option<oxiroute_config::HttpAccessPolicy>,
    pub(crate) policy: RoutePolicyPlan,
    pub(crate) action: HttpActionBlueprint,
}

pub(crate) enum HttpActionBlueprint {
    Proxy {
        pool: usize,
        policy: ProxyPolicyPlan,
        cache: Option<CachePolicyBlueprint>,
    },
    Fixed(FixedResponsePlan),
    Redirect(RedirectPlan),
    Static(StaticFilesBlueprint),
}

#[derive(Clone)]
pub(crate) struct L4ServiceBlueprint {
    pub(crate) pool: usize,
    pub(crate) connect_timeout: std::time::Duration,
    pub(crate) idle_timeout: std::time::Duration,
    pub(crate) lifetime_timeout: Option<std::time::Duration>,
    pub(crate) proxy_protocol: Option<ProxyProtocolPolicy>,
    pub(crate) udp: UdpPolicy,
}

#[derive(Clone, Copy)]
pub(crate) enum ServiceReference {
    Http(usize),
    Forward(usize),
    Rtmp(usize),
    L4(usize),
}

pub(crate) struct RtmpSpec {
    pub(crate) plan: RtmpServicePlan,
    pub(crate) access_log: Option<AccessLogPolicy>,
    pub(crate) callbacks: RtmpCallbackBlueprint,
    pub(crate) applications: Box<[RtmpApplicationBlueprint]>,
}

pub(crate) struct RtmpApplicationBlueprint {
    pub(crate) publish_policy: oxiroute_rtmp::RtmpAccessPolicy,
    pub(crate) play_policy: oxiroute_rtmp::RtmpAccessPolicy,
    pub(crate) fanout_limits: oxiroute_rtmp::LiveHubLimits,
    pub(crate) callbacks: RtmpCallbackBlueprint,
    pub(crate) vod: Option<oxiroute_rtmp::VodApplicationBlueprint>,
}

pub(crate) struct RtmpCallbackBlueprint {
    pub(crate) endpoints: [Option<RtmpCallbackEndpointBlueprint>; 8],
    pub(crate) method: oxiroute_rtmp::RtmpCallbackMethod,
    pub(crate) timeout: std::time::Duration,
    pub(crate) update_timeout: std::time::Duration,
    pub(crate) update_strict: bool,
    pub(crate) relay_redirect: bool,
}

pub(crate) struct RtmpCallbackEndpointBlueprint {
    pub(crate) endpoint: oxiroute_rtmp::RtmpCallbackEndpointBlueprint,
    pub(crate) service: String,
    pub(crate) scope: String,
    pub(crate) field: &'static str,
}
