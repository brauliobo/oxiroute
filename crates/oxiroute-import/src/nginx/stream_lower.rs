use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
};

use oxiroute_config::{
    Config, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpVersionPolicy, L4Service, Listener,
    ListenerBind, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint,
    UpstreamPool, UpstreamServer, validate_config,
};

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_INVALID_VALUE, Report, Severity,
};

use super::{
    DirectiveOrigin, EffectiveStream, EffectiveStreamListen, EffectiveStreamProxyPass,
    EffectiveStreamServer, EffectiveStreamUpstream, OccurrenceDecision, OccurrenceDisposition,
    OccurrenceId, SourceGraph, StaticEndpoint, StreamDestination, StreamResolution, load,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedStreamService {
    pub path: String,
    pub server: OccurrenceId,
    pub binds: Vec<ListenerBind>,
    pub diagnostic_codes: Vec<DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamImportReport {
    pub source_graph: SourceGraph,
    pub occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    pub blocked_services: Vec<BlockedStreamService>,
    pub draft: CanonicalDraft,
    pub config: Option<Config>,
}

impl StreamImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

#[must_use]
pub fn import_stream_fragment(root: &Path, root_prefix: &Path) -> StreamImportReport {
    lower_stream(load(root, root_prefix))
}

pub(super) fn lower_stream(loaded: Report<SourceGraph>) -> StreamImportReport {
    lower_stream_with_mode(loaded, false)
}

pub(super) fn lower_stream_root(loaded: Report<SourceGraph>) -> StreamImportReport {
    lower_stream_with_mode(loaded, true)
}

fn lower_stream_with_mode(loaded: Report<SourceGraph>, complete_root: bool) -> StreamImportReport {
    let (graph, mut diagnostics) = loaded.into_parts();
    let resolved = if complete_root {
        super::stream_semantic::resolve_stream_root_graph(&graph)
    } else {
        super::stream_semantic::resolve_stream_graph(&graph)
    };
    let (resolution, resolve_diagnostics) = resolved.into_parts();
    diagnostics.extend(resolve_diagnostics);
    Lowerer::new(graph, resolution, diagnostics).run()
}

struct Lowerer {
    graph: SourceGraph,
    resolution: StreamResolution,
    diagnostics: Vec<Diagnostic>,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    blocked_services: Vec<BlockedStreamService>,
    draft: CanonicalDraft,
    upstream_pool_names: HashMap<OccurrenceId, String>,
    direct_pool_names: HashMap<StaticEndpoint, String>,
}

impl Lowerer {
    fn new(graph: SourceGraph, resolution: StreamResolution, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            provenance: Vec::new(),
            blocked_services: Vec::new(),
            draft: CanonicalDraft::default(),
            upstream_pool_names: HashMap::new(),
            direct_pool_names: HashMap::new(),
        }
    }

    fn run(mut self) -> StreamImportReport {
        let streams = self.resolution.stream_blocks.clone();
        for (stream_index, stream) in streams.iter().enumerate() {
            for (server_index, server) in stream.servers.iter().enumerate() {
                let path = format!("/nginx/stream/{stream_index}/servers/{server_index}");
                let codes = self.blocking_codes(stream, server);
                if codes.is_empty() {
                    self.lower_server(stream, server, stream_index, server_index);
                } else {
                    self.blocked_services.push(BlockedStreamService {
                        path,
                        server: server.origin.occurrence,
                        binds: server
                            .listens
                            .iter()
                            .filter_map(|listen| listen.endpoint.as_ref().and_then(listener_bind))
                            .collect(),
                        diagnostic_codes: codes,
                    });
                }
            }
        }

        let mut config = self.draft.to_config();
        let finalizable = self.blocked_services.is_empty()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity() == Severity::Error);
        let config = if finalizable {
            match validate_config(&mut config) {
                Ok(()) => Some(config),
                Err(error) => {
                    self.diagnostics.push(Diagnostic::new(
                        E_INVALID_VALUE,
                        Severity::Error,
                        DiagnosticStage::Validate,
                        format!("lowered canonical nginx stream configuration is invalid: {error}"),
                    ));
                    None
                }
            }
        } else {
            None
        };
        let ((), diagnostics) = Report::new((), self.diagnostics).into_parts();

        StreamImportReport {
            source_graph: self.graph,
            occurrence_ledger: self.resolution.decisions,
            diagnostics,
            provenance: self.provenance,
            blocked_services: self.blocked_services,
            draft: self.draft,
            config,
        }
    }

    fn lower_server(
        &mut self,
        stream: &EffectiveStream,
        server: &EffectiveStreamServer,
        stream_index: usize,
        server_index: usize,
    ) {
        let Some(proxy_pass) = &server.proxy_pass else {
            return;
        };
        let pool_name = self.pool_name(stream, proxy_pass, stream_index, server_index);
        let service_index = self.draft.l4_services.len();
        let service_name = format!("nginx-stream-service-{stream_index}-{server_index}");
        let connect_timeout_ms = server
            .connect_timeout_ms
            .unwrap_or(stream.connect_timeout_ms);
        let idle_timeout_ms = server.idle_timeout_ms.unwrap_or(stream.idle_timeout_ms);

        self.draft.l4_services.push(L4Service {
            name: service_name.clone(),
            upstream_pool: pool_name.clone(),
            connect_timeout_ms,
            idle_timeout_ms,
            lifetime_timeout_ms: None,
            udp: None,
        });
        let service_path = format!("/l4_services/{service_index}");
        let mut service_origins = vec![stream.origin.clone(), server.origin.clone()];
        service_origins.push(proxy_pass.origin.clone());
        if let Some(origin) = &stream.connect_timeout_origin {
            service_origins.push(origin.clone());
        }
        if let Some(origin) = &stream.idle_timeout_origin {
            service_origins.push(origin.clone());
        }
        if let Some(origin) = &server.connect_timeout_origin {
            service_origins.push(origin.clone());
        }
        if let Some(origin) = &server.idle_timeout_origin {
            service_origins.push(origin.clone());
        }
        self.record(service_path.clone(), service_origins.clone());
        self.record(
            format!("{service_path}/upstream_pool"),
            vec![proxy_pass.origin.clone()],
        );
        self.record(
            format!("{service_path}/connect_timeout_ms"),
            vec![
                server
                    .connect_timeout_origin
                    .clone()
                    .or_else(|| stream.connect_timeout_origin.clone())
                    .unwrap_or_else(|| stream.origin.clone()),
            ],
        );
        self.record(
            format!("{service_path}/idle_timeout_ms"),
            vec![
                server
                    .idle_timeout_origin
                    .clone()
                    .or_else(|| stream.idle_timeout_origin.clone())
                    .unwrap_or_else(|| stream.origin.clone()),
            ],
        );

        for (listen_index, listen) in server.listens.iter().enumerate() {
            self.lower_listener(
                listen,
                &service_name,
                stream_index,
                server_index,
                listen_index,
                &service_origins,
            );
        }
    }

    fn lower_listener(
        &mut self,
        listen: &EffectiveStreamListen,
        service_name: &str,
        stream_index: usize,
        server_index: usize,
        listen_index: usize,
        service_origins: &[DirectiveOrigin],
    ) {
        let Some(endpoint) = listen.endpoint.as_ref() else {
            return;
        };
        let Some(bind) = listener_bind(endpoint) else {
            return;
        };
        let listener_index = self.draft.listeners.len();
        self.draft.listeners.push(Listener {
            name: format!("nginx-stream-listener-{stream_index}-{server_index}-{listen_index}"),
            bind,
            protocol: Protocol::Tcp,
            service: Some(service_name.to_owned()),
            tls_profile: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        });
        let listener_path = format!("/listeners/{listener_index}");
        let mut origins = service_origins.to_vec();
        origins.push(listen.origin.clone());
        self.record(listener_path.clone(), origins.clone());
        for suffix in ["/name", "/bind", "/bind/type", "/protocol", "/service"] {
            self.record(format!("{listener_path}{suffix}"), origins.clone());
        }
        match endpoint {
            super::ListenEndpoint::Socket { .. } => {
                self.record(format!("{listener_path}/bind/address"), origins);
            }
            super::ListenEndpoint::Unix { .. } => {
                self.record(format!("{listener_path}/bind/path"), origins);
            }
        }
    }

    fn pool_name(
        &mut self,
        stream: &EffectiveStream,
        proxy_pass: &EffectiveStreamProxyPass,
        stream_index: usize,
        server_index: usize,
    ) -> String {
        match &proxy_pass.destination {
            StreamDestination::Upstream(occurrence) => {
                if let Some(name) = self.upstream_pool_names.get(occurrence) {
                    return name.clone();
                }
                let upstream = stream
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.origin.occurrence == *occurrence)
                    .expect("resolved stream upstream occurrence is retained");
                let name = format!(
                    "nginx-stream-upstream-{stream_index}-{}",
                    stream
                        .upstreams
                        .iter()
                        .position(|candidate| candidate.origin.occurrence == *occurrence)
                        .expect("resolved stream upstream has a declaration index")
                );
                self.insert_upstream_pool(&name, upstream);
                self.upstream_pool_names.insert(*occurrence, name.clone());
                name
            }
            StreamDestination::Direct(endpoint) => {
                if let Some(name) = self.direct_pool_names.get(endpoint) {
                    return name.clone();
                }
                let name = format!("nginx-stream-direct-{stream_index}-{server_index}");
                let origin = proxy_pass.origin.clone();
                self.insert_pool(
                    &name,
                    std::slice::from_ref(endpoint),
                    &[origin.clone()],
                    &origin,
                );
                self.direct_pool_names
                    .insert(endpoint.clone(), name.clone());
                name
            }
            StreamDestination::Unresolved | StreamDestination::Variable => {
                unreachable!("blocked stream proxy_pass is not lowered")
            }
        }
    }

    fn insert_upstream_pool(&mut self, name: &str, upstream: &EffectiveStreamUpstream) {
        let endpoints = upstream
            .servers
            .iter()
            .filter_map(|server| server.endpoint.as_ref())
            .cloned()
            .collect::<Vec<_>>();
        let origins = upstream
            .servers
            .iter()
            .map(|server| server.origin.clone())
            .collect::<Vec<_>>();
        self.insert_pool(name, &endpoints, &origins, &upstream.origin);
    }

    fn insert_pool(
        &mut self,
        name: &str,
        endpoints: &[StaticEndpoint],
        endpoint_origins: &[DirectiveOrigin],
        origin: &DirectiveOrigin,
    ) {
        let servers = endpoints
            .iter()
            .enumerate()
            .map(|(index, endpoint)| UpstreamServer {
                name: format!("{name}-endpoint-{index}"),
                endpoint: canonical_endpoint(endpoint),
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::default(),
            })
            .collect::<Vec<_>>();
        let pool_index = self.draft.upstream_pools.len();
        self.draft.upstream_pools.push(UpstreamPool {
            name: name.to_owned(),
            servers,
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Never,
        });
        let pool_path = format!("/upstream_pools/{pool_index}");
        self.record(pool_path.clone(), vec![origin.clone()]);
        for suffix in ["/name", "/algorithm", "/http_versions"] {
            self.record(format!("{pool_path}{suffix}"), vec![origin.clone()]);
        }
        self.record(format!("{pool_path}/servers"), endpoint_origins.to_vec());
        for (index, endpoint_origin) in endpoint_origins.iter().enumerate() {
            let server_path = format!("{pool_path}/servers/{index}");
            self.record(server_path.clone(), vec![endpoint_origin.clone()]);
            self.record(format!("{server_path}/name"), vec![endpoint_origin.clone()]);
            let endpoint_path = format!("{server_path}/endpoint");
            let suffixes = match self.draft.upstream_pools[pool_index].servers[index].endpoint {
                UpstreamEndpoint::Socket { .. } => ["", "/type", "/address"].as_slice(),
                UpstreamEndpoint::Dns { .. } => ["", "/type", "/host", "/port"].as_slice(),
                UpstreamEndpoint::Unix { .. } => ["", "/type", "/path"].as_slice(),
            };
            for suffix in suffixes {
                self.record(
                    format!("{endpoint_path}{suffix}"),
                    vec![endpoint_origin.clone()],
                );
            }
        }
    }

    fn blocking_codes(
        &self,
        stream: &EffectiveStream,
        server: &EffectiveStreamServer,
    ) -> Vec<DiagnosticCode> {
        let mut codes = Vec::new();
        let server_id = server.origin.occurrence;
        let stream_id = stream.origin.occurrence;
        let referenced_upstream =
            server
                .proxy_pass
                .as_ref()
                .and_then(|proxy| match proxy.destination {
                    StreamDestination::Upstream(occurrence) => Some(occurrence),
                    StreamDestination::Direct(_)
                    | StreamDestination::Unresolved
                    | StreamDestination::Variable => None,
                });
        for decision in &self.resolution.decisions {
            let OccurrenceDisposition::Blocking(code) = decision.disposition else {
                continue;
            };
            let affects_server = self.is_descendant(decision.occurrence, server_id)
                || (self.is_descendant(server_id, decision.occurrence)
                    && self.is_descendant(decision.occurrence, stream_id))
                || decision.occurrence == stream_id
                || self.is_stream_global(decision.occurrence, stream_id)
                || referenced_upstream
                    .is_some_and(|upstream| self.is_descendant(decision.occurrence, upstream));
            if affects_server && !codes.contains(&code) {
                codes.push(code);
            }
        }
        codes
    }

    fn is_stream_global(&self, occurrence: OccurrenceId, stream: OccurrenceId) -> bool {
        let mut current = occurrence;
        loop {
            if current == stream {
                return true;
            }
            let Some(item) = self.graph.expanded_occurrences.get(current.get()) else {
                return false;
            };
            if item.directive.name.value == b"server" && item.parent == Some(stream) {
                return false;
            }
            let Some(parent) = item.parent else {
                return false;
            };
            current = parent;
        }
    }

    fn is_descendant(&self, occurrence: OccurrenceId, ancestor: OccurrenceId) -> bool {
        let mut current = Some(occurrence);
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self
                .graph
                .expanded_occurrences
                .get(id.get())
                .and_then(|item| item.parent);
        }
        false
    }

    fn record(&mut self, path: String, mut origins: Vec<DirectiveOrigin>) {
        origins.sort_by_key(|origin| (origin.occurrence, origin.span));
        origins.dedup();
        self.provenance.push(CanonicalProvenance { path, origins });
    }
}

fn listener_bind(endpoint: &super::ListenEndpoint) -> Option<ListenerBind> {
    match endpoint {
        super::ListenEndpoint::Unix { path } => Some(ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        }),
        super::ListenEndpoint::Socket { address, port } => {
            let address = address
                .strip_prefix(b"[")
                .and_then(|address| address.strip_suffix(b"]"))
                .unwrap_or(address);
            let address = if address == b"*" {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            } else {
                std::str::from_utf8(address).ok()?.parse().ok()?
            };
            Some(ListenerBind::Socket {
                address: SocketAddr::new(address, *port),
            })
        }
    }
}

fn canonical_endpoint(endpoint: &StaticEndpoint) -> UpstreamEndpoint {
    match endpoint {
        StaticEndpoint::Socket { address } => UpstreamEndpoint::Socket { address: *address },
        StaticEndpoint::Dns { host, port } => UpstreamEndpoint::Dns {
            host: host.clone(),
            port: *port,
        },
        StaticEndpoint::Unix { path } => UpstreamEndpoint::Unix { path: path.clone() },
    }
}
