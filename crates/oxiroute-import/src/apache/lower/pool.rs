use crate::apache::{ApacheProvenance, EffectiveProxyPass, ProxyScheme, ProxyTarget};
use crate::{E_SEMANTICS_NOT_REPRESENTABLE, ProvenanceRole};
use oxiroute_config::{
    DnsResolutionPolicy, HttpVersionPolicy, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamPool, UpstreamServer, UpstreamTls,
};

use super::{LoweredPool, Lowerer};

impl Lowerer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn pool_for(
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
        let proxy_origin = self.origin(
            &proxy.origin,
            if proxy.inherited {
                ProvenanceRole::Inherited
            } else {
                ProvenanceRole::Reference
            },
        );
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
                        server_name: Some(host.clone()),
                        verify: oxiroute_config::UpstreamTlsVerify::Full,
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
                    passive_health: None,
                    tls,
                    http_versions: HttpVersionPolicy::default(),
                    queue_timeout_ms: None,
                    connect_timeout_ms: None,
                    server_timeout_ms: None,
                    connection_reuse: UpstreamConnectionReuse::default(),
                    registration: None,
                    canary: None,
                    resilience: oxiroute_config::PoolResilience::default(),
                    queue: None,
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
                    server_name: Some(first_host.clone()),
                    verify: oxiroute_config::UpstreamTlsVerify::Full,
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
                    passive_health: None,
                    tls,
                    http_versions: HttpVersionPolicy::default(),
                    queue_timeout_ms: None,
                    connect_timeout_ms: None,
                    server_timeout_ms: None,
                    connection_reuse: UpstreamConnectionReuse::default(),
                    registration: None,
                    canary: None,
                    resilience: oxiroute_config::PoolResilience::default(),
                    queue: None,
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
}
