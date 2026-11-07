use std::{
    collections::HashMap,
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub use crate::service_plan::errors::ServicePlanError;
use crate::{
    ForwardHttp1ServicePlan, ForwardHttp2ServicePlan, HealthSupervisor, L4ServicePlan,
    PassiveFailurePolicy, PoolError, PreparedTls, RelayPolicy, RoundRobinPool, RouteTable,
    RuntimeEndpoint, TlsProfilePlan, TopologySnapshot, health,
    http_action::{
        AccessLog, HttpActionPlan, HttpGzipPlan, HttpRoutePlan, PoolSelectPlan, ProxyActionPlan,
        ProxyPolicyPlan, RouteAccess, StaticFilesPlan,
    },
    http_cache::{CachePurgeAccess, DiskBackend, HttpCacheBackend, HttpCachePlan},
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
use oxiroute_cache::{Cache, DiskCache, DiskCacheConfig};
use oxiroute_config::{
    CachePurgeAuthorization, HttpPoolSelect, ListenerBind, Protocol, ValidatedConfig,
};
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpCapabilities, RtmpPrepareContext, RtmpPrepareMode,
};

#[derive(Debug)]
pub(crate) enum RuntimeAcquisitionError {
    ServicePlan(ServicePlanError),
    PreparationTimedOut,
}

impl From<ServicePlanError> for RuntimeAcquisitionError {
    fn from(error: ServicePlanError) -> Self {
        Self::ServicePlan(error)
    }
}

mod compile_http;
mod compile_services;
mod disk_registry;
pub(crate) mod errors;
mod runtime_plan;
mod spec;
pub(crate) mod types;

#[allow(clippy::wildcard_imports)]
use compile_http::*;
#[allow(clippy::wildcard_imports)]
use compile_services::*;
pub(crate) use disk_registry::*;
pub use runtime_plan::*;
pub use spec::*;

#[cfg(test)]
mod disk_registry_tests;

#[cfg(test)]
mod rtmp_value_plan_tests;
