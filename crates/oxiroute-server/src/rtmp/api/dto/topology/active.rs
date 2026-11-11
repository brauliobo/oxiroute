//! The body of `GET /api/v1/topology`: the compiled nodes and edges are rendered once per topology snapshot (one per
//! generation) and the live part, the overlays and the servers that registered at runtime, is written around them.
use std::{collections::HashSet, io::Write as _};

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{
    EndpointHealthSnapshot, EndpointHealthState, HealthFailure, ListenerRuntimeState,
    PoolHealthSnapshot, ServerOrigin, TopologyEdge,
    topology::{RenderedStructure, TopologyResponseError, endpoint_id},
};

impl TopologyResponse {
    /// The active topology as the JSON of a [`TopologyResponse`].
    pub(crate) fn active_body(
        topology: &TopologySnapshot,
        runtime: &RuntimeHealthSnapshot,
    ) -> Result<Vec<u8>, String> {
        let structure = topology
            .rendered_structure(render_structure)
            .map_err(str::to_owned)?;
        let overlays = active_overlays(topology, runtime).map_err(|error| error.to_string())?;
        let (nodes, edges) = registered_servers(topology, runtime);
        let state = TopologyStateDto {
            config: ConfigStateDto::Active,
            runtime: RuntimeStateDto::of(&runtime.listeners),
            sampled_at_unix_ms: runtime.sampled_at_unix_ms,
        };
        let mut body = Vec::with_capacity(
            structure.nodes.len() + structure.edges.len() + 256 * overlays.len(),
        );
        let write = |body: &mut Vec<u8>| -> Result<(), serde_json::Error> {
            write!(
                body,
                "{{\"schemaVersion\":{TOPOLOGY_SCHEMA_VERSION},\"state\":"
            )
            .expect("a Vec grows");
            serde_json::to_writer(&mut *body, &state)?;
            body.extend_from_slice(b",\"nodes\":[");
            append_items(body, &structure.nodes, &nodes)?;
            body.extend_from_slice(b"],\"edges\":[");
            append_items(body, &structure.edges, &edges)?;
            body.extend_from_slice(b"],\"overlays\":");
            serde_json::to_writer(&mut *body, &overlays)?;
            body.push(b'}');
            Ok(())
        };
        write(&mut body)
            .map_err(|error| format!("could not serialize active topology: {error}"))?;
        Ok(body)
    }
}

fn render_structure(topology: &TopologySnapshot) -> Result<RenderedStructure, String> {
    let nodes = topology
        .nodes()
        .iter()
        .map(TopologyNodeDto::project)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RenderedStructure {
        nodes: render_items(&nodes)?,
        edges: render_items(
            &topology
                .edges()
                .iter()
                .map(TopologyEdgeDto::from)
                .collect::<Vec<_>>(),
        )?,
    })
}

fn render_items(items: &[impl Serialize]) -> Result<Box<[u8]>, String> {
    let mut out = Vec::new();
    for item in items {
        if !out.is_empty() {
            out.push(b',');
        }
        serde_json::to_writer(&mut out, item).map_err(|error| error.to_string())?;
    }
    Ok(out.into())
}

/// Writes the rendered `fixed` items and then `extra`, comma separated.
fn append_items(
    body: &mut Vec<u8>,
    fixed: &[u8],
    extra: &[impl Serialize],
) -> Result<(), serde_json::Error> {
    body.extend_from_slice(fixed);
    for item in extra {
        if !body.ends_with(b"[") {
            body.push(b',');
        }
        serde_json::to_writer(&mut *body, item)?;
    }
    Ok(())
}

/// Servers that joined at runtime are not in the compiled graph; they hang off their pool.
fn registered_servers(
    topology: &TopologySnapshot,
    runtime: &RuntimeHealthSnapshot,
) -> (Vec<TopologyNodeDto>, Vec<TopologyEdgeDto>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for pool in &runtime.upstream_pools {
        let Some((pool_node, pool_path)) = topology.pool_node(&pool.name) else {
            continue;
        };
        for endpoint in pool
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.origin == ServerOrigin::Dynamic)
        {
            let id = endpoint_id(&pool.name, &endpoint.name);
            let config_path = format!("{pool_path}/registration");
            nodes.push(TopologyNodeDto::Endpoint {
                common: TopologyNodeCommonDto {
                    id: id.clone(),
                    name: endpoint.name.clone(),
                    config_path: config_path.clone(),
                },
                attributes: EndpointAttributesDto::Socket {
                    address: endpoint.address.to_string(),
                    max_connections: endpoint.max_connections,
                    server_name: endpoint.name.clone(),
                    origin: Some(ServerOrigin::Dynamic),
                },
            });
            edges.push(TopologyEdgeDto::from(&TopologyEdge::new(
                TopologyEdgeKind::PoolEndpoint,
                pool_node,
                &id,
                config_path,
            )));
        }
    }
    (nodes, edges)
}

