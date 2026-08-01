use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use oxiroute_config::{
    DnsResolutionPolicy, DownstreamTimeoutPolicy, HealthCheck, HealthCheckType, HealthStartup,
    HttpVersionPolicy, Listener, ListenerBind, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamEndpoint, UpstreamPool, UpstreamServer,
};

use super::{Lowerer, Representability};
use crate::canonical::{dns_name, duration_milliseconds, ip_address, unix_socket_path};
use crate::haproxy::{
    BalanceAlgorithm, BindAddress, EffectiveBind, EffectiveSection, EffectiveServer,
    EffectiveValue, OptionState, ProxyMode, ProxySettings,
};

use super::{
    policy::ModeSelection,
    provenance::{CanonicalPath, extend_sources, provenance_sources, section_sources},
};

impl Lowerer<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_pool(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) {
        let semantic_settings_blocked = self.block_semantic_settings(settings);
        let Some(name) = self.canonical_name(section, "upstream pool") else {
            return;
        };
        if servers.is_empty() {
            self.block_section(
                section,
                "HAProxy backend has no static servers and cannot form a canonical pool",
            );
        }

        let mut lowered_servers = Vec::with_capacity(servers.len());
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
            let Some(server_name) = std::str::from_utf8(&server.name.value)
                .ok()
                .filter(|name| !name.trim().is_empty() && name.trim() == *name)
            else {
                self.block_value(
                    &server.name,
                    "HAProxy server name is not a canonical UTF-8 identity",
                );
                decision.require(false);
                continue;
            };
            let dns_resolution = if matches!(endpoint, UpstreamEndpoint::Dns { .. }) {
                DnsResolutionPolicy::Startup
            } else {
                DnsResolutionPolicy::OnConnect
            };
            lowered_servers.push(UpstreamServer {
                name: server_name.into(),
                endpoint,
                max_connections: server
                    .max_connections
                    .as_ref()
                    .and_then(|value| (value.value != 0).then_some(value.value)),
                dns_resolution,
            });
        }
        let algorithm = self.lower_algorithm(
            section,
            settings.balance.as_ref(),
            settings.mode.as_ref(),
            &lowered_servers
                .iter()
                .map(|server| server.endpoint.clone())
                .collect::<Vec<_>>(),
        );
        decision.require(algorithm.is_some());
        let health_check = self.lower_health_check(section, settings, servers);
        decision.require(health_check.is_some());
        let queue_timeout_ms = settings
            .timeouts
            .queue
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy queue timeout"));
        let connect_timeout_ms = settings
            .timeouts
            .connect
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy connect timeout"));
        let server_timeout_ms = settings
            .timeouts
            .server
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy server timeout"));
        decision.require(settings.timeouts.queue.is_none() || queue_timeout_ms.is_some());
        decision.require(settings.timeouts.connect.is_none() || connect_timeout_ms.is_some());
        decision.require(settings.timeouts.server.is_none() || server_timeout_ms.is_some());
        let request_lifetime_sensitive = algorithm
            .as_ref()
            .is_some_and(|algorithm| *algorithm == UpstreamAlgorithm::First)
            || lowered_servers
                .iter()
                .any(|server| server.max_connections.is_some());
        let source_closes_after_request = settings
            .http_server_close
            .as_ref()
            .is_some_and(|value| value.value);
        let overlay_closes_after_request = if request_lifetime_sensitive
            && settings
                .mode
                .as_ref()
                .is_some_and(|mode| mode.value == ProxyMode::Http)
            && !source_closes_after_request
        {
            let matching = self
                .options
                .one_request_per_connection
                .iter()
                .enumerate()
                .filter(|(_, overlay)| {
                    section
                        .name
                        .as_deref()
                        .is_some_and(|name| name == overlay.backend.as_bytes())
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matching.len() == 1 {
                self.used_connection_lifecycle_overlays.insert(matching[0]);
                true
            } else {
                false
            }
        } else {
            false
        };
        let closes_after_request = source_closes_after_request || overlay_closes_after_request;
        if settings
            .mode
            .as_ref()
            .is_some_and(|mode| mode.value == ProxyMode::Http)
            && request_lifetime_sensitive
            && !closes_after_request
        {
            self.block_section(
                section,
                "HAProxy server maxconn/first requires option http-server-close or an explicit audited one-request-per-connection overlay",
            );
            return;
        }
        if !decision.is_complete() {
            return;
        }
        let algorithm = algorithm.expect("representable pool has an algorithm");

        let pool_index = self.draft.upstream_pools.len();
        self.lowered_pools.insert(section.id);
        self.draft.upstream_pools.push(UpstreamPool {
            name: name.clone(),
            servers: lowered_servers,
            endpoints: Vec::new(),
            algorithm,
            health_check: health_check.flatten(),
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms,
            connect_timeout_ms,
            server_timeout_ms,
            connection_reuse: if closes_after_request {
                UpstreamConnectionReuse::Never
            } else {
                UpstreamConnectionReuse::Safe
            },
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
        if let Some(queue_timeout) = &settings.timeouts.queue {
            self.record(
                pool_path.field("queue_timeout_ms"),
                provenance_sources(&queue_timeout.provenance),
            );
        }
        if let Some(balance) = &settings.balance {
            self.record(
                pool_path.field("algorithm"),
                provenance_sources(&balance.provenance),
            );
        }
        let servers_path = pool_path.field("servers");
        for (endpoint_index, server) in servers.iter().enumerate() {
            let path = servers_path.index(endpoint_index);
            let origins = provenance_sources(&server.address.provenance);
            let endpoint = self.draft.upstream_pools[pool_index].servers[endpoint_index]
                .endpoint
                .clone();
            self.record(
                path.field("name"),
                provenance_sources(&server.name.provenance),
            );
            let path = path.field("endpoint");
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
        _mode: Option<&EffectiveValue<ProxyMode>>,
        endpoints: &[UpstreamEndpoint],
    ) -> Option<UpstreamAlgorithm> {
        match balance {
            Some(balance) if balance.value == BalanceAlgorithm::RoundRobin => {
                Some(UpstreamAlgorithm::RoundRobin)
            }
            Some(balance)
                if balance.value == BalanceAlgorithm::LeastConnections && !endpoints.is_empty() =>
            {
                Some(UpstreamAlgorithm::LeastConnections)
            }
            Some(balance) if balance.value == BalanceAlgorithm::First && !endpoints.is_empty() => {
                Some(UpstreamAlgorithm::First)
            }
            Some(balance) => {
                self.block_value(
                    balance,
                    "HAProxy balance policy is not represented by the canonical upstream algorithms",
                );
                None
            }
            None => {
                self.block_section(
                    section,
                    "HAProxy backend requires an explicit representable balance policy for lowering",
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

    #[expect(
        clippy::too_many_lines,
        clippy::option_option,
        reason = "one health transaction distinguishes invalid policy from a disabled check"
    )]
    fn lower_health_check(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) -> Option<Option<HealthCheck>> {
        let checked = servers
            .iter()
            .map(|server| server.check.as_ref().is_some_and(|check| check.value))
            .collect::<HashSet<_>>();
        if checked.len() > 1 {
            self.block_section(
                section,
                "canonical pool health checks cannot target only part of a HAProxy backend",
            );
            return None;
        }
        if checked == HashSet::from([false]) {
            return Some(None);
        }
        for server in servers {
            for (value, description) in [
                (server.interval.as_ref(), "HAProxy health interval"),
                (
                    server.fast_interval.as_ref(),
                    "HAProxy fast health interval",
                ),
                (
                    server.down_interval.as_ref(),
                    "HAProxy down health interval",
                ),
            ] {
                let Some(value) = value else {
                    continue;
                };
                if duration_milliseconds(value.value).is_none() {
                    self.duration_ms(value, description);
                    return None;
                }
            }
        }
        let Some(interval_ms) = Self::common_server_u64(servers, |server| {
            server
                .interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
                .or(Some(2_000))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health intervals",
            );
            return None;
        };
        let Ok(fast_interval_ms) = Self::common_server_optional_u64(servers, |server| {
            server
                .fast_interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective fast health intervals",
            );
            return None;
        };
        let Ok(down_interval_ms) = Self::common_server_optional_u64(servers, |server| {
            server
                .down_interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective down health intervals",
            );
            return None;
        };
        let Some(healthy_threshold) = Self::common_server_u16(servers, |server| {
            server
                .rise
                .as_ref()
                .and_then(|value| u16::try_from(value.value).ok())
                .or(Some(2))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health rise thresholds",
            );
            return None;
        };
        let Some(unhealthy_threshold) = Self::common_server_u16(servers, |server| {
            server
                .fall
                .as_ref()
                .and_then(|value| u16::try_from(value.value).ok())
                .or(Some(3))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health fall thresholds",
            );
            return None;
        };
        let timeout_ms = interval_ms.max(1);
        let (kind, path, host, http_version) = match settings.http_check.as_ref() {
            Some(check_value) => match &check_value.value {
                OptionState::Enabled(check) => {
                    if check.method != b"GET" {
                        self.block_value(
                            check_value,
                            "HAProxy HTTP health check method is not representable by the canonical GET health check",
                        );
                        return None;
                    }
                    let (Ok(path), Ok(host)) = (
                        std::str::from_utf8(&check.uri),
                        check.host.as_deref().map(std::str::from_utf8).transpose(),
                    ) else {
                        self.block_value(
                            check_value,
                            "HAProxy HTTP check path or host is not UTF-8",
                        );
                        return None;
                    };
                    let version = match check.version.as_slice() {
                        b"HTTP/1.0" => Some(oxiroute_config::HealthHttpVersion::Http10),
                        b"HTTP/1.1" => Some(oxiroute_config::HealthHttpVersion::Http11),
                        _ => None,
                    };
                    (
                        HealthCheckType::Http,
                        Some(path.into()),
                        host.map(str::to_owned),
                        version,
                    )
                }
                OptionState::Disabled => (HealthCheckType::Tcp, None, None, None),
            },
            None => (HealthCheckType::Tcp, None, None, None),
        };
        let expected_status = match (kind, settings.http_check_expect.as_ref()) {
            (HealthCheckType::Http, Some(expect)) => match expect.value.as_slice() {
                [range] if range.start == range.end => Some(range.start),
                _ => {
                    self.block_value(
                        expect,
                        "HAProxy HTTP health status ranges cannot lower to one exact canonical status",
                    );
                    return None;
                }
            },
            (HealthCheckType::Http, None) => {
                self.block_section(
                    section,
                    "HAProxy HTTP health checks require an exact http-check expect status before canonical lowering",
                );
                return None;
            }
            (HealthCheckType::Tcp, _) => None,
        };
        Some(Some(HealthCheck {
            kind,
            interval_ms,
            timeout_ms,
            healthy_threshold,
            unhealthy_threshold,
            startup: HealthStartup::Healthy,
            fast_interval_ms,
            down_interval_ms,
            host,
            path,
            expected_status,
            http_version,
        }))
    }

    fn duration_value_ms(duration: std::time::Duration) -> Option<u64> {
        duration_milliseconds(duration)
    }

    fn common_server_u64(
        servers: &[EffectiveServer],
        value: impl Fn(&EffectiveServer) -> Option<u64>,
    ) -> Option<u64> {
        let values = servers.iter().filter_map(value).collect::<HashSet<_>>();
        (values.len() == 1).then(|| *values.iter().next().expect("one value"))
    }

    fn common_server_u16(
        servers: &[EffectiveServer],
        value: impl Fn(&EffectiveServer) -> Option<u16>,
    ) -> Option<u16> {
        let values = servers.iter().filter_map(value).collect::<HashSet<_>>();
        (values.len() == 1).then(|| *values.iter().next().expect("one value"))
    }

    fn common_server_optional_u64(
        servers: &[EffectiveServer],
        value: impl Fn(&EffectiveServer) -> Option<u64>,
    ) -> Result<Option<u64>, ()> {
        let values = servers.iter().map(value).collect::<HashSet<_>>();
        (values.len() == 1)
            .then(|| *values.iter().next().expect("one value"))
            .ok_or(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_listeners(
        &mut self,
        section: &EffectiveSection,
        service_name: &str,
        binds: &[EffectiveBind],
        settings: &ProxySettings,
        mode: &ModeSelection,
    ) -> bool {
        if binds.is_empty() {
            self.block_section(
                section,
                "HAProxy proxy has no bind that can form a canonical listener",
            );
            return false;
        }
        let Some(caps) = self.listener_caps(section, binds, settings.maxconn.as_ref()) else {
            return false;
        };
        let Some(downstream_timeouts) = self.lower_downstream_timeouts(settings) else {
            return false;
        };
        let DownstreamTimeoutPolicy {
            client_timeout_ms,
            request_timeout_ms,
            keepalive_timeout_ms,
        } = downstream_timeouts;
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
                downstream_timeouts: DownstreamTimeoutPolicy {
                    client_timeout_ms,
                    request_timeout_ms,
                    keepalive_timeout_ms,
                },
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
                    if let Some(mode) = &bind.mode {
                        self.record(
                            bind_path.field("mode"),
                            provenance_sources(&mode.provenance),
                        );
                    }
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

    pub(super) fn lower_downstream_timeouts(
        &mut self,
        settings: &ProxySettings,
    ) -> Option<DownstreamTimeoutPolicy> {
        let client_timeout_ms = settings
            .timeouts
            .client
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy client timeout"));
        let request_timeout_ms = settings
            .timeouts
            .http_request
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy HTTP request timeout"));
        let keepalive_timeout_ms = settings
            .timeouts
            .http_keep_alive
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy HTTP keepalive timeout"));
        if (settings.timeouts.client.is_some() && client_timeout_ms.is_none())
            || (settings.timeouts.http_request.is_some() && request_timeout_ms.is_none())
            || (settings.timeouts.http_keep_alive.is_some() && keepalive_timeout_ms.is_none())
        {
            return None;
        }
        Some(DownstreamTimeoutPolicy {
            client_timeout_ms,
            request_timeout_ms,
            keepalive_timeout_ms,
        })
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
                Some(ListenerBind::Unix {
                    path,
                    mode: bind.mode.as_ref().map(|mode| mode.value),
                })
            }
        }
    }
}
