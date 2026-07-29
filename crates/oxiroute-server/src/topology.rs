use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use oxiroute_config::{
    AlpnProtocol, CertificateSource, Config, HealthCheck, HealthCheckType, HttpAccessPolicy,
    HttpHostSelector, HttpPathSelector, HttpRoute, HttpRouteAction, HttpUpstreamHost, HttpVersion,
    ListenerBind, Protocol, RtmpRecorderStart, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamTls,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{ListenerRuntimeState, RoundRobinPool, ServiceSpec, monitoring::RuntimeHealthSnapshot};

pub const TOPOLOGY_SCHEMA_VERSION: u32 = 1;

/// The immutable, redacted topology compiled with one active runtime generation.
#[derive(Debug)]
pub struct TopologySnapshot {
    nodes: Box<[TopologyNode]>,
    edges: Box<[TopologyEdge]>,
    listener_nodes: HashMap<String, String>,
    pool_nodes: HashMap<String, String>,
    endpoint_nodes: HashMap<(String, String), String>,
}

impl TopologySnapshot {
    pub(crate) fn compile(
        config: &Config,
        services: &[ServiceSpec],
        pools: &[Arc<RoundRobinPool>],
    ) -> Self {
        debug_assert_eq!(config.listeners.len(), services.len());
        debug_assert_eq!(config.upstream_pools.len(), pools.len());

        let mut builder = TopologyBuilder::new(config);
        builder.add_listeners(config, services);
        builder.add_tls(config);
        builder.add_http_services(config);
        builder.add_l4_services(config);
        builder.add_upstream_pools(config, pools);
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

        serde_json::to_value(TopologyResponse {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            state: TopologyState {
                config: "active",
                runtime: runtime_state(&runtime.listeners),
                sampled_at_unix_ms: runtime.sampled_at_unix_ms,
            },
            nodes: &self.nodes,
            edges: &self.edges,
            overlays,
        })
        .map_err(TopologyResponseError::Serialize)
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
                let node_id = self
                    .endpoint_nodes
                    .get(&(pool.name.clone(), endpoint.name.clone()))
                    .ok_or_else(|| TopologyResponseError::UnknownEndpoint {
                        pool: pool.name.clone(),
                        endpoint: endpoint.name.clone(),
                    })?;
                overlays.push(TopologyRuntimeOverlay {
                    node_id: node_id.clone(),
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

struct TopologyBuilder {
    nodes: Vec<TopologyNode>,
    edges: Vec<TopologyEdge>,
    listener_nodes: HashMap<String, String>,
    pool_nodes: HashMap<String, String>,
    endpoint_nodes: HashMap<(String, String), String>,
}

impl TopologyBuilder {
    fn new(config: &Config) -> Self {
        let endpoint_capacity = config
            .upstream_pools
            .iter()
            .map(|pool| pool.endpoints.len())
            .sum();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            listener_nodes: HashMap::with_capacity(config.listeners.len()),
            pool_nodes: HashMap::with_capacity(config.upstream_pools.len()),
            endpoint_nodes: HashMap::with_capacity(endpoint_capacity),
        }
    }

    fn add_listeners(&mut self, config: &Config, services: &[ServiceSpec]) {
        for (index, (listener, service)) in config.listeners.iter().zip(services).enumerate() {
            let kind = match listener.protocol {
                Protocol::Rtmp => TopologyNodeKind::RtmpListener,
                Protocol::Http | Protocol::Tcp => TopologyNodeKind::Listener,
                Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => {
                    TopologyNodeKind::ForwardProxyListener
                }
            };
            let id = listener_id(&listener.name, kind);
            let config_path = format!("/listeners/{index}");
            let mut attributes = json!({
                "bind": listener_bind(&listener.bind),
                "protocol": protocol(listener.protocol),
                "service": listener.service,
                "tlsProfile": listener.tls_profile,
                "maxConnections": listener.max_connections,
                "downstreamTimeouts": {
                    "clientTimeoutMs": listener.downstream_timeouts.client_timeout_ms,
                    "requestTimeoutMs": listener.downstream_timeouts.request_timeout_ms,
                    "keepaliveTimeoutMs": listener.downstream_timeouts.keepalive_timeout_ms,
                },
            });
            if listener.protocol == Protocol::Rtmp {
                if let Some(rtmp_service) = listener.service.as_deref().and_then(|name| {
                    config
                        .rtmp_services
                        .iter()
                        .find(|candidate| candidate.name == name)
                }) {
                    attributes["outboundChunkSize"] = json!(rtmp_service.outbound_chunk_size);
                    attributes["accessLog"] = json!(match rtmp_service.access_log {
                        Some(oxiroute_config::AccessLogPolicy::Disabled) => "disabled",
                        Some(oxiroute_config::AccessLogPolicy::File { .. }) => "structured_file",
                        None => "default_disabled",
                    });
                    attributes["applications"] = rtmp_applications(rtmp_service);
                } else {
                    attributes["applications"] = Value::Array(Vec::new());
                }
            }
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind,
                name: listener.name.clone(),
                config_path: config_path.clone(),
                attributes,
            });
            self.listener_nodes.insert(service.name.clone(), id.clone());

            if let Some(service) = &listener.service {
                let target = match listener.protocol {
                    Protocol::Http => http_service_id(service),
                    Protocol::Tcp => l4_service_id(service),
                    Protocol::Rtmp
                    | Protocol::ForwardHttp1
                    | Protocol::ForwardHttp2
                    | Protocol::ForwardHttp3 => continue,
                };
                self.edges.push(TopologyEdge::new(
                    TopologyEdgeKind::DispatchService,
                    &id,
                    &target,
                    format!("{config_path}/service"),
                ));
            }
            if let Some(profile) = &listener.tls_profile {
                self.edges.push(TopologyEdge::new(
                    TopologyEdgeKind::ListenerTls,
                    &id,
                    &tls_profile_id(profile),
                    format!("{config_path}/tls_profile"),
                ));
            }
        }
    }

    fn add_tls(&mut self, config: &Config) {
        for (index, profile) in config.tls_profiles.iter().enumerate() {
            let id = tls_profile_id(&profile.name);
            let config_path = format!("/tls_profiles/{index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind: TopologyNodeKind::TlsProfile,
                name: profile.name.clone(),
                config_path: config_path.clone(),
                attributes: json!({
                    "certificates": profile.certificates,
                    "defaultCertificate": profile.default_certificate,
                    "minVersion": tls_version(profile.min_version),
                    "alpn": profile.alpn.iter().copied().map(alpn_protocol).collect::<Vec<_>>(),
                }),
            });
            for (certificate_index, certificate) in profile.certificates.iter().enumerate() {
                self.edges.push(TopologyEdge::new(
                    TopologyEdgeKind::TlsCertificate,
                    &id,
                    &certificate_id(certificate),
                    format!("{config_path}/certificates/{certificate_index}"),
                ));
            }
        }
        for (index, certificate) in config.certificates.iter().enumerate() {
            self.nodes.push(TopologyNode {
                id: certificate_id(&certificate.name),
                kind: TopologyNodeKind::Certificate,
                name: certificate.name.clone(),
                config_path: format!("/certificates/{index}"),
                attributes: json!({
                    "dnsNames": certificate.dns_names,
                    "source": certificate_source(&certificate.source),
                }),
            });
        }
    }

    fn add_http_services(&mut self, config: &Config) {
        for (service_index, service) in config.http_services.iter().enumerate() {
            let id = http_service_id(&service.name);
            let config_path = format!("/http_services/{service_index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind: TopologyNodeKind::HttpService,
                name: service.name.clone(),
                config_path: config_path.clone(),
                attributes: json!({
                    "upstreamIoTimeoutMs": service.upstream_io_timeout_ms,
                    "maxRequestBodyBytes": service.max_request_body_bytes,
                    "gzip": service.gzip.as_ref().map(|gzip| json!({
                        "level": gzip.level,
                        "contentTypes": gzip.content_types,
                    })),
                    "accessLog": match service.access_log {
                        Some(oxiroute_config::AccessLogPolicy::Disabled) => "disabled",
                        Some(oxiroute_config::AccessLogPolicy::File { .. }) => "structured_file",
                        None => "default_disabled",
                    },
                }),
            });
            for (route_index, route) in service.routes.iter().enumerate() {
                let route_id = http_route_id(&service.name, route);
                let route_path = format!("{config_path}/routes/{route_index}");
                self.nodes.push(TopologyNode {
                    id: route_id.clone(),
                    kind: TopologyNodeKind::HttpRoute,
                    name: route_name(route),
                    config_path: route_path.clone(),
                    attributes: json!({
                        "host": route.host.as_ref().map(host_selector),
                        "path": path_selector(&route.path),
                        "methods": route.methods,
                        "access": route.access_policy.as_ref().map(access_policy),
                        "policy": {
                            "maxRequestBodyBytes": route.policy.max_request_body_bytes,
                            "connectTimeoutMs": route.policy.connect_timeout_ms,
                            "readTimeoutMs": route.policy.read_timeout_ms,
                            "writeTimeoutMs": route.policy.write_timeout_ms,
                            "requestBuffering": route.policy.request_buffering,
                            "responseBuffering": route.policy.response_buffering,
                        },
                        "action": route_action(&route.action),
                    }),
                });
                self.edges.push(TopologyEdge::new(
                    TopologyEdgeKind::ServiceRoute,
                    &id,
                    &route_id,
                    route_path.clone(),
                ));
                if let HttpRouteAction::Proxy { upstream_pool, .. } = &route.action {
                    self.edges.push(TopologyEdge::new(
                        TopologyEdgeKind::RoutePool,
                        &route_id,
                        &pool_id(upstream_pool),
                        format!("{route_path}/action/upstream_pool"),
                    ));
                }
            }
        }
    }

    fn add_l4_services(&mut self, config: &Config) {
        for (index, service) in config.l4_services.iter().enumerate() {
            let id = l4_service_id(&service.name);
            let config_path = format!("/l4_services/{index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind: TopologyNodeKind::L4Service,
                name: service.name.clone(),
                config_path: config_path.clone(),
                attributes: json!({
                    "upstreamPool": service.upstream_pool,
                    "connectTimeoutMs": service.connect_timeout_ms,
                    "idleTimeoutMs": service.idle_timeout_ms,
                    "lifetimeTimeoutMs": service.lifetime_timeout_ms,
                }),
            });
            self.edges.push(TopologyEdge::new(
                TopologyEdgeKind::ServicePool,
                &id,
                &pool_id(&service.upstream_pool),
                format!("{config_path}/upstream_pool"),
            ));
        }
    }

    fn add_upstream_pools(&mut self, config: &Config, pools: &[Arc<RoundRobinPool>]) {
        for (pool_index, (pool, runtime_pool)) in
            config.upstream_pools.iter().zip(pools).enumerate()
        {
            let id = pool_id(&pool.name);
            let config_path = format!("/upstream_pools/{pool_index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind: TopologyNodeKind::UpstreamPool,
                name: pool.name.clone(),
                config_path: config_path.clone(),
                attributes: json!({
                    "algorithm": upstream_algorithm(pool.algorithm),
                    "healthCheck": pool.health_check.as_ref().map(health_check),
                    "tls": pool.tls.as_ref().map(upstream_tls),
                    "httpVersions": {
                        "min": http_version(pool.http_versions.min),
                        "max": http_version(pool.http_versions.max),
                    },
                }),
            });
            let runtime_pool_name = runtime_pool.health_snapshot().name;
            self.pool_nodes
                .insert(runtime_pool_name.clone(), id.clone());

            for ((server_index, server), (_, runtime_name, _runtime_endpoint)) in
                pool.servers.iter().enumerate().zip(runtime_pool.servers())
            {
                let endpoint = &server.endpoint;
                let endpoint_id = endpoint_id(&pool.name, &server.name);
                let endpoint_name = server.name.clone();
                let endpoint_path = format!("{config_path}/servers/{server_index}");
                let mut attributes = endpoint_attributes(endpoint);
                attributes["address"] = json!(endpoint.to_string());
                attributes["maxConnections"] = json!(server.max_connections);
                attributes["serverName"] = json!(server.name);
                self.nodes.push(TopologyNode {
                    id: endpoint_id.clone(),
                    kind: TopologyNodeKind::Endpoint,
                    name: endpoint_name.clone(),
                    config_path: endpoint_path.clone(),
                    attributes,
                });
                self.edges.push(TopologyEdge::new(
                    TopologyEdgeKind::PoolEndpoint,
                    &id,
                    &endpoint_id,
                    endpoint_path,
                ));
                self.endpoint_nodes
                    .insert((runtime_pool_name.clone(), runtime_name), endpoint_id);
            }
        }
    }

    fn finish(self) -> TopologySnapshot {
        TopologySnapshot {
            nodes: self.nodes.into_boxed_slice(),
            edges: self.edges.into_boxed_slice(),
            listener_nodes: self.listener_nodes,
            pool_nodes: self.pool_nodes,
            endpoint_nodes: self.endpoint_nodes,
        }
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
    nodes: &'a [TopologyNode],
    edges: &'a [TopologyEdge],
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

fn rtmp_applications(service: &oxiroute_config::RtmpService) -> Value {
    Value::Array(
        service
            .applications
            .iter()
            .map(|application| {
                let manual_recorders = application
                    .recorders
                    .iter()
                    .filter(|recorder| recorder.start == RtmpRecorderStart::Manual)
                    .count();
                json!({
                    "name": application.name,
                    "live": application.live,
                    "idleStreams": application.idle_streams,
                    "pushTargetCount": application.push_targets.len(),
                    "fanout": {
                        "maxSubscribers": application.fanout.max_subscribers,
                        "maxQueueMessagesPerSubscriber": application.fanout.max_queue_messages_per_subscriber,
                        "maxQueueBytesPerSubscriber": application.fanout.max_queue_bytes_per_subscriber,
                    },
                    "recording": {
                        "supported": !application.recorders.is_empty(),
                        "recorderCount": application.recorders.len(),
                        "manualRecorderCount": manual_recorders,
                        "continuousRecorderCount": application.recorders.len() - manual_recorders,
                    },
                })
            })
            .collect(),
    )
}

fn stable_id(kind: &str, components: &[&str]) -> String {
    let capacity = kind.len()
        + components
            .iter()
            .map(|component| component.len() + 24)
            .sum::<usize>();
    let mut id = String::with_capacity(capacity);
    id.push_str(kind);
    for component in components {
        write!(id, ":{}:{component}", component.len()).expect("writing to a String cannot fail");
    }
    id
}

fn listener_id(name: &str, kind: TopologyNodeKind) -> String {
    let prefix = if kind == TopologyNodeKind::RtmpListener {
        "rtmp_listener"
    } else {
        "listener"
    };
    stable_id(prefix, &[name])
}

fn tls_profile_id(name: &str) -> String {
    stable_id("tls_profile", &[name])
}

fn certificate_id(name: &str) -> String {
    stable_id("certificate", &[name])
}

fn http_service_id(name: &str) -> String {
    stable_id("http_service", &[name])
}

fn http_route_id(service: &str, route: &HttpRoute) -> String {
    let mut methods = route.methods.clone();
    methods.sort_unstable();
    let host = route
        .host
        .as_ref()
        .map_or_else(String::new, |host| match host {
            HttpHostSelector::NormalizedHost { value } => format!("normalized_host:{value}"),
            HttpHostSelector::ExactAuthority { value } => format!("exact_authority:{value}"),
            HttpHostSelector::NginxLeadingWildcard { value } => {
                format!("nginx_leading_wildcard:{value}")
            }
            HttpHostSelector::NginxLeadingDot { value } => {
                format!("nginx_leading_dot:{value}")
            }
        });
    let path = match &route.path {
        HttpPathSelector::SegmentPrefix { value } => format!("segment_prefix:{value}"),
        HttpPathSelector::RawPrefix { value } => format!("raw_prefix:{value}"),
        HttpPathSelector::Exact { value } => format!("exact:{value}"),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => {
            format!("ascii_case_insensitive_exact:{value}")
        }
    };
    stable_id("http_route", &[service, &host, &path, &methods.join(",")])
}

fn l4_service_id(name: &str) -> String {
    stable_id("l4_service", &[name])
}

fn pool_id(name: &str) -> String {
    stable_id("upstream_pool", &[name])
}

fn endpoint_id(pool: &str, server: &str) -> String {
    stable_id("upstream_server", &[pool, server])
}

const fn edge_kind_name(kind: TopologyEdgeKind) -> &'static str {
    match kind {
        TopologyEdgeKind::DispatchService => "dispatch_service",
        TopologyEdgeKind::ServiceRoute => "service_route",
        TopologyEdgeKind::RoutePool => "route_pool",
        TopologyEdgeKind::ServicePool => "service_pool",
        TopologyEdgeKind::PoolEndpoint => "pool_endpoint",
        TopologyEdgeKind::ListenerTls => "listener_tls",
        TopologyEdgeKind::TlsCertificate => "tls_certificate",
    }
}

