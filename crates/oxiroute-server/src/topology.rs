use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    sync::OnceLock,
};

use oxiroute_config::{
    AlpnProtocol, CertificateSource, ConfigDraft, HealthCheck, HealthCheckType, HttpAccessPolicy,
    HttpHostSelector, HttpPathSelector, HttpRoute, HttpRouteAction, HttpUpstreamHost, HttpVersion,
    ListenerBind, Protocol, RtmpRecorderStart, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamTls, UpstreamTlsVerify,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::observability::monitoring::RuntimeHealthSnapshot;

mod attributes;
mod builder;
mod ids;

use builder::TopologyBuilder;
pub(crate) use ids::endpoint_id;
use ids::{edge_kind_name, stable_id};

pub const TOPOLOGY_SCHEMA_VERSION: u32 = 1;

/// The immutable, redacted topology compiled with one active runtime generation.
#[derive(Debug)]
pub struct TopologySnapshot {
    nodes: Box<[TopologyNode]>,
    edges: Box<[TopologyEdge]>,
    listener_nodes: HashMap<String, String>,
    pool_nodes: HashMap<String, String>,
    pool_paths: HashMap<String, String>,
    static_endpoints: HashMap<String, HashSet<String>>,
    rendered: OnceLock<Result<RenderedStructure, String>>,
}

impl TopologySnapshot {
    pub(crate) fn compile(config: &ConfigDraft) -> Self {
        let mut builder = TopologyBuilder::new(config);
        builder.add_listeners(config);
        builder.add_stats_pages(config);
        builder.add_tls(config);
        builder.add_forward_proxy_services(config);
        builder.add_http_services(config);
        builder.add_l4_services(config);
        builder.add_upstream_pools(config);
        builder.finish()
    }

    #[must_use]
    pub fn nodes(&self) -> &[TopologyNode] {
        &self.nodes
    }

    #[must_use]
    pub fn edges(&self) -> &[TopologyEdge] {
        &self.edges
    }

    /// The compiled nodes and edges as JSON array items, rendered by `render` the first time and kept for the life of
    /// the snapshot, which is one generation: a topology only changes with a new one.
    pub(crate) fn rendered_structure(
        &self,
        render: impl FnOnce(&Self) -> Result<RenderedStructure, String>,
    ) -> Result<&RenderedStructure, &str> {
        self.rendered
            .get_or_init(|| render(self))
            .as_ref()
            .map_err(String::as_str)
    }

    pub(crate) fn listener_node(&self, name: &str) -> Option<&str> {
        self.listener_nodes.get(name).map(String::as_str)
    }

    pub(crate) fn pool_node(&self, name: &str) -> Option<(&str, &str)> {
        Some((self.pool_nodes.get(name)?, self.pool_paths.get(name)?))
    }

    pub(crate) fn has_static_endpoint(&self, pool: &str, server: &str) -> bool {
        self.static_endpoints
            .get(pool)
            .is_some_and(|servers| servers.contains(server))
    }

    /// The first compiled listener, pool or server that `runtime` lacks.
    pub(crate) fn missing_from(
        &self,
        runtime: &RuntimeHealthSnapshot,
    ) -> Option<TopologyResponseError> {
        let listener = self.listener_nodes.keys().find(|name| {
            runtime
                .listeners
                .iter()
                .all(|listener| &listener.name != *name)
        });
        let pool = self.pool_nodes.keys().find(|name| {
            runtime
                .upstream_pools
                .iter()
                .all(|pool| &pool.name != *name)
        });
        let endpoint = self.static_endpoints.iter().find_map(|(pool, servers)| {
            let live = runtime
                .upstream_pools
                .iter()
                .find(|live| &live.name == pool);
            servers
                .iter()
                .find(|server| {
                    live.is_none_or(|live| {
                        live.endpoints
                            .iter()
                            .all(|candidate| &candidate.name != *server)
                    })
                })
                .map(|server| (pool, server))
        });
        listener
            .map(|name| TopologyResponseError::MissingListener(name.clone()))
            .or_else(|| pool.map(|name| TopologyResponseError::MissingPool(name.clone())))
            .or_else(|| {
                endpoint.map(|(pool, endpoint)| TopologyResponseError::MissingEndpoint {
                    pool: pool.clone(),
                    endpoint: endpoint.clone(),
                })
            })
    }

    pub(crate) fn listener_count(&self) -> usize {
        self.listener_nodes.len()
    }

    pub(crate) fn pool_count(&self) -> usize {
        self.pool_nodes.len()
    }

    pub(crate) fn static_endpoint_count(&self) -> usize {
        self.static_endpoints.values().map(HashSet::len).sum()
    }
}

/// The nodes and edges of a topology as comma separated JSON objects, without the enclosing brackets.
#[derive(Debug)]
pub(crate) struct RenderedStructure {
    pub(crate) nodes: Box<[u8]>,
    pub(crate) edges: Box<[u8]>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TopologyResponseError {
    #[error("compiled listener `{0}` has no active runtime state")]
    MissingListener(String),
    #[error("active listener `{0}` is absent from the compiled topology")]
    UnknownListener(String),
    #[error("compiled upstream pool `{0}` has no active runtime state")]
    MissingPool(String),
    #[error("active upstream pool `{0}` is absent from the compiled topology")]
    UnknownPool(String),
    #[error("active server `{endpoint}` in pool `{pool}` is absent from the compiled topology")]
    UnknownEndpoint { pool: String, endpoint: String },
    #[error("compiled server `{endpoint}` in pool `{pool}` has no active runtime state")]
    MissingEndpoint { pool: String, endpoint: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyNodeKind {
    Listener,
    ForwardProxyListener,
    ForwardProxyService,
    RtmpListener,
    TlsProfile,
    Certificate,
    HttpService,
    HttpRoute,
    L4Service,
    UpstreamPool,
    Endpoint,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyNode {
    pub id: String,
    pub kind: TopologyNodeKind,
    pub name: String,
    pub config_path: String,
    pub attributes: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyEdgeKind {
    DispatchService,
    ServiceRoute,
    RoutePool,
    ServicePool,
    PoolEndpoint,
    ListenerTls,
    TlsCertificate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyEdge {
    pub id: String,
    pub kind: TopologyEdgeKind,
    pub source: String,
    pub target: String,
    pub config_path: String,
}

impl TopologyEdge {
    pub(crate) fn new(
        kind: TopologyEdgeKind,
        source: &str,
        target: &str,
        config_path: String,
    ) -> Self {
        let kind_name = edge_kind_name(kind);
        Self {
            id: stable_id("edge", &[kind_name, source, target]),
            kind,
            source: source.into(),
            target: target.into(),
            config_path,
        }
    }
}
