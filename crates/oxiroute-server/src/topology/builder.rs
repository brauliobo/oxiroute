#[allow(clippy::wildcard_imports)]
use super::*;
use super::{
    attributes::{
        access_policy, alpn_protocol, certificate_source, endpoint_attributes, health_check,
        host_selector, http_version, listener_bind, path_selector, protocol, route_action,
        rtmp_applications, tls_client_auth_mode, tls_version, upstream_algorithm, upstream_tls,
    },
    ids::{
        certificate_id, endpoint_id, forward_proxy_service_id, http_route_id, http_service_id,
        l4_service_id, listener_id, pool_id, route_name, tls_profile_id,
    },
};

pub(super) struct TopologyBuilder {
    nodes: Vec<TopologyNode>,
    edges: Vec<TopologyEdge>,
    listener_nodes: HashMap<String, String>,
    pool_nodes: HashMap<String, String>,
    pool_paths: HashMap<String, String>,
    static_endpoints: HashMap<String, HashSet<String>>,
}

impl TopologyBuilder {
    pub(super) fn new(config: &ConfigDraft) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            listener_nodes: HashMap::with_capacity(
                config.listeners.len() + config.stats.as_ref().map_or(0, |stats| stats.pages.len()),
            ),
            pool_nodes: HashMap::with_capacity(config.upstream_pools.len()),
            pool_paths: HashMap::with_capacity(config.upstream_pools.len()),
            static_endpoints: HashMap::with_capacity(config.upstream_pools.len()),
        }
    }

    pub(super) fn add_listeners(&mut self, config: &ConfigDraft) {
        for (index, listener) in config.listeners.iter().enumerate() {
            let kind = match listener.protocol {
                Protocol::Rtmp => TopologyNodeKind::RtmpListener,
                Protocol::Http | Protocol::Http3 | Protocol::Tcp | Protocol::Udp => {
                    TopologyNodeKind::Listener
                }
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
            self.listener_nodes
                .insert(listener.name.clone(), id.clone());

            if let Some(service) = &listener.service {
                let target = match listener.protocol {
                    Protocol::Http | Protocol::Http3 => http_service_id(service),
                    Protocol::Tcp | Protocol::Udp => l4_service_id(service),
                    Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => {
                        forward_proxy_service_id(service)
                    }
                    Protocol::Rtmp => continue,
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

    pub(super) fn add_stats_pages(&mut self, config: &ConfigDraft) {
        let Some(stats) = &config.stats else {
            return;
        };
        for (index, page) in stats.pages.iter().enumerate() {
            let name = format!("@stats-page-{index}");
            let kind = TopologyNodeKind::Listener;
            let id = listener_id(&name, kind);
            let config_path = format!("/stats/pages/{index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind,
                name: name.clone(),
                config_path,
                attributes: json!({
                    "bind": listener_bind(&ListenerBind::Socket { address: page.bind }),
                    "protocol": "http",
                    "maxConnections": page.max_connections,
                    "downstreamTimeouts": {
                        "clientTimeoutMs": page.downstream_timeouts.client_timeout_ms,
                        "requestTimeoutMs": page.downstream_timeouts.request_timeout_ms,
                        "keepaliveTimeoutMs": page.downstream_timeouts.keepalive_timeout_ms,
                    },
                    "uriPrefix": page.uri_prefix,
                    "refreshMs": page.refresh_ms,
                    "admin": page.admin,
                }),
            });
            self.listener_nodes.insert(name, id);
        }
    }

    pub(super) fn add_forward_proxy_services(&mut self, config: &ConfigDraft) {
        for (index, service) in config.forward_proxy_services.iter().enumerate() {
            let id = forward_proxy_service_id(&service.name);
            let config_path = format!("/forward_proxy_services/{index}");
            self.nodes.push(TopologyNode {
                id,
                kind: TopologyNodeKind::ForwardProxyService,
                name: service.name.clone(),
                config_path,
                attributes: json!({
                    "enabledVersions": service.enabled_versions,
                    "allowAbsoluteForm": service.allow_absolute_form,
                    "tlsRequired": service.tls_required,
                    "connectEnabled": service.connect.enabled,
                    "connectPortCount": service.connect.allowed_ports.len(),
                    "connectUdpEnabled": service.connect_udp.enabled,
                    "connectUdpPortCount": service.connect_udp.allowed_ports.len(),
                    "auth": match service.auth {
                        Some(oxiroute_config::ForwardProxyAuth::BearerTokenFile { .. }) => "bearer_token_file",
                        Some(oxiroute_config::ForwardProxyAuth::BasicHtpasswdFile { .. }) => "basic_htpasswd_file",
                        Some(oxiroute_config::ForwardProxyAuth::MutualTls { .. }) => "mutual_tls",
                        None => "none",
                    },
                    "accessRuleCount": service.access_policy.as_ref().map_or(0, |policy| policy.rules.len()),
                    "denyPrivate": service.destination_policy.deny_private,
                    "nameserverCount": service.resolver.nameservers.len(),
                    "auditMode": service.audit_mode,
                }),
            });
        }
    }

    pub(super) fn add_tls(&mut self, config: &ConfigDraft) {
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
                    "clientAuth": {
                        "mode": tls_client_auth_mode(profile.policy.client_auth.mode),
                        "caConfigured": profile.policy.client_auth.ca_certificate_path.is_some(),
                        "allowedDnsNameCount": profile.policy.client_auth.allowed_dns_names.len(),
                    },
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

    pub(super) fn add_http_services(&mut self, config: &ConfigDraft) {
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
                        "minLengthBytes": gzip.min_length_bytes,
                        "minHttpVersion": match gzip.min_http_version {
                            oxiroute_config::HttpGzipMinimumVersion::Http10 => "1.0",
                            oxiroute_config::HttpGzipMinimumVersion::Http11 => "1.1",
                        },
                        "disableOnVia": gzip.disable_on_via,
                        "vary": gzip.vary,
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

    pub(super) fn add_l4_services(&mut self, config: &ConfigDraft) {
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

    pub(super) fn add_upstream_pools(&mut self, config: &ConfigDraft) {
        for (pool_index, pool) in config.upstream_pools.iter().enumerate() {
            let id = pool_id(&pool.name);
            let config_path = format!("/upstream_pools/{pool_index}");
            self.nodes.push(TopologyNode {
                id: id.clone(),
                kind: TopologyNodeKind::UpstreamPool,
                name: pool.name.clone(),
                config_path: config_path.clone(),
                attributes: json!({
                    "algorithm": upstream_algorithm(&pool.algorithm),
                    "healthCheck": pool.health_check.as_ref().map(health_check),
                    "tls": pool.tls.as_ref().map(upstream_tls),
                    "httpVersions": {
                        "min": http_version(pool.http_versions.min),
                        "max": http_version(pool.http_versions.max),
                    },
                }),
            });
            self.pool_nodes.insert(pool.name.clone(), id.clone());
            self.pool_paths
                .insert(pool.name.clone(), config_path.clone());

            let mut static_servers = HashSet::with_capacity(pool.servers.len());
            for (server_index, server) in pool.servers.iter().enumerate() {
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
                static_servers.insert(server.name.clone());
            }
            self.static_endpoints
                .insert(pool.name.clone(), static_servers);
        }
    }

    pub(super) fn finish(self) -> TopologySnapshot {
        TopologySnapshot {
            nodes: self.nodes.into_boxed_slice(),
            edges: self.edges.into_boxed_slice(),
            listener_nodes: self.listener_nodes,
            pool_nodes: self.pool_nodes,
            pool_paths: self.pool_paths,
            static_endpoints: self.static_endpoints,
            rendered: OnceLock::new(),
        }
    }
}
