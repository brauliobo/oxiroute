use std::{collections::HashMap, fmt::Write as _};

use oxiroute_config::{
    AlpnProtocol, CertificateSource, ConfigDraft, HealthCheck, HealthCheckType, HttpAccessPolicy,
    HttpHostSelector, HttpPathSelector, HttpRoute, HttpRouteAction, HttpUpstreamHost, HttpVersion,
    ListenerBind, Protocol, RtmpRecorderStart, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamTls,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{ListenerRuntimeState, ServerOrigin, observability::monitoring::RuntimeHealthSnapshot};

mod attributes;
mod builder;
mod ids;

use attributes::endpoint_state;
use builder::TopologyBuilder;
use ids::{edge_kind_name, endpoint_id, stable_id};

pub const TOPOLOGY_SCHEMA_VERSION: u32 = 1;

/// The immutable, redacted topology compiled with one active runtime generation.
#[derive(Debug)]
pub struct TopologySnapshot {
    nodes: Box<[TopologyNode]>,
    edges: Box<[TopologyEdge]>,
    listener_nodes: HashMap<String, String>,
    pool_nodes: HashMap<String, String>,
    pool_paths: HashMap<String, String>,
    endpoint_nodes: HashMap<(String, String), String>,
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

    pub(crate) fn response_value(
        &self,
        runtime: &RuntimeHealthSnapshot,
    ) -> Result<Value, TopologyResponseError> {
        let mut overlays = Vec::with_capacity(
            runtime.listeners.len()
                + runtime.upstream_pools.len()
                + runtime
                    .upstream_pools
                    .iter()
                    .map(|pool| pool.endpoints.len())
                    .sum::<usize>(),
        );
        self.add_listener_overlays(runtime, &mut overlays)?;
        self.add_pool_overlays(runtime, &mut overlays)?;
        let registered = self.registered_servers(runtime);

        serde_json::to_value(TopologyResponse {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            state: TopologyState {
                config: "active",
                runtime: runtime_state(&runtime.listeners),
                sampled_at_unix_ms: runtime.sampled_at_unix_ms,
            },
            nodes: self
                .nodes
                .iter()
                .chain(registered.iter().map(|(node, _)| node))
                .collect(),
            edges: self
                .edges
                .iter()
                .chain(registered.iter().map(|(_, edge)| edge))
                .collect(),
            overlays,
        })
        .map_err(TopologyResponseError::Serialize)
    }

    /// Servers that joined at runtime are not in the compiled graph; they hang off their pool.
    fn registered_servers(
        &self,
        runtime: &RuntimeHealthSnapshot,
    ) -> Vec<(TopologyNode, TopologyEdge)> {
        runtime
            .upstream_pools
            .iter()
            .filter_map(|pool| {
                Some((
                    pool,
                    self.pool_nodes.get(&pool.name)?,
                    self.pool_paths.get(&pool.name)?,
                ))
            })
            .flat_map(|(pool, pool_node, pool_path)| {
                pool.endpoints
                    .iter()
                    .filter(|endpoint| endpoint.origin == ServerOrigin::Dynamic)
                    .map(move |endpoint| {
                        let id = endpoint_id(&pool.name, &endpoint.name);
                        let config_path = format!("{pool_path}/registration");
                        let node = TopologyNode {
                            id: id.clone(),
                            kind: TopologyNodeKind::Endpoint,
                            name: endpoint.name.clone(),
                            config_path: config_path.clone(),
                            attributes: json!({
                                "type": "socket",
                                "origin": "dynamic",
                                "address": endpoint.address.to_string(),
                                "maxConnections": endpoint.max_connections,
                                "serverName": endpoint.name,
                            }),
                        };
                        let edge = TopologyEdge::new(
                            TopologyEdgeKind::PoolEndpoint,
                            pool_node,
                            &id,
                            config_path,
                        );
                        (node, edge)
                    })
            })
            .collect()
    }

    fn add_listener_overlays(
        &self,
        runtime: &RuntimeHealthSnapshot,
        overlays: &mut Vec<TopologyRuntimeOverlay>,
    ) -> Result<(), TopologyResponseError> {
        for listener in &runtime.listeners {
            let node_id = self
                .listener_nodes
                .get(&listener.name)
                .ok_or_else(|| TopologyResponseError::UnknownListener(listener.name.clone()))?;
            overlays.push(TopologyRuntimeOverlay {
                node_id: node_id.clone(),
                state: listener_state(listener.state),
                metrics: json!({
                    "activeConnections": listener.active_connections,
                    "acceptedConnections": listener.accepted_connections.to_string(),
                    "rejectedConnections": listener.rejected_connections.to_string(),
                    "bytesReceived": listener.bytes_received.to_string(),
                    "bytesSent": listener.bytes_sent.to_string(),
                }),
            });
        }
        for listener in self.listener_nodes.keys() {
            if !runtime
                .listeners
                .iter()
                .any(|snapshot| &snapshot.name == listener)
            {
                return Err(TopologyResponseError::MissingListener(listener.clone()));
            }
        }
        Ok(())
    }

    fn add_pool_overlays(
        &self,
        runtime: &RuntimeHealthSnapshot,
        overlays: &mut Vec<TopologyRuntimeOverlay>,
    ) -> Result<(), TopologyResponseError> {
        for pool in &runtime.upstream_pools {
            let node_id = self
                .pool_nodes
                .get(&pool.name)
                .ok_or_else(|| TopologyResponseError::UnknownPool(pool.name.clone()))?;
            let state = if pool.available_endpoints == 0 {
                "unavailable"
            } else if pool.available_endpoints == pool.total_endpoints {
                "available"
            } else {
                "degraded"
            };
            overlays.push(TopologyRuntimeOverlay {
                node_id: node_id.clone(),
                state,
                metrics: json!({
                    "availableEndpoints": pool.available_endpoints,
                    "totalEndpoints": pool.total_endpoints,
                    "unavailableSelections": pool.unavailable_selections.to_string(),
                    "queued": pool.queued,
                    "queuedTotal": pool.queued_total.to_string(),
                    "queueTimeouts": pool.queue_timeouts.to_string(),
                    "queueCancellations": pool.queue_cancellations.to_string(),
                }),
            });
            for endpoint in &pool.endpoints {
                let node_id = if endpoint.origin == ServerOrigin::Dynamic {
                    endpoint_id(&pool.name, &endpoint.name)
                } else {
                    self.endpoint_nodes
                        .get(&(pool.name.clone(), endpoint.name.clone()))
                        .ok_or_else(|| TopologyResponseError::UnknownEndpoint {
                            pool: pool.name.clone(),
                            endpoint: endpoint.name.clone(),
                        })?
                        .clone()
                };
                overlays.push(TopologyRuntimeOverlay {
                    node_id,
                    state: endpoint_state(endpoint.state),
                    metrics: json!({
                        "activeConnections": endpoint.active_connections.to_string(),
                        "maxConnections": endpoint.max_connections,
                        "lastCheckedAtUnixMs": endpoint.last_checked_at_unix_ms,
                        "lastTransitionAtUnixMs": endpoint.last_transition_at_unix_ms,
                        "successfulChecks": endpoint.successful_checks.to_string(),
                        "failedChecks": endpoint.failed_checks.to_string(),
                        "consecutiveSuccesses": endpoint.consecutive_successes.to_string(),
                        "consecutiveFailures": endpoint.consecutive_failures.to_string(),
                        "lastFailure": endpoint.last_failure,
                    }),
                });
            }
        }
        for pool in self.pool_nodes.keys() {
            if !runtime
                .upstream_pools
                .iter()
                .any(|snapshot| &snapshot.name == pool)
            {
                return Err(TopologyResponseError::MissingPool(pool.clone()));
            }
        }
        for (pool, server) in self.endpoint_nodes.keys() {
            let present = runtime.upstream_pools.iter().any(|snapshot| {
                snapshot.name == *pool
                    && snapshot
                        .endpoints
                        .iter()
                        .any(|candidate| candidate.name == *server)
            });
            if !present {
                return Err(TopologyResponseError::MissingEndpoint {
                    pool: pool.clone(),
                    endpoint: server.clone(),
                });
            }
        }
        Ok(())
    }
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
    #[error("active topology could not be serialized: {0}")]
    Serialize(#[source] serde_json::Error),
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
    fn new(kind: TopologyEdgeKind, source: &str, target: &str, config_path: String) -> Self {
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyResponse<'a> {
    schema_version: u32,
    state: TopologyState,
    nodes: Vec<&'a TopologyNode>,
    edges: Vec<&'a TopologyEdge>,
    overlays: Vec<TopologyRuntimeOverlay>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyState {
    config: &'static str,
    runtime: &'static str,
    sampled_at_unix_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyRuntimeOverlay {
    node_id: String,
    state: &'static str,
    metrics: Value,
}

fn runtime_state(listeners: &[crate::ListenerSnapshot]) -> &'static str {
    if listeners
        .iter()
        .all(|listener| listener.state == ListenerRuntimeState::Listening)
    {
        "active"
    } else if listeners.iter().any(|listener| {
        matches!(
            listener.state,
            ListenerRuntimeState::Stopped | ListenerRuntimeState::Failed
        )
    }) {
        "degraded"
    } else {
        "starting"
    }
}

const fn listener_state(state: ListenerRuntimeState) -> &'static str {
    match state {
        ListenerRuntimeState::Configured => "configured",
        ListenerRuntimeState::Listening => "listening",
        ListenerRuntimeState::Stopped => "stopped",
        ListenerRuntimeState::Failed => "failed",
    }
}
