#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    #[allow(clippy::unnecessary_to_owned)]
    pub(super) fn lower_backends(&mut self) {
        for (index, backend) in self.backends.to_vec().into_iter().enumerate() {
            if let Some(info) = self.lower_backend(&backend) {
                self.backend_infos.insert(index, info);
            }
        }
        for (index, director) in self.directors.to_vec().into_iter().enumerate() {
            if let Some(info) = self.lower_director(&director) {
                self.director_infos.insert(index, info);
            }
        }
        let pools = self
            .backend_infos
            .values()
            .map(|info| (info.pool.clone(), info.health.clone()))
            .chain(
                self.director_infos
                    .values()
                    .map(|info| (info.pool.clone(), info.health.clone())),
            )
            .collect::<Vec<_>>();
        for (index, (pool, health)) in pools.iter().enumerate() {
            self.draft.upstream_pools.push(pool.clone());
            let path = format!("/upstream_pools/{index}");
            let origin = self.pool_origin(pool).into_iter().collect::<Vec<_>>();
            self.record(path.clone(), origin.clone());
            for suffix in ["/name", "/servers", "/algorithm", "/connection_reuse"] {
                self.record(format!("{path}{suffix}"), origin.clone());
            }
            if let Some(health) = health {
                for (suffix, origins) in &health.provenance {
                    self.record(format!("{path}/health_check{suffix}"), origins.clone());
                }
            }
            for server_index in 0..pool.servers.len() {
                let server_path = format!("{path}/servers/{server_index}");
                self.record(server_path.clone(), origin.clone());
                self.record(format!("{server_path}/name"), origin.clone());
                self.record(format!("{server_path}/endpoint"), origin.clone());
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_backend(&mut self, backend: &Backend) -> Option<BackendInfo> {
        let name = match String::from_utf8(backend.name.clone()) {
            Ok(name) if !name.is_empty() => name,
            _ => {
                self.block_backend(backend, "backend identity is not valid UTF-8");
                return None;
            }
        };
        if matches!(&backend.kind, BackendKind::None) {
            return None;
        }
        if matches!(&backend.kind, BackendKind::Dynamic) {
            self.block_backend(backend, "dynamic backend declaration is not lowered");
            return None;
        }
        let mut host = None;
        let mut port = None;
        let mut path = None;
        let mut timing = RouteTiming::default();
        let mut max_connections = None;
        let mut explicit_probe = None;
        for property in &backend.properties {
            match property {
                BackendProperty::Host(value) => {
                    if host.replace(value).is_some() {
                        self.block_backend(backend, "backend host is assigned more than once");
                    }
                }
                BackendProperty::Port(value) => {
                    if port.replace(value).is_some() {
                        self.block_backend(backend, "backend port is assigned more than once");
                    }
                }
                BackendProperty::Path(value) => {
                    if path.replace(value).is_some() {
                        self.block_backend(backend, "backend Unix path is assigned more than once");
                    }
                }
                BackendProperty::ConnectTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.connect_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend connect timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::FirstByteTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.read_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend first-byte timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::BetweenBytesTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.write_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend between-bytes timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::MaxConnections(value) => {
                    if let Some(value) = integer(value).filter(|value| *value > 0) {
                        max_connections = Some(value);
                    } else {
                        self.block_backend(
                            backend,
                            "backend max_connections is not a positive integer",
                        );
                    }
                }
                BackendProperty::Probe(reference) => {
                    if explicit_probe.replace(reference.clone()).is_some() {
                        self.block_backend(backend, "backend probe is assigned more than once");
                    }
                }
                BackendProperty::ProxyHeader(_) | BackendProperty::Unsupported { .. } => {
                    self.block_backend(backend, "backend property has no exact canonical mapping");
                }
            }
        }
        let probe_reference = explicit_probe.or_else(|| {
            self.probes
                .iter()
                .position(|probe| probe.name == b"default")
                .map(|declaration| ProbeReference::Named {
                    name: b"default".to_vec(),
                    declaration,
                })
        });
        let health = probe_reference.as_ref().and_then(|reference| {
            match self.lower_probe(reference, &backend.provenance) {
                Ok(health) => Some(health),
                Err(error) => {
                    self.block_with_provenance(
                        LoweringBlocker::SemanticMismatch,
                        E_VCL_SEMANTIC_MISMATCH,
                        error.message,
                        &error.provenance,
                    );
                    None
                }
            }
        });
        if health.is_some() && matches!(backend.kind, BackendKind::Unix { .. }) {
            self.block_backend(
                backend,
                "Varnish probes on Unix backends have no canonical health check",
            );
        }
        let endpoint = match &backend.kind {
            BackendKind::Network {
                host: kind_host,
                port: kind_port,
            } => {
                let host = host.or(kind_host.as_ref());
                let port = port.or(kind_port.as_ref());
                let Some(host) = host.and_then(static_text) else {
                    self.block_backend(backend, "network backend host is not static");
                    return None;
                };
                let port = port.and_then(static_port).unwrap_or(80);
                if let Ok(address) = host.parse::<IpAddr>() {
                    UpstreamEndpoint::Socket {
                        address: SocketAddr::new(address, port),
                    }
                } else if canonical_dns_name(&host).is_ok() {
                    UpstreamEndpoint::Dns { host, port }
                } else {
                    self.block_backend(
                        backend,
                        "network backend host is not a canonical IP or DNS name",
                    );
                    return None;
                }
            }
            BackendKind::Unix { path: kind_path } => {
                let path = path.or(Some(kind_path));
                let Some(path) = path
                    .and_then(static_text)
                    .and_then(|value| canonical_unix_path(&value))
                else {
                    self.block_backend(
                        backend,
                        "Unix backend path is not a safe absolute socket path",
                    );
                    return None;
                };
                UpstreamEndpoint::Unix { path }
            }
            BackendKind::None | BackendKind::Dynamic => return None,
        };
        let server = UpstreamServer {
            name: name.clone(),
            endpoint,
            max_connections,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        };
        Some(BackendInfo {
            pool: UpstreamPool {
                name,
                servers: vec![server],
                endpoints: Vec::new(),
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: health.as_ref().map(|health| health.check.clone()),
                passive_health: None,
                tls: None,
                http_versions: oxiroute_config::HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
                registration: None,
                canary: None,
                resilience: oxiroute_config::PoolResilience::default(),
                queue: None,
            },
            timing,
            health,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn lower_probe(
        &self,
        reference: &ProbeReference,
        fallback: &Provenance,
    ) -> Result<HealthProbeInfo, ProbeError> {
        let (properties, provenance) = match reference {
            ProbeReference::Named { declaration, .. } => {
                let Some(probe) = self.probes.get(*declaration) else {
                    return Err(ProbeError {
                        provenance: fallback.clone(),
                        message: "Varnish probe reference has no declaration",
                    });
                };
                (probe.properties.clone(), probe.provenance.clone())
            }
            ProbeReference::Inline(properties) => (properties.clone(), fallback.clone()),
            ProbeReference::Unresolved { .. } | ProbeReference::Dynamic(_) => {
                return Err(ProbeError {
                    provenance: fallback.clone(),
                    message: "Varnish probe reference is not static",
                });
            }
        };
        let mut path_expression = None;
        let mut expected_status_expression = None;
        let mut timeout_expression = None;
        let mut interval_expression = None;
        let mut window_expression = None;
        let mut threshold_expression = None;
        let mut initial_expression = None;
        for property in &properties {
            let (slot, value) = match property {
                ProbeProperty::Url(value) => (&mut path_expression, value),
                ProbeProperty::Request(value) => {
                    return Err(ProbeError {
                        provenance: probe_provenance(value, &provenance),
                        message: "Varnish probe request syntax is outside the exact HTTP health subset",
                    });
                }
                ProbeProperty::ExpectedResponse(value) => (&mut expected_status_expression, value),
                ProbeProperty::Timeout(value) => (&mut timeout_expression, value),
                ProbeProperty::Interval(value) => (&mut interval_expression, value),
                ProbeProperty::Window(value) => (&mut window_expression, value),
                ProbeProperty::Threshold(value) => (&mut threshold_expression, value),
                ProbeProperty::Initial(value) => (&mut initial_expression, value),
                ProbeProperty::Unsupported { value, .. } => {
                    return Err(ProbeError {
                        provenance: probe_provenance(value, &provenance),
                        message: "Varnish probe property is outside the exact HTTP health subset",
                    });
                }
            };
            if slot.replace(value).is_some() {
                return Err(ProbeError {
                    provenance: probe_provenance(value, &provenance),
                    message: "Varnish probe property is assigned more than once",
                });
            }
        }
        let Some(interval_value) = interval_expression.and_then(duration_ms) else {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probes require one finite explicit interval",
            });
        };
        let Some(timeout_value) = timeout_expression.and_then(duration_ms) else {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probes require one finite explicit timeout",
            });
        };
        if !(1_000..=86_400_000).contains(&interval_value)
            || timeout_value == 0
            || timeout_value > 30_000
            || timeout_value > interval_value
        {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probe timing is outside the canonical health bounds",
            });
        }
        let Some(window_value) = window_expression.and_then(integer) else {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probes require one explicit window",
            });
        };
        let Some(threshold_value) = threshold_expression.and_then(integer) else {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probes require one explicit threshold",
            });
        };
        if window_value != 1 || threshold_value != 1 {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probe window and threshold must both be one",
            });
        }
        let initial_value =
            initial_expression
                .map_or(Some(0), integer)
                .ok_or_else(|| ProbeError {
                    provenance: initial_expression.map_or_else(
                        || provenance.clone(),
                        |value| probe_provenance(value, &provenance),
                    ),
                    message: "Varnish probe initial must be an integer",
                })?;
        if initial_value > 1 {
            return Err(ProbeError {
                provenance: provenance.clone(),
                message: "Varnish probe initial must be zero or one when window and threshold are one",
            });
        }
        let path_value = match path_expression {
            Some(value) => static_string(value).ok_or_else(|| ProbeError {
                provenance: probe_provenance(value, &provenance),
                message: "Varnish probe URL must be one static string",
            })?,
            None => "/".into(),
        };
        let parsed_path = path_value.parse::<http::uri::PathAndQuery>().ok();
        if path_value.len() > 2_048
            || !path_value.starts_with('/')
            || !oxiroute_config::is_unambiguous_http_path(&path_value)
            || parsed_path
                .as_ref()
                .is_none_or(|parsed| parsed.query().is_some() || parsed.path() != path_value)
        {
            return Err(ProbeError {
                provenance: path_expression.map_or_else(
                    || provenance.clone(),
                    |value| probe_provenance(value, &provenance),
                ),
                message: "Varnish probe URL must be an unambiguous absolute path without a query",
            });
        }
        let expected_status_value = expected_status_expression
            .map_or(Some(200), integer)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| ProbeError {
                provenance: expected_status_expression.map_or_else(
                    || provenance.clone(),
                    |value| probe_provenance(value, &provenance),
                ),
                message: "Varnish probe expected_response must be an integer",
            })?;
        if !(200..=599).contains(&expected_status_value) {
            return Err(ProbeError {
                provenance: expected_status_expression.map_or_else(
                    || provenance.clone(),
                    |value| probe_provenance(value, &provenance),
                ),
                message: "Varnish probe expected_response must be between 200 and 599",
            });
        }
        let startup = if initial_value == 1 {
            HealthStartup::Healthy
        } else {
            HealthStartup::Unhealthy
        };
        let check = HealthCheck {
            kind: HealthCheckType::Http,
            interval_ms: interval_value,
            timeout_ms: timeout_value,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup,
            fast_interval_ms: None,
            down_interval_ms: None,
            host: None,
            path: Some(path_value),
            expected_status: (expected_status_value != 200).then_some(expected_status_value),
            http_version: None,
        };
        let default_origin = vec![provenance.clone()];
        let origin = |value: Option<&Expression>| {
            value.map_or_else(
                || default_origin.clone(),
                |value| vec![probe_provenance(value, &provenance)],
            )
        };
        let all_origins = properties
            .iter()
            .map(|property| match property {
                ProbeProperty::Url(value)
                | ProbeProperty::Request(value)
                | ProbeProperty::ExpectedResponse(value)
                | ProbeProperty::Timeout(value)
                | ProbeProperty::Interval(value)
                | ProbeProperty::Window(value)
                | ProbeProperty::Threshold(value)
                | ProbeProperty::Initial(value)
                | ProbeProperty::Unsupported { value, .. } => probe_provenance(value, &provenance),
            })
            .collect::<Vec<_>>();
        Ok(HealthProbeInfo {
            check,
            provenance: vec![
                (String::new(), all_origins),
                ("/type".into(), default_origin.clone()),
                ("/interval_ms".into(), origin(interval_expression)),
                ("/timeout_ms".into(), origin(timeout_expression)),
                ("/healthy_threshold".into(), origin(threshold_expression)),
                ("/unhealthy_threshold".into(), origin(threshold_expression)),
                ("/startup".into(), origin(initial_expression)),
                ("/path".into(), origin(path_expression)),
                (
                    "/expected_status".into(),
                    origin(expected_status_expression),
                ),
                ("/http_version".into(), default_origin),
            ],
        })
    }

    fn lower_director(&mut self, director: &Director) -> Option<DirectorInfo> {
        let name = match String::from_utf8(director.name.clone()) {
            Ok(name) if !name.is_empty() => name,
            _ => {
                self.block_director(director, "director identity is not valid UTF-8");
                return None;
            }
        };
        let algorithm = match &director.policy {
            DirectorKind::RoundRobin => UpstreamAlgorithm::RoundRobin,
            DirectorKind::Fallback => UpstreamAlgorithm::First,
            DirectorKind::Random | DirectorKind::Hash | DirectorKind::Unknown(_) => {
                self.block_director(director, "director policy has no exact canonical algorithm");
                return None;
            }
        };
        if !director.unsupported_properties.is_empty() || director.members.is_empty() {
            self.block_director(director, "director properties or membership are not exact");
            return None;
        }
        let mut servers = Vec::new();
        let mut timing = None;
        let mut health = None;
        for member in &director.members {
            let BackendReference::Backend { declaration, .. } = member else {
                self.block_director(
                    director,
                    "director member is not a static backend reference",
                );
                return None;
            };
            let Some(info) = self.backend_infos.get(declaration).cloned() else {
                self.block_director(director, "director member backend could not be lowered");
                return None;
            };
            if servers
                .iter()
                .any(|server: &UpstreamServer| server.endpoint == info.pool.servers[0].endpoint)
            {
                self.block_director(director, "director repeats one backend endpoint");
                return None;
            }
            if timing.is_some_and(|candidate| !same_timing(candidate, info.timing)) {
                self.block_director(director, "director members use different timeout semantics");
                return None;
            }
            timing = Some(info.timing);
            match health.as_mut() {
                None => health = Some(info.health.clone()),
                Some(existing) => match (existing.as_mut(), info.health.as_ref()) {
                    (None, None) => {}
                    (Some(existing), Some(candidate)) if existing.check == candidate.check => {
                        for (path, origins) in &candidate.provenance {
                            if let Some((_, existing_origins)) = existing
                                .provenance
                                .iter_mut()
                                .find(|(existing_path, _)| existing_path == path)
                            {
                                for origin in origins {
                                    if !existing_origins.contains(origin) {
                                        existing_origins.push(origin.clone());
                                    }
                                }
                            } else {
                                existing.provenance.push((path.clone(), origins.clone()));
                            }
                        }
                    }
                    _ => {
                        self.block_director(
                            director,
                            "director members use different health-check semantics",
                        );
                        return None;
                    }
                },
            }
            servers.push(info.pool.servers[0].clone());
        }
        Some(DirectorInfo {
            pool: UpstreamPool {
                name,
                servers,
                endpoints: Vec::new(),
                algorithm,
                health_check: health
                    .as_ref()
                    .and_then(|health| health.as_ref())
                    .map(|health| health.check.clone()),
                passive_health: None,
                tls: None,
                http_versions: oxiroute_config::HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
                registration: None,
                canary: None,
                resilience: oxiroute_config::PoolResilience::default(),
                queue: None,
            },
            timing: timing.unwrap_or_default(),
            health: health.flatten(),
        })
    }
}
