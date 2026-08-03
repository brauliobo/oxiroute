use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, DnsResolutionPolicy,
    DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpService, HttpVersionPolicy, Listener, ListenerBind, Protocol, TlsPolicy,
    TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
    UpstreamServer, UpstreamTls, validate_config,
};

use crate::{
    ActivationRequirement, CanonicalCandidate as SharedCanonicalCandidate, CanonicalDraft,
    CanonicalProvenance, DeploymentRequirement, DeploymentRequirementKind, Diagnostic,
    DiagnosticStage, E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, OperationalOverlayKind,
    OperationalOverlayRequirement, ProvenanceRole, Report, Severity, SourceImportMetadata,
};

use super::{
    ApacheResolution, DirectiveOrigin, EffectiveProxyPass, EffectiveVirtualHost,
    OccurrenceDecision, ProxyScheme, ProxyTarget, SourceGraph,
};

pub use super::semantic::ApacheProvenance;

pub type CanonicalCandidate = SharedCanonicalCandidate<ApacheProvenance>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedVirtualHost {
    pub address: std::net::SocketAddr,
    pub origin: ApacheProvenance,
    pub diagnostic_codes: Vec<crate::DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApacheImportReport {
    pub source_graph: SourceGraph,
    pub occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub blocked_virtual_hosts: Vec<BlockedVirtualHost>,
    pub candidate: CanonicalCandidate,
}

impl ApacheImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

pub(super) fn lower(loaded: Report<SourceGraph>) -> ApacheImportReport {
    let (graph, load_diagnostics) = loaded.into_parts();
    let resolved = super::semantic::resolve(Report::new(graph.clone(), load_diagnostics));
    let (resolution, diagnostics) = resolved.into_parts();
    Lowerer::new(graph, resolution, diagnostics).run()
}

struct Lowerer {
    graph: SourceGraph,
    resolution: ApacheResolution,
    diagnostics: Vec<Diagnostic>,
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<ApacheProvenance>>,
    deployment_requirements: Vec<DeploymentRequirement<ApacheProvenance>>,
    activation_requirements: Vec<ActivationRequirement<ApacheProvenance>>,
    operational_overlays: Vec<OperationalOverlayRequirement<ApacheProvenance>>,
    pool_names: HashMap<String, String>,
    pool_definitions: HashMap<String, LoweredPool>,
    next_pool_number: usize,
    blocked_virtual_hosts: Vec<BlockedVirtualHost>,
}

struct LoweredPool {
    pool: UpstreamPool,
    origins: Vec<ApacheProvenance>,
}

struct LoweredVirtualHost {
    routes: Vec<HttpRoute>,
    route_origins: Vec<Vec<ApacheProvenance>>,
    origins: Vec<ApacheProvenance>,
}

impl Lowerer {
    fn new(graph: SourceGraph, resolution: ApacheResolution, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            draft: CanonicalDraft::default(),
            provenance: Vec::new(),
            deployment_requirements: Vec::new(),
            activation_requirements: Vec::new(),
            operational_overlays: Vec::new(),
            pool_names: HashMap::new(),
            pool_definitions: HashMap::new(),
            next_pool_number: 1,
            blocked_virtual_hosts: Vec::new(),
        }
    }

    fn run(mut self) -> ApacheImportReport {
        for module in &self.resolution.module_loads {
            let origin = self.origin(&module.origin, ProvenanceRole::Declaration);
            self.deployment_requirements.push(DeploymentRequirement {
                kind: DeploymentRequirementKind::ModuleLoad,
                directive: "LoadModule".into(),
                value: vec![module.module.clone(), module.path.clone()],
                origin,
            });
        }

        let listens = self.resolution.listens.clone();
        for listen in listens {
            let virtual_hosts = self
                .resolution
                .virtual_hosts
                .iter()
                .filter(|virtual_host| virtual_host.address == listen.address)
                .cloned()
                .collect::<Vec<_>>();
            if virtual_hosts.is_empty() {
                continue;
            }
            self.lower_listener(&listen, &virtual_hosts);
        }

        let mut pools = self.pool_definitions.drain().collect::<Vec<_>>();
        pools.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (key, pool) in pools {
            let index = self.draft.upstream_pools.len();
            self.draft.upstream_pools.push(pool.pool);
            let path = format!("/upstream_pools/{index}");
            self.record(path.clone(), pool.origins.clone());
            self.record(format!("{path}/name"), pool.origins.clone());
            self.record(format!("{path}/algorithm"), pool.origins.clone());
            self.record(format!("{path}/servers"), pool.origins.clone());
            let server_count = self.draft.upstream_pools[index].servers.len();
            for server_index in 0..server_count {
                let server_path = format!("{path}/servers/{server_index}");
                self.record(server_path.clone(), pool.origins.clone());
                self.record(format!("{server_path}/name"), pool.origins.clone());
                self.record(format!("{server_path}/endpoint"), pool.origins.clone());
                self.record(format!("{server_path}/endpoint/type"), pool.origins.clone());
            }
            if self.draft.upstream_pools[index].tls.is_some() {
                self.record(format!("{path}/tls"), pool.origins.clone());
            }
            let _ = key;
        }

        let draft = self.draft.clone();
        let config = self.finalize(&draft);
        let source_metadata = SourceImportMetadata {
            original_sources: self
                .graph
                .sources
                .iter()
                .map(|source| source.source.clone())
                .collect(),
            ..SourceImportMetadata::default()
        };
        ApacheImportReport {
            source_graph: self.graph,
            occurrence_ledger: self.resolution.decisions,
            diagnostics: Report::new((), self.diagnostics).into_parts().1,
            blocked_virtual_hosts: self.blocked_virtual_hosts,
            candidate: CanonicalCandidate {
                draft,
                provenance: self.provenance,
                deployment_requirements: self.deployment_requirements,
                activation_requirements: self.activation_requirements,
                operational_overlays: self.operational_overlays,
                source_metadata,
                config,
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_listener(
        &mut self,
        listen: &super::EffectiveListen,
        virtual_hosts: &[EffectiveVirtualHost],
    ) {
        for virtual_host in virtual_hosts
            .iter()
            .filter(|virtual_host| !virtual_host.blocked.is_empty())
        {
            self.record_blocked_virtual_host(virtual_host);
        }
        let usable = virtual_hosts
            .iter()
            .filter(|virtual_host| virtual_host.blocked.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if usable.is_empty() {
            return;
        }

        let tls_modes = usable
            .iter()
            .map(|virtual_host| virtual_host.tls.engine_on)
            .collect::<HashSet<_>>();
        if tls_modes.len() > 1 {
            self.block_origin(
                &usable[0].origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache virtual hosts sharing one listener disagree about TLS termination",
            );
            for virtual_host in &usable {
                self.record_blocked_virtual_host_with_code(
                    virtual_host,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                );
            }
            return;
        }

        let mut routes = Vec::new();
        let mut route_origins = Vec::new();
        let mut listener_origins = vec![self.origin(&listen.origin, ProvenanceRole::Declaration)];
        for (vhost_index, virtual_host) in usable.iter().enumerate() {
            let Some(lowered) = self.lower_virtual_host(virtual_host, vhost_index == 0) else {
                self.record_blocked_virtual_host(virtual_host);
                continue;
            };
            listener_origins.extend(lowered.origins.clone());
            routes.extend(lowered.routes);
            route_origins.extend(lowered.route_origins);
        }
        if routes.is_empty() {
            return;
        }

        let listener_index = self.draft.listeners.len();
        let service_name = format!("apache-http-{}", listener_index + 1);
        let tls_profile = tls_modes
            .contains(&true)
            .then(|| self.lower_tls(&usable, listener_index));
        if let Some((certificates, profile, tls_origins)) = tls_profile {
            self.commit_certificates(certificates, &tls_origins);
            let profile_index = self.draft.tls_profiles.len();
            let profile_name = profile.name.clone();
            self.draft.tls_profiles.push(profile);
            let profile_path = format!("/tls_profiles/{profile_index}");
            for suffix in [
                "",
                "/name",
                "/certificates",
                "/default_certificate",
                "/min_version",
                "/alpn",
            ] {
                self.record(format!("{profile_path}{suffix}"), tls_origins.clone());
            }
            let listener = Listener {
                name: format!("apache-listener-{}", listener_index + 1),
                bind: ListenerBind::Socket {
                    address: listen.address,
                },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: Some(profile_name),
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            };
            self.commit_listener_and_service(
                listener,
                service_name,
                routes,
                route_origins,
                &listener_origins,
            );
        } else {
            let listener = Listener {
                name: format!("apache-listener-{}", listener_index + 1),
                bind: ListenerBind::Socket {
                    address: listen.address,
                },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            };
            self.commit_listener_and_service(
                listener,
                service_name,
                routes,
                route_origins,
                &listener_origins,
            );
        }
    }

    fn lower_virtual_host(
        &mut self,
        virtual_host: &EffectiveVirtualHost,
        default_server: bool,
    ) -> Option<LoweredVirtualHost> {
        if !proxy_order_is_canonical(virtual_host, &mut self.diagnostics, &self.graph) {
            return None;
        }
        let mut routes = Vec::new();
        let mut route_origins = Vec::new();
        let vhost_origin = self.origin(&virtual_host.origin, ProvenanceRole::Declaration);
        let mut origins = vec![vhost_origin.clone()];
        let host_selectors = virtual_host
            .names
            .iter()
            .map(|name| Some(name.selector.clone()))
            .chain(default_server.then_some(None))
            .collect::<Vec<_>>();
        for proxy in &virtual_host.proxy_passes {
            let (_pool_key, pool_name, pool_origins) = self.pool_for(proxy)?;
            let proxy_origin = self.origin(&proxy.origin, ProvenanceRole::Value);
            let mut route_sources = vec![vhost_origin.clone(), proxy_origin.clone()];
            route_sources.extend(pool_origins.clone());
            origins.extend(pool_origins);
            for host in &host_selectors {
                routes.push(HttpRoute {
                    host: host.clone(),
                    path: HttpPathSelector::RawPrefix {
                        value: proxy.path.clone(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy {
                        max_request_body_bytes: None,
                        connect_timeout_ms: 30_000,
                        read_timeout_ms: 30_000,
                        write_timeout_ms: 30_000,
                        request_buffering: false,
                        response_buffering: false,
                    },
                    action: HttpRouteAction::Proxy {
                        upstream_pool: pool_name.clone(),
                        policy: HttpProxyPolicy {
                            upstream_host: if virtual_host.preserve_host {
                                oxiroute_config::HttpUpstreamHost::PreserveIncoming
                            } else {
                                oxiroute_config::HttpUpstreamHost::Endpoint {
                                    unix_fallback: None,
                                }
                            },
                            ..HttpProxyPolicy::default()
                        },
                    },
                });
                route_origins.push(route_sources.clone());
            }
        }
        Some(LoweredVirtualHost {
            routes,
            route_origins,
            origins,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn pool_for(
        &mut self,
        proxy: &EffectiveProxyPass,
    ) -> Option<(String, String, Vec<ApacheProvenance>)> {
        let key = match &proxy.target {
            ProxyTarget::Direct {
                scheme, host, port, ..
            } => format!("direct:{}://{host}:{port}", scheme.as_str()),
            ProxyTarget::Balancer { name, .. } => format!("balancer:{name}"),
        };
        if let Some(name) = self.pool_names.get(&key) {
            let origins = self
                .pool_definitions
                .get(&key)
                .map_or_else(Vec::new, |pool| pool.origins.clone());
            return Some((key, name.clone(), origins));
        }
        let proxy_origin = self.origin(&proxy.origin, ProvenanceRole::Reference);
        let (name, pool, mut origins) = match &proxy.target {
            ProxyTarget::Direct {
                scheme,
                endpoint,
                host,
                ..
            } => {
                let tls = match scheme {
                    ProxyScheme::Http => None,
                    ProxyScheme::Https if host.parse::<std::net::IpAddr>().is_ok() => {
                        self.block_origin(
                            &proxy.origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "HTTPS ProxyPass to an IP address requires an explicit verified SNI overlay",
                        );
                        return None;
                    }
                    ProxyScheme::Https => Some(UpstreamTls {
                        server_name: host.clone(),
                        ca_certificate_path: None,
                    }),
                };
                let name = format!("apache-pool-{}", self.next_pool_number);
                self.next_pool_number += 1;
                let pool = UpstreamPool {
                    name: name.clone(),
                    servers: vec![UpstreamServer {
                        name: "member-1".into(),
                        endpoint: endpoint.clone(),
                        max_connections: None,
                        dns_resolution: DnsResolutionPolicy::default(),
                    }],
                    endpoints: Vec::new(),
                    algorithm: UpstreamAlgorithm::RoundRobin,
                    health_check: None,
                    tls,
                    http_versions: HttpVersionPolicy::default(),
                    queue_timeout_ms: None,
                    connect_timeout_ms: None,
                    server_timeout_ms: None,
                    connection_reuse: UpstreamConnectionReuse::default(),
                };
                (name, pool, vec![proxy_origin.clone()])
            }
            ProxyTarget::Balancer {
                name: balancer_name,
                ..
            } => {
                let Some(balancer) = self
                    .resolution
                    .balancers
                    .iter()
                    .find(|balancer| balancer.name == *balancer_name)
                    .cloned()
                else {
                    self.block_origin(
                        &proxy.origin,
                        crate::E_UNRESOLVED_REFERENCE,
                        format!("ProxyPass references unknown balancer `{balancer_name}`"),
                    );
                    return None;
                };
                let Some((first_scheme, first_host)) = balancer
                    .members
                    .first()
                    .map(|member| (member.scheme, member.host.clone()))
                else {
                    self.block_origin(
                        &proxy.origin,
                        crate::E_UNRESOLVED_REFERENCE,
                        "ProxyPass references a balancer with no static members",
                    );
                    return None;
                };
                if balancer
                    .members
                    .iter()
                    .any(|member| member.scheme != first_scheme)
                {
                    self.block_origin(
                        &proxy.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "Apache balancer members mix HTTP and HTTPS origins",
                    );
                    return None;
                }
                if first_scheme == ProxyScheme::Https
                    && balancer.members.iter().any(|member| {
                        member.host != first_host || member.host.parse::<std::net::IpAddr>().is_ok()
                    })
                {
                    self.block_origin(
                        &proxy.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "HTTPS balancer members require one shared verified DNS SNI identity",
                    );
                    return None;
                }
                let pool_name = format!("apache-balancer-{balancer_name}");
                let tls = (first_scheme == ProxyScheme::Https).then(|| UpstreamTls {
                    server_name: first_host.clone(),
                    ca_certificate_path: None,
                });
                let mut pool_origins = vec![
                    proxy_origin.clone(),
                    self.origin(&balancer.origin, ProvenanceRole::Reference),
                ];
                let servers = balancer
                    .members
                    .iter()
                    .enumerate()
                    .map(|(index, member)| {
                        pool_origins.push(self.origin(&member.origin, ProvenanceRole::Reference));
                        UpstreamServer {
                            name: format!("member-{}", index + 1),
                            endpoint: member.endpoint.clone(),
                            max_connections: None,
                            dns_resolution: DnsResolutionPolicy::default(),
                        }
                    })
                    .collect();
                let pool = UpstreamPool {
                    name: pool_name.clone(),
                    servers,
                    endpoints: Vec::new(),
                    algorithm: UpstreamAlgorithm::RoundRobin,
                    health_check: None,
                    tls,
                    http_versions: HttpVersionPolicy::default(),
                    queue_timeout_ms: None,
                    connect_timeout_ms: None,
                    server_timeout_ms: None,
                    connection_reuse: UpstreamConnectionReuse::default(),
                };
                (pool_name, pool, pool_origins)
            }
        };
        origins.sort_by_key(|origin| (origin.source, origin.span));
        origins.dedup_by_key(|origin| (origin.source, origin.span));
        self.pool_names.insert(key.clone(), name.clone());
        self.pool_definitions.insert(
            key.clone(),
            LoweredPool {
                pool,
                origins: origins.clone(),
            },
        );
        Some((key, name, origins))
    }

    fn lower_tls(
        &mut self,
        virtual_hosts: &[EffectiveVirtualHost],
        listener_index: usize,
    ) -> (Vec<Certificate>, TlsProfile, Vec<ApacheProvenance>) {
        let mut certificates = Vec::<Certificate>::new();
        let mut names = Vec::<String>::new();
        let mut origins = Vec::new();
        let mut certificate_identities = HashMap::<(PathBuf, PathBuf), String>::new();
        for virtual_host in virtual_hosts {
            if !virtual_host.tls.engine_on || !virtual_host.blocked.is_empty() {
                continue;
            }
            let (Some(chain), Some(key)) = (
                virtual_host.tls.certificate_chain.as_ref(),
                virtual_host.tls.private_key.as_ref(),
            ) else {
                continue;
            };
            let chain_origin = self.origin(&chain.origin, ProvenanceRole::Value);
            let key_origin = self.origin(&key.origin, ProvenanceRole::Value);
            origins.extend([chain_origin.clone(), key_origin.clone()]);
            for (kind, path, origin) in [
                ("certificate", &chain.path, chain_origin.clone()),
                ("private-key", &key.path, key_origin.clone()),
            ] {
                self.operational_overlays
                    .push(OperationalOverlayRequirement {
                        id: format!("apache-tls-{kind}-{}", origin.span.range().start()),
                        kind: OperationalOverlayKind::CertificateMaterial,
                        origin: Some(origin.clone()),
                        redacted_evidence: false,
                        values: vec![path.display().to_string()],
                        satisfied: true,
                    });
            }
            let identity = (chain.path.clone(), key.path.clone());
            let certificate_name = if let Some(name) = certificate_identities.get(&identity) {
                name.clone()
            } else {
                let name = format!("apache-certificate-{}", certificates.len() + 1);
                certificate_identities.insert(identity, name.clone());
                certificates.push(Certificate {
                    name: name.clone(),
                    dns_names: Vec::new(),
                    source: CertificateSource::Files {
                        certificate_chain_path: chain.path.clone(),
                        private_key_path: key.path.clone(),
                    },
                });
                name
            };
            if !names.contains(&certificate_name) {
                names.push(certificate_name.clone());
            }
            let certificate = certificates
                .iter_mut()
                .find(|certificate| certificate.name == certificate_name)
                .expect("certificate identity was inserted");
            for name in &virtual_host.names {
                if !certificate.dns_names.contains(&name.certificate_name) {
                    certificate.dns_names.push(name.certificate_name.clone());
                }
            }
        }
        let Some(default_certificate) = names.first().cloned() else {
            return (
                Vec::new(),
                TlsProfile {
                    name: format!("apache-tls-profile-{}", listener_index + 1),
                    certificates: vec!["missing".into()],
                    default_certificate: "missing".into(),
                    min_version: TlsVersion::Tls12,
                    alpn: vec![AlpnProtocol::Http11],
                    policy: TlsPolicy::default(),
                },
                origins,
            );
        };
        let profile = TlsProfile {
            name: format!("apache-tls-profile-{}", listener_index + 1),
            certificates: names,
            default_certificate,
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::Http11],
            policy: TlsPolicy::default(),
        };
        (certificates, profile, origins)
    }

    fn commit_certificates(
        &mut self,
        certificates: Vec<Certificate>,
        origins: &[ApacheProvenance],
    ) {
        for certificate in certificates {
            let index = self.draft.certificates.len();
            let path = format!("/certificates/{index}");
            self.draft.certificates.push(certificate);
            for suffix in [
                "",
                "/name",
                "/dns_names",
                "/source",
                "/source/type",
                "/source/certificate_chain_path",
                "/source/private_key_path",
            ] {
                self.record(format!("{path}{suffix}"), origins.to_vec());
            }
        }
    }

    fn commit_listener_and_service(
        &mut self,
        listener: Listener,
        service_name: String,
        routes: Vec<HttpRoute>,
        route_origins: Vec<Vec<ApacheProvenance>>,
        listener_origins: &[ApacheProvenance],
    ) {
        let listener_index = self.draft.listeners.len();
        self.draft.listeners.push(listener);
        let listener_path = format!("/listeners/{listener_index}");
        for suffix in [
            "",
            "/name",
            "/bind",
            "/bind/type",
            "/bind/address",
            "/protocol",
            "/service",
        ] {
            self.record(
                format!("{listener_path}{suffix}"),
                listener_origins.to_vec(),
            );
        }
        let service_index = self.draft.http_services.len();
        self.draft.http_services.push(HttpService {
            name: service_name,
            routes,
            automatic_response_headers: false,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        });
        let service_path = format!("/http_services/{service_index}");
        self.record(service_path.clone(), listener_origins.to_vec());
        self.record(
            format!("{service_path}/automatic_response_headers"),
            listener_origins.to_vec(),
        );
        self.record(
            format!("{service_path}/upstream_io_timeout_ms"),
            listener_origins.to_vec(),
        );
        self.record(
            format!("{service_path}/max_request_body_bytes"),
            listener_origins.to_vec(),
        );
        for (route_index, origins) in route_origins.into_iter().enumerate() {
            let route_path = format!("{service_path}/routes/{route_index}");
            let route = self.draft.http_services[service_index].routes[route_index].clone();
            for suffix in [
                "",
                "/path",
                "/path/kind",
                "/path/value",
                "/methods",
                "/action",
                "/action/type",
            ] {
                self.record(format!("{route_path}{suffix}"), origins.clone());
            }
            if route.host.is_some() {
                for suffix in ["/host", "/host/kind", "/host/value"] {
                    self.record(format!("{route_path}{suffix}"), origins.clone());
                }
            }
            if let HttpRouteAction::Proxy { .. } = &route.action {
                for suffix in [
                    "/action/upstream_pool",
                    "/action/policy",
                    "/action/policy/upstream_host",
                    "/action/policy/upstream_host/type",
                    "/action/policy/request_headers",
                    "/action/policy/response_headers",
                    "/action/policy/retry",
                ] {
                    self.record(format!("{route_path}{suffix}"), origins.clone());
                }
            }
        }
    }

    fn record_blocked_virtual_host(&mut self, virtual_host: &EffectiveVirtualHost) {
        if virtual_host.blocked.is_empty() {
            return;
        }
        self.blocked_virtual_hosts.push(BlockedVirtualHost {
            address: virtual_host.address,
            origin: self.origin(&virtual_host.origin, ProvenanceRole::Declaration),
            diagnostic_codes: virtual_host.blocked.clone(),
        });
    }

    fn record_blocked_virtual_host_with_code(
        &mut self,
        virtual_host: &EffectiveVirtualHost,
        code: crate::DiagnosticCode,
    ) {
        let mut virtual_host = virtual_host.clone();
        virtual_host.blocked.push(code);
        self.record_blocked_virtual_host(&virtual_host);
    }

    fn finalize(&mut self, draft: &CanonicalDraft) -> Option<Config> {
        if self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
        {
            return None;
        }
        let mut config = draft.to_config();
        if let Err(error) = validate_config(&mut config) {
            let mut diagnostic = Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Validate,
                format!("lowered Apache canonical draft is invalid: {error}"),
            );
            if let Some(origin) = self
                .provenance
                .first()
                .and_then(|entry| entry.origins.first())
            {
                diagnostic = diagnostic
                    .with_primary_span(origin.span)
                    .with_include_stack(
                        origin
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    );
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        Some(config)
    }

    #[allow(clippy::naive_bytecount)]
    fn origin(&self, origin: &DirectiveOrigin, role: ProvenanceRole) -> ApacheProvenance {
        let source = self
            .graph
            .source(origin.provenance.source)
            .expect("Apache provenance source retained in graph");
        let offset = origin.span.range().start().min(source.source.len());
        ApacheProvenance {
            role,
            source: origin.provenance.source,
            path: source.canonical_path.clone(),
            line: source.source.bytes()[..offset]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count()
                + 1,
            span: origin.span,
            include_stack: origin.provenance.include_stack.clone(),
        }
    }

    fn record(&mut self, path: String, mut origins: Vec<ApacheProvenance>) {
        origins.sort_by_key(|origin| (origin.source, origin.span, origin.role as u8));
        origins.dedup_by_key(|origin| (origin.source, origin.span, origin.role as u8));
        if let Some(existing) = self.provenance.iter_mut().find(|entry| entry.path == path) {
            existing.origins.extend(origins);
            existing
                .origins
                .sort_by_key(|origin| (origin.source, origin.span, origin.role as u8));
            existing
                .origins
                .dedup_by_key(|origin| (origin.source, origin.span, origin.role as u8));
        } else {
            self.provenance.push(CanonicalProvenance { path, origins });
        }
    }

    fn block_origin(
        &mut self,
        origin: &DirectiveOrigin,
        code: crate::DiagnosticCode,
        message: impl Into<String>,
    ) {
        let source = self.origin(origin, ProvenanceRole::Value);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Lower, message)
                .with_primary_span(source.span)
                .with_include_stack(
                    source
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }
}

fn proxy_order_is_canonical(
    virtual_host: &EffectiveVirtualHost,
    diagnostics: &mut Vec<Diagnostic>,
    graph: &SourceGraph,
) -> bool {
    let mut valid = true;
    for (first_index, first) in virtual_host.proxy_passes.iter().enumerate() {
        for second in virtual_host.proxy_passes.iter().skip(first_index + 1) {
            let same_path = first.path == second.path;
            if same_path
                || (prefix_overlap(&first.path, &second.path)
                    && first.path.len() < second.path.len())
            {
                let origin = apache_origin(graph, &second.origin, ProvenanceRole::Value);
                let first_origin = apache_origin(graph, &first.origin, ProvenanceRole::Value);
                diagnostics.push(
                    Diagnostic::new(
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        Severity::Error,
                        DiagnosticStage::Lower,
                        "ordered Apache ProxyPass rules would change under canonical longest-prefix routing",
                    )
                    .with_primary_span(origin.span)
                    .with_related_span(first_origin.span, "earlier ProxyPass rule is here")
                    .with_include_stack(
                        origin.include_stack.iter().map(|frame| frame.directive_span),
                    ),
                );
                valid = false;
            }
        }
    }
    valid
}

fn prefix_overlap(first: &str, second: &str) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

#[allow(clippy::naive_bytecount)]
fn apache_origin(
    graph: &SourceGraph,
    origin: &DirectiveOrigin,
    role: ProvenanceRole,
) -> ApacheProvenance {
    let source = graph
        .source(origin.provenance.source)
        .expect("Apache provenance source retained in graph");
    let offset = origin.span.range().start().min(source.source.len());
    ApacheProvenance {
        role,
        source: origin.provenance.source,
        path: source.canonical_path.clone(),
        line: source.source.bytes()[..offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
        span: origin.span,
        include_stack: origin.provenance.include_stack.clone(),
    }
}