fn route_name(route: &HttpRoute) -> String {
    let host = route.host.as_ref().map_or("*", |selector| match selector {
        HttpHostSelector::NormalizedHost { value }
        | HttpHostSelector::ExactAuthority { value }
        | HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => value,
    });
    let path = match &route.path {
        HttpPathSelector::SegmentPrefix { value }
        | HttpPathSelector::RawPrefix { value }
        | HttpPathSelector::Exact { value }
        | HttpPathSelector::AsciiCaseInsensitiveExact { value } => value,
    };
    format!("{host} {path}")
}

fn host_selector(selector: &HttpHostSelector) -> Value {
    match selector {
        HttpHostSelector::NormalizedHost { value } => json!({
            "kind": "normalized_host",
            "value": value,
        }),
        HttpHostSelector::ExactAuthority { value } => json!({
            "kind": "exact_authority",
            "value": value,
        }),
        HttpHostSelector::NginxLeadingWildcard { value } => json!({
            "kind": "nginx_leading_wildcard",
            "value": value,
        }),
        HttpHostSelector::NginxLeadingDot { value } => json!({
            "kind": "nginx_leading_dot",
            "value": value,
        }),
    }
}

fn path_selector(selector: &HttpPathSelector) -> Value {
    match selector {
        HttpPathSelector::SegmentPrefix { value } => json!({
            "kind": "segment_prefix",
            "value": value,
        }),
        HttpPathSelector::RawPrefix { value } => json!({
            "kind": "raw_prefix",
            "value": value,
        }),
        HttpPathSelector::Exact { value } => json!({
            "kind": "exact",
            "value": value,
        }),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => json!({
            "kind": "ascii_case_insensitive_exact",
            "value": value,
        }),
    }
}