fn active_overlays(
    topology: &TopologySnapshot,
    runtime: &RuntimeHealthSnapshot,
) -> Result<Vec<TopologyOverlayDto>, TopologyResponseError> {
    let mut overlays = Vec::with_capacity(
        runtime.listeners.len()
            + runtime.upstream_pools.len()
            + runtime
                .upstream_pools
                .iter()
                .map(|pool| pool.endpoints.len())
                .sum::<usize>(),
    );
    let mut listeners = HashSet::with_capacity(runtime.listeners.len());
    for listener in &runtime.listeners {
        let node_id = topology
            .listener_node(&listener.name)
            .ok_or_else(|| TopologyResponseError::UnknownListener(listener.name.clone()))?;
        listeners.insert(listener.name.as_str());
        overlays.push(TopologyOverlayDto::Listener(TopologyOverlay {
            node_id: node_id.to_owned(),
            state: listener.state.into(),
            metrics: ListenerOverlayMetricsDto {
                active_connections: listener.active_connections,
                accepted_connections: listener.accepted_connections.into(),
                rejected_connections: listener.rejected_connections.into(),
                bytes_received: listener.bytes_received.into(),
                bytes_sent: listener.bytes_sent.into(),
            },
        }));
    }
    let mut static_endpoints = 0;
    for pool in &runtime.upstream_pools {
        let (node_id, _) = topology
            .pool_node(&pool.name)
            .ok_or_else(|| TopologyResponseError::UnknownPool(pool.name.clone()))?;
        overlays.push(pool_overlay(node_id, pool));
        for endpoint in &pool.endpoints {
            if endpoint.origin != ServerOrigin::Dynamic {
                if !topology.has_static_endpoint(&pool.name, &endpoint.name) {
                    return Err(TopologyResponseError::UnknownEndpoint {
                        pool: pool.name.clone(),
                        endpoint: endpoint.name.clone(),
                    });
                }
                static_endpoints += 1;
            }
            overlays.push(endpoint_overlay(&pool.name, endpoint));
        }
    }
    // Names are unique in a runtime snapshot, so equal counts mean every compiled item is present.
    let complete = listeners.len() == topology.listener_count()
        && runtime.upstream_pools.len() == topology.pool_count()
        && static_endpoints == topology.static_endpoint_count();
    if complete {
        return Ok(overlays);
    }
    topology.missing_from(runtime).map_or(Ok(overlays), Err)
}

fn pool_overlay(node_id: &str, pool: &PoolHealthSnapshot) -> TopologyOverlayDto {
    let state = if pool.available_endpoints == 0 {
        PoolOverlayStateDto::Unavailable
    } else if pool.available_endpoints == pool.total_endpoints {
        PoolOverlayStateDto::Available
    } else {
        PoolOverlayStateDto::Degraded
    };
    TopologyOverlayDto::Pool(TopologyOverlay {
        node_id: node_id.to_owned(),
        state,
        metrics: PoolOverlayMetricsDto {
            available_endpoints: pool.available_endpoints,
            total_endpoints: pool.total_endpoints,
            unavailable_selections: pool.unavailable_selections.into(),
            queued: pool.queued,
            queued_total: pool.queued_total.into(),
            queue_timeouts: pool.queue_timeouts.into(),
            queue_cancellations: pool.queue_cancellations.into(),
        },
    })
}

fn endpoint_overlay(pool: &str, endpoint: &EndpointHealthSnapshot) -> TopologyOverlayDto {
    TopologyOverlayDto::Endpoint(TopologyOverlay {
        node_id: endpoint_id(pool, &endpoint.name),
        state: endpoint.state.into(),
        metrics: EndpointOverlayMetricsDto {
            active_connections: endpoint.active_connections.into(),
            max_connections: endpoint.max_connections,
            last_checked_at_unix_ms: endpoint.last_checked_at_unix_ms,
            last_transition_at_unix_ms: endpoint.last_transition_at_unix_ms,
            successful_checks: endpoint.successful_checks.into(),
            failed_checks: endpoint.failed_checks.into(),
            consecutive_successes: endpoint.consecutive_successes.into(),
            consecutive_failures: endpoint.consecutive_failures.into(),
            last_failure: endpoint.last_failure.map(Into::into),
        },
    })
}

impl RuntimeStateDto {
    fn of(listeners: &[crate::ListenerSnapshot]) -> Self {
        if listeners
            .iter()
            .all(|listener| listener.state == ListenerRuntimeState::Listening)
        {
            Self::Active
        } else if listeners.iter().any(|listener| {
            matches!(
                listener.state,
                ListenerRuntimeState::Stopped | ListenerRuntimeState::Failed
            )
        }) {
            Self::Degraded
        } else {
            Self::Starting
        }
    }
}

impl From<ListenerRuntimeState> for ListenerOverlayStateDto {
    fn from(state: ListenerRuntimeState) -> Self {
        match state {
            ListenerRuntimeState::Configured => Self::Configured,
            ListenerRuntimeState::Listening => Self::Listening,
            ListenerRuntimeState::Stopped => Self::Stopped,
            ListenerRuntimeState::Failed => Self::Failed,
        }
    }
}

impl From<EndpointHealthState> for EndpointOverlayStateDto {
    fn from(state: EndpointHealthState) -> Self {
        match state {
            EndpointHealthState::Unchecked => Self::Unchecked,
            EndpointHealthState::Unknown => Self::Unknown,
            EndpointHealthState::Healthy => Self::Healthy,
            EndpointHealthState::Unhealthy => Self::Unhealthy,
        }
    }
}

impl From<HealthFailure> for HealthFailureDto {
    fn from(failure: HealthFailure) -> Self {
        match failure {
            HealthFailure::Timeout => Self::Timeout,
            HealthFailure::ConnectFailed => Self::ConnectFailed,
            HealthFailure::UnexpectedStatus => Self::UnexpectedStatus,
            HealthFailure::ProtocolError => Self::ProtocolError,
            HealthFailure::Slow => Self::Slow,
        }
    }
}
