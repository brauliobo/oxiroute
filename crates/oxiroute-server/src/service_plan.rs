use std::{
    collections::HashMap,
    net::ToSocketAddrs,
    sync::Arc,
    time::{Duration, Instant},
};

pub use crate::service_plan::errors::ServicePlanError;
use crate::{
    ForwardHttp1ServicePlan, ForwardHttp2ServicePlan, HealthSupervisor, L4ServicePlan,
    PassiveFailurePolicy, PoolError, PreparedTls, RelayPolicy, RoundRobinPool, RouteTable,
    RuntimeEndpoint, TlsProfilePlan, TopologySnapshot, health,
    http::action::{
        AccessLog, HttpActionPlan, HttpGzipPlan, HttpRoutePlan, PoolSelectPlan, ProxyActionPlan,
        ProxyPolicyPlan, RouteAccess, StaticFilesPlan,
    },
    http::cache::{
        CacheCompression, CachePurgeAccess, DiskBackend, HttpCacheBackend, HttpCachePlan,
    },
    routing::RuntimeServer,
    upstream_peer::UpstreamPlan,
};
use crate::{
    generation::compiler::GenerationCompiler,
    service_plan::types::{
        CachePolicyBlueprint, CacheStoreBlueprint, HttpActionBlueprint, HttpRouteBlueprint,
        HttpServiceBlueprint, L4ServiceBlueprint, ListenerBlueprint, PoolBlueprint, RtmpSpec,
        ServiceReference,
    },
};
use http::{Method, Uri, uri::Authority};
use oxiroute_cache::DiskCache;
use oxiroute_config::{
    CachePurgeAuthorization, HttpPoolSelect, ListenerBind, Protocol, ValidatedConfig,
};
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpCapabilities, RtmpPrepareContext, RtmpPrepareMode,
};

#[path = "service_plan/compile/http.rs"]
mod compile_http;
#[path = "service_plan/compile/services.rs"]
mod compile_services;
mod data_plane;
pub(crate) mod errors;
mod exclusive;
mod runtime_plan;
mod spec;
pub(crate) mod types;
mod warm;

#[allow(clippy::wildcard_imports)]
use compile_http::*;
#[allow(clippy::wildcard_imports)]
use compile_services::*;
pub use data_plane::{DataPlane, PlaneSelection, Skip, Skipped};
pub(crate) use data_plane::{DataPlanePolicy, ListenerPlane, PlaneCapabilities};
pub use exclusive::{ExclusiveStores, StoreOwnershipError};
pub use runtime_plan::*;
pub use spec::*;
pub use warm::WarmCaches;

#[cfg(test)]
mod rtmp_value_plan_tests;