fn access_policy(policy: &HttpAccessPolicy) -> Value {
    match policy {
        HttpAccessPolicy::BearerTokenFile {
            token_file_path: _,
            header_name,
            realm,
        } => json!({
            "type": "bearer_token_file",
            "headerName": header_name,
            "realm": realm,
        }),
        HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: _,
            realm,
        } => json!({
            "type": "basic_htpasswd_file",
            "realm": realm,
        }),
    }
}

fn route_action(action: &HttpRouteAction) -> Value {
    match action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => json!({
            "type": "proxy",
            "upstreamPool": upstream_pool,
            "upstreamHost": match &policy.upstream_host {
                HttpUpstreamHost::PreserveIncoming => "preserve_incoming",
                HttpUpstreamHost::NginxHost { .. } => "nginx_host",
                HttpUpstreamHost::Endpoint { .. } => "endpoint",
                HttpUpstreamHost::Literal { .. } => "literal",
            },
            "requestHeaderMutationCount": policy.request_headers.len(),
            "responseHeaderMutationCount": policy.response_headers.len(),
            "cookiePathRewriteCount": policy.response_cookie_path_rewrites.len(),
            "cookieAttributePolicyCount": policy.response_cookie_attributes.len(),
            "retry": {
                "maxRetries": policy.retry.max_retries,
                "triggers": policy.retry.triggers,
                "target": policy.retry.target,
                "delayMs": policy.retry.delay_ms,
            },
        }),
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => json!({
            "type": "fixed_response",
            "status": status,
            "bodyBytes": body.len(),
            "headerCount": headers.len(),
        }),
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => json!({
            "type": "redirect",
            "status": status,
            "locationType": match location {
                oxiroute_config::HttpRedirectLocation::Literal { .. } => "literal",
                oxiroute_config::HttpRedirectLocation::RequestTemplate { .. } => "request_template",
            },
            "headerCount": headers.len(),
        }),
        HttpRouteAction::StaticFiles {
            root_directory: _,
            path_mapping,
            index_files,
            spa_fallback,
            try_files,
            autoindex,
            autoindex_exact_size,
            autoindex_local_time,
            mime,
            headers,
            error_responses,
            ..
        } => json!({
            "type": "static_files",
            "pathMapping": match path_mapping {
                oxiroute_config::HttpStaticPathMapping::Root => "root",
                oxiroute_config::HttpStaticPathMapping::Alias => "alias",
            },
            "indexFiles": index_files,
            "spaFallback": spa_fallback.is_some(),
            "tryFileCount": try_files.len(),
            "autoindex": autoindex,
            "autoindexExactSize": autoindex_exact_size,
            "autoindexLocalTime": autoindex_local_time,
            "mimeMappingCount": mime.types.len(),
            "defaultType": mime.default_type,
            "headerCount": headers.len(),
            "errorResponseCount": error_responses.len(),
        }),
    }
}

