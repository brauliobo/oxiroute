use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use oxiroute_config::{
    HttpVersionPolicy, Listener, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
};

use super::{Lowerer, Representability};
use crate::canonical::{dns_name, ip_address, unix_socket_path};
use crate::haproxy::{
    BalanceAlgorithm, BindAddress, EffectiveBind, EffectiveSection, EffectiveServer,
    EffectiveValue, OptionState, ProxyMode, ProxySettings,
};

use super::{
    policy::ModeSelection,
    provenance::{CanonicalPath, extend_sources, provenance_sources, section_sources},
};

impl Lowerer<'_> {
    pub(super) fn lower_pool(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) {
        let semantic_settings_blocked = self.block_semantic_settings(settings);
        self.block_forward_for(settings);
        self.block_redispatch(settings);
        self.require_zero_retries(section, settings);
        if settings
            .mode
            .as_ref()
            .is_some_and(|mode| matches!(&mode.value, ProxyMode::Http))
        {
            self.matching_http_timeout(section, settings);
        }
        let Some(name) = self.canonical_name(section, "upstream pool") else {
            return;
        };
        if servers.is_empty() {
            self.block_section(
                section,
                "HAProxy backend has no static servers and cannot form a canonical pool",
            );
        }

        let mut endpoints = Vec::with_capacity(servers.len());
        let mut endpoint_set = HashSet::with_capacity(servers.len());
        let mut decision = Representability::new(!servers.is_empty() && !semantic_settings_blocked);
        for server in servers {
            let options_clear = !self.block_server_options(server);
            decision.require(options_clear);
            let Some(endpoint) = self.lower_server_endpoint(server) else {
                decision.require(false);
                continue;
            };
            if !endpoint_set.insert(endpoint.clone()) {
                self.block_value(
                    &server.address,
                    "duplicate HAProxy server endpoints encode weight that canonical pools cannot preserve",
                );
                decision.require(false);
            }
            endpoints.push(endpoint);
        }
        let algorithm = self.lower_algorithm(
            section,
            settings.balance.as_ref(),
            settings.mode.as_ref(),
            &endpoints,
        );
        decision.require(algorithm.is_some());
        let startup_clear = !self.block_health_startup(settings, servers);
        decision.require(startup_clear);
        if !decision.is_complete() {
            return;
        }
        let algorithm = algorithm.expect("representable pool has an algorithm");

        let pool_index = self.draft.upstream_pools.len();
        self.lowered_pools.insert(section.id);
        self.draft.upstream_pools.push(UpstreamPool {
            name: name.clone(),
            endpoints,
            algorithm,
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
        });
        let pool_path = CanonicalPath::indexed("upstream_pools", pool_index);
        let mut sources = section_sources(section);
        if let Some(balance) = &settings.balance {
            extend_sources(&mut sources, &balance.provenance);
        }
        for server in servers {
            extend_sources(&mut sources, &server.name.provenance);
            extend_sources(&mut sources, &server.address.provenance);
        }
        self.record(pool_path.clone(), sources);
        if let Some(balance) = &settings.balance {
            self.record(
                pool_path.field("algorithm"),
                provenance_sources(&balance.provenance),
            );
        }
        let endpoints_path = pool_path.field("endpoints");
        for (endpoint_index, server) in servers.iter().enumerate() {
            let path = endpoints_path.index(endpoint_index);
            let origins = provenance_sources(&server.address.provenance);
            let endpoint = self.draft.upstream_pools[pool_index].endpoints[endpoint_index].clone();
            self.record(path.field("type"), origins.clone());
            match endpoint {
                UpstreamEndpoint::Socket { .. } => self.record(path.field("address"), origins),
                UpstreamEndpoint::Dns { .. } => {
                    self.record(path.field("host"), origins.clone());
                    self.record(path.field("port"), origins);
                }
                UpstreamEndpoint::Unix { .. } => self.record(path.field("path"), origins),
            }
        }
    }

    fn lower_algorithm(
        &mut self,
        section: &EffectiveSection,
        balance: Option<&EffectiveValue<BalanceAlgorithm>>,
        mode: Option<&EffectiveValue<ProxyMode>>,
        endpoints: &[UpstreamEndpoint],
    ) -> Option<UpstreamAlgorithm> {
        match balance {
            Some(balance) if balance.value == BalanceAlgorithm::RoundRobin => {
                Some(UpstreamAlgorithm::RoundRobin)
            }
            Some(balance)
                if balance.value == BalanceAlgorithm::LeastConnections
                    && mode.is_some_and(|mode| mode.value == ProxyMode::Tcp)
                    && !endpoints.is_empty() =>
            {
                Some(UpstreamAlgorithm::LeastConnections)
            }
            Some(balance) => {
                self.block_value(
                    balance,
                    "HAProxy leastconn is lowerable only for a complete TCP endpoint set with canonical connection-count semantics",
                );
                None
            }
            None => {
                self.block_section(
                    section,
                    "HAProxy backend requires an explicit roundrobin or exactly representable leastconn balance policy for lowering",
                );
                None
            }
        }
    }

    fn lower_server_endpoint(&mut self, server: &EffectiveServer) -> Option<UpstreamEndpoint> {
        match &server.address.value {
            crate::haproxy::ServerAddress::Tcp { host, port } => {
                if let Some(ip) = ip_address(host) {
                    return Some(UpstreamEndpoint::Socket {
                        address: SocketAddr::new(ip, *port),
                    });
                }
                let Some(host) = dns_name(host) else {
                    self.block_value(
                        &server.address,
                        "HAProxy server name is not an exact canonical DNS endpoint",
                    );
                    return None;
                };
                Some(UpstreamEndpoint::Dns { host, port: *port })
            }
            crate::haproxy::ServerAddress::Unix { path } => {
                let Some(path) = unix_socket_path(path) else {
                    self.block_value(
                        &server.address,
                        "HAProxy Unix server path is not an exact canonical absolute socket path",
                    );
                    return None;
                };
                Some(UpstreamEndpoint::Unix { path })
            }
        }
    }

    fn block_server_options(&mut self, server: &EffectiveServer) -> bool {
        for option in &server.unsupported_options {
            self.block_value(
                option,
                "HAProxy server selection, capacity, TLS, or check option has no canonical equivalent",
            );
        }
        !server.unsupported_options.is_empty()
    }

    fn block_health_startup(
        &mut self,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) -> bool {
        let provenance = servers
            .iter()
            .find_map(|server| {
                server
                    .check
                    .as_ref()
                    .map(|value| &value.provenance)
                    .or_else(|| server.interval.as_ref().map(|value| &value.provenance))
                    .or_else(|| server.rise.as_ref().map(|value| &value.provenance))
                    .or_else(|| server.fall.as_ref().map(|value| &value.provenance))
            })
            .or_else(|| {
                settings
                    .http_check
                    .as_ref()
                    .filter(|check| matches!(check.value, OptionState::Enabled(_)))
                    .map(|check| &check.provenance)
            })
            .or_else(|| {
                settings
                    .http_check_expect
                    .as_ref()
                    .map(|expect| &expect.provenance)
            });
        if let Some(provenance) = provenance {
            self.block_provenance(
                provenance,
                "HAProxy checked servers are initially eligible, while canonical checked pools start unavailable",
            );
            true
        } else {
            false
        }
    }

    pub(super) fn lower_listeners(
        &mut self,
        section: &EffectiveSection,
        service_name: &str,
        binds: &[EffectiveBind],
        maxconn: Option<&EffectiveValue<u64>>,
        mode: &ModeSelection,
    ) -> bool {
        if binds.is_empty() {
            self.block_section(
                section,
                "HAProxy proxy has no bind that can form a canonical listener",
            );
            return false;
        }
        if self.effective.global.maxconn.is_some() {
            return false;
        }
        let Some(caps) = self.listener_caps(section, binds, maxconn) else {
            return false;
        };
        if mode.protocol != oxiroute_config::Protocol::Http {
            if let Some(tls) = binds.iter().find_map(|bind| bind.tls.as_ref()) {
                self.block_value(
                    tls,
                    "HAProxy TLS termination on a non-HTTP listener has no canonical representation",
                );
                return false;
            }
        }

        let mut addresses = Vec::with_capacity(binds.len());
        for bind in binds {
            let Some(address) = self.lower_listener_bind(bind) else {
                return false;
            };
            if matches!(address, ListenerBind::Unix { .. }) && bind.tls.is_some() {
                self.block_value(
                    bind.tls.as_ref().expect("checked TLS bind"),
                    "HAProxy TLS termination on a Unix listener has no canonical representation",
                );
                return false;
            }
            addresses.push(address);
        }

        for (index, ((bind, address), (max_connections, cap_sources))) in
            binds.iter().zip(addresses).zip(caps).enumerate()
        {
            let listener_name = if binds.len() == 1 {
                service_name.to_owned()
            } else {
                format!("{service_name}-bind-{}", index + 1)
            };
            let listener_index = self.draft.listeners.len();
            let tls_profile = if let Some(tls) = &bind.tls {
                let Some(profile) = self.lower_bind_tls(tls, mode.protocol, listener_index) else {
                    return false;
                };
                Some(profile)
            } else {
                None
            };
            self.draft.listeners.push(Listener {
                name: listener_name.clone(),
                bind: address,
                protocol: mode.protocol,
                service: Some(service_name.to_owned()),
                tls_profile,
                max_connections,
            });
            let listener_path = CanonicalPath::indexed("listeners", listener_index);
            let mut sources = section_sources(section);
            sources.extend(mode.sources.clone());
            extend_sources(&mut sources, &bind.address.provenance);
            if let Some(tls) = &bind.tls {
                extend_sources(&mut sources, &tls.provenance);
            }
            sources.extend(cap_sources.clone());
            self.record(listener_path.clone(), sources);
            let bind_sources = provenance_sources(&bind.address.provenance);
            let bind_path = listener_path.field("bind");
            self.record(bind_path.field("type"), bind_sources.clone());
            match &self.draft.listeners[listener_index].bind {
                ListenerBind::Socket { .. } => {
                    self.record(bind_path.field("address"), bind_sources);
                }
                ListenerBind::Unix { .. } => {
                    self.record(bind_path.field("path"), bind_sources);
                }
                ListenerBind::Udp { .. } => {
                    self.block_provenance(
                        &bind.address.provenance,
                        "HAProxy stream listeners cannot lower to a canonical UDP listener",
                    );
                    return false;
                }
            }
            self.record(listener_path.field("protocol"), mode.sources.clone());
            if !cap_sources.is_empty() {
                self.record(listener_path.field("max_connections"), cap_sources);
            }
            self.record(listener_path.field("service"), section_sources(section));
        }
        true
    }

    fn lower_listener_bind(&mut self, bind: &EffectiveBind) -> Option<ListenerBind> {
        match &bind.address.value {
            BindAddress::Tcp { host, port } => {
                let ip = if host.is_empty() || host == b"*" {
                    Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                } else {
                    ip_address(host)
                };
                let Some(ip) = ip else {
                    self.block_value(
                        &bind.address,
                        "HAProxy named bind addresses cannot be lowered to canonical socket listeners",
                    );
                    return None;
                };
                Some(ListenerBind::Socket {
                    address: SocketAddr::new(ip, *port),
                })
            }
            BindAddress::Unix { path } => {
                let Some(path) = unix_socket_path(path) else {
                    self.block_value(
                        &bind.address,
                        "HAProxy Unix bind path is not an exact canonical absolute socket path",
                    );
                    return None;
                };
                Some(ListenerBind::Unix { path })
            }
        }
    }
}
