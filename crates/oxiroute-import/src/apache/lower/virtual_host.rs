use crate::apache::{EffectiveVirtualHost, SourceGraph};
use crate::{Diagnostic, DiagnosticStage, E_SEMANTICS_NOT_REPRESENTABLE, ProvenanceRole, Severity};
use oxiroute_config::{
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy,
};

use super::{LoweredVirtualHost, Lowerer, apache_origin};

impl Lowerer {
    pub(super) fn lower_virtual_host(
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
            .map(Some)
            .chain(default_server.then_some(None))
            .collect::<Vec<_>>();
        let preserve_origin = virtual_host.preserve_host_origin.as_ref().map(|origin| {
            self.origin(
                origin,
                if virtual_host.preserve_host_inherited {
                    ProvenanceRole::Inherited
                } else {
                    ProvenanceRole::Value
                },
            )
        });
        for proxy in &virtual_host.proxy_passes {
            let (_pool_key, pool_name, pool_origins) = self.pool_for(proxy)?;
            let proxy_origin = self.origin(
                &proxy.origin,
                if proxy.inherited {
                    ProvenanceRole::Inherited
                } else {
                    ProvenanceRole::Value
                },
            );
            let mut route_sources = vec![vhost_origin.clone(), proxy_origin.clone()];
            route_sources.extend(pool_origins.clone());
            if let Some(preserve_origin) = &preserve_origin {
                route_sources.push(preserve_origin.clone());
            }
            origins.extend(pool_origins);
            for host in &host_selectors {
                let host_selector = host.map(|name| name.selector.clone());
                let mut host_route_sources = route_sources.clone();
                if let Some(name) = host {
                    let role = if name.inherited {
                        ProvenanceRole::Inherited
                    } else {
                        ProvenanceRole::Value
                    };
                    host_route_sources.push(self.origin(&name.origin, role));
                }
                routes.push(HttpRoute {
                    host: host_selector,
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
                        }
                        .into(),
                    },
                });
                route_origins.push(host_route_sources);
            }
        }
        Some(LoweredVirtualHost {
            routes,
            route_origins,
            origins,
        })
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
                        "ordered Apache ProxyPass first-match rules would change under canonical longest-prefix routing",
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