fn certificate_source(source: &CertificateSource) -> Value {
    match source {
        CertificateSource::Files {
            certificate_chain_path,
            private_key_path: _,
        } => json!({
            "type": "files",
            "certificateChainPath": certificate_chain_path,
            "privateKeyPath": "<redacted>",
        }),
        CertificateSource::Certbot {
            live_directory_path,
            archive_directory_path,
        } => json!({
            "type": "certbot",
            "liveDirectoryPath": live_directory_path,
            "archiveDirectoryPath": archive_directory_path,
        }),
    }
}

const fn tls_version(version: TlsVersion) -> &'static str {
    match version {
        TlsVersion::Tls12 => "1.2",
        TlsVersion::Tls13 => "1.3",
    }
}

const fn alpn_protocol(protocol: AlpnProtocol) -> &'static str {
    match protocol {
        AlpnProtocol::H3 => "h3",
        AlpnProtocol::H2 => "h2",
        AlpnProtocol::Http11 => "http/1.1",
    }
}

const fn upstream_algorithm(algorithm: UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
        UpstreamAlgorithm::First => "first",
    }
}

fn listener_bind(bind: &ListenerBind) -> Value {
    match bind {
        ListenerBind::Socket { address } => json!({
            "type": "socket",
            "address": address,
        }),
        ListenerBind::Udp { address } => json!({
            "type": "udp",
            "address": address,
        }),
        ListenerBind::Unix { path, mode } => json!({
            "type": "unix",
            "path": path,
            "mode": mode,
        }),
    }
}

fn endpoint_attributes(endpoint: &UpstreamEndpoint) -> Value {
    match endpoint {
        UpstreamEndpoint::Socket { address } => json!({
            "type": "socket",
            "address": address,
        }),
        UpstreamEndpoint::Dns { host, port } => json!({
            "type": "dns",
            "host": host,
            "port": port,
        }),
        UpstreamEndpoint::Unix { path } => json!({
            "type": "unix",
            "path": path,
        }),
    }
}

const fn protocol(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http => "http",
        Protocol::Rtmp => "rtmp",
        Protocol::Tcp => "tcp",
        Protocol::ForwardHttp1 => "forward_http1",
        Protocol::ForwardHttp2 => "forward_http2",
        Protocol::ForwardHttp3 => "forward_http3",
    }
}

fn health_check(check: &HealthCheck) -> Value {
    json!({
        "type": match check.kind {
            HealthCheckType::Http => "http",
            HealthCheckType::Tcp => "tcp",
        },
        "intervalMs": check.interval_ms,
        "timeoutMs": check.timeout_ms,
        "healthyThreshold": check.healthy_threshold,
        "unhealthyThreshold": check.unhealthy_threshold,
        "host": check.host,
        "path": check.path,
    })
}

fn upstream_tls(tls: &UpstreamTls) -> Value {
    json!({
        "serverName": tls.server_name,
        "caCertificatePath": tls.ca_certificate_path,
    })
}

const fn http_version(version: HttpVersion) -> &'static str {
    match version {
        HttpVersion::Http11 => "1.1",
        HttpVersion::Http2 => "2",
    }
}

const fn endpoint_state(state: crate::EndpointHealthState) -> &'static str {
    match state {
        crate::EndpointHealthState::Unchecked => "unchecked",
        crate::EndpointHealthState::Unknown => "unknown",
        crate::EndpointHealthState::Healthy => "healthy",
        crate::EndpointHealthState::Unhealthy => "unhealthy",
    }
}
