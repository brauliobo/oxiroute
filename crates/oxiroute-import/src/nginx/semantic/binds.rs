#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "socket identity, overlap, protocol, default, and name resolution are one pass"
    )]
    pub(super) fn resolve_binds(&mut self, servers: &[EffectiveServer]) -> Vec<EffectiveBind> {
        let mut binds: Vec<EffectiveBind> = Vec::new();
        let mut endpoint_origins = Vec::new();
        let mut endpoint_ssl = HashMap::new();
        for server in servers {
            let mut server_endpoints = HashSet::new();
            for listen in &server.listens {
                let Some(endpoint) = &listen.endpoint else {
                    continue;
                };
                if !server_endpoints.insert(endpoint.clone()) {
                    if !listen.implicit {
                        let first = server
                            .listens
                            .iter()
                            .find(|candidate| {
                                candidate.origin.occurrence != listen.origin.occurrence
                                    && candidate.endpoint.as_ref() == Some(endpoint)
                            })
                            .map_or(server.origin.occurrence, |candidate| {
                                candidate.origin.occurrence
                            });
                        self.warn_related(
                            listen.origin.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "paired nginx wildcard listen is represented by one canonical listener",
                            first,
                        );
                    }
                    continue;
                }

                if !endpoint_origins
                    .iter()
                    .any(|(candidate, _)| candidate == endpoint)
                {
                    endpoint_origins.push((endpoint.clone(), listen.origin.occurrence));
                }
                let ssl = listen_ssl(listen);
                if let Some((first_ssl, first)) = endpoint_ssl.get(endpoint).copied() {
                    if first_ssl != ssl {
                        self.warn_related(
                            listen.origin.occurrence,
                            E_INVALID_VALUE,
                            "nginx listen protocol options are reconciled across one effective socket",
                            first,
                        );
                    }
                } else {
                    endpoint_ssl.insert(endpoint.clone(), (ssl, listen.origin.occurrence));
                }

                let bind_index = binds
                    .iter()
                    .position(|bind| bind.endpoint == *endpoint)
                    .unwrap_or_else(|| {
                        binds.push(EffectiveBind {
                            endpoint: endpoint.clone(),
                            servers: Vec::new(),
                            default_server: server.origin.occurrence,
                            default_selection: DefaultServerSelection::First,
                            names: Vec::new(),
                            paired_endpoint: None,
                        });
                        binds.len() - 1
                    });
                let bind = &mut binds[bind_index];
                bind.servers.push(server.origin.occurrence);
                if listen.default_server {
                    match bind.default_selection {
                        DefaultServerSelection::First => {
                            bind.default_server = server.origin.occurrence;
                            bind.default_selection = DefaultServerSelection::Explicit {
                                listen: listen.origin.occurrence,
                            };
                        }
                        DefaultServerSelection::Explicit { listen: first } => {
                            self.block_related(
                                listen.origin.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate explicit default server for nginx listen identity",
                                first,
                            );
                        }
                    }
                }
            }
        }

        for (index, (first_endpoint, first_origin)) in endpoint_origins.iter().enumerate() {
            for (second_endpoint, second_origin) in &endpoint_origins[index + 1..] {
                if listen_endpoints_overlap(first_endpoint, second_endpoint) {
                    self.block_related(
                        *second_origin,
                        E_INVALID_VALUE,
                        "conflicting listen sockets overlap in canonical listener semantics",
                        *first_origin,
                    );
                    self.block_related(
                        *first_origin,
                        E_INVALID_VALUE,
                        "conflicting listen sockets overlap in canonical listener semantics",
                        *second_origin,
                    );
                }
            }
        }

        self.pair_dual_stack(servers, &mut binds);
        self.resolve_bind_names(servers, &mut binds);
        binds
    }

    /// nginx binds `listen P` (IPv4) and `listen [::]:P` (IPv6 only) as two sockets, each routing to the virtual
    /// servers that listen on it. A pair that serves identical virtual servers the same way is one dual-stack `[::]`
    /// canonical listener; any other pair stays two listeners, the IPv6 one `ipv6_only`.
    fn pair_dual_stack(&mut self, servers: &[EffectiveServer], binds: &mut Vec<EffectiveBind>) {
        let ipv6_endpoints = binds
            .iter()
            .filter(|bind| ipv4_wildcard_pair(&bind.endpoint).is_some())
            .map(|bind| bind.endpoint.clone())
            .collect::<Vec<_>>();
        for ipv6_endpoint in ipv6_endpoints {
            let ipv4_endpoint = ipv4_wildcard_pair(&ipv6_endpoint).expect("selected IPv6 wildcard");
            let ipv6_index = Self::bind_index(binds, &ipv6_endpoint);
            let Some(ipv4_index) = binds.iter().position(|bind| bind.endpoint == ipv4_endpoint)
            else {
                continue;
            };
            if !Self::dual_stack_equivalent(servers, &binds[ipv6_index], &binds[ipv4_index]) {
                continue;
            }
            let first = binds[ipv6_index].servers[0];
            let (ipv6_listen, ipv4_listen) = (
                Self::server_listen(servers, first, &ipv6_endpoint),
                Self::server_listen(servers, first, &ipv4_endpoint),
            );
            self.warn_related(
                ipv6_listen.origin.occurrence,
                W_DUAL_STACK_LISTENER,
                "listen and listen [::] on one port are represented by one dual-stack [::] listener",
                ipv4_listen.origin.occurrence,
            );
            let mut merged = binds[ipv6_index].clone();
            merged.paired_endpoint = Some(ipv4_endpoint);
            let (keep, discard) = (ipv6_index.min(ipv4_index), ipv6_index.max(ipv4_index));
            binds[keep] = merged;
            binds.remove(discard);
        }
    }

    fn bind_index(binds: &[EffectiveBind], endpoint: &ListenEndpoint) -> usize {
        binds
            .iter()
            .position(|bind| bind.endpoint == *endpoint)
            .expect("endpoint belongs to a resolved bind")
    }

    fn server_listen<'a>(
        servers: &'a [EffectiveServer],
        server: OccurrenceId,
        endpoint: &ListenEndpoint,
    ) -> &'a EffectiveListen {
        servers
            .iter()
            .find(|candidate| candidate.origin.occurrence == server)
            .and_then(|server| {
                server
                    .listens
                    .iter()
                    .find(|listen| listen.endpoint.as_ref() == Some(endpoint))
            })
            .expect("bind retains each contributing listen")
    }

    fn dual_stack_equivalent(
        servers: &[EffectiveServer],
        ipv6: &EffectiveBind,
        ipv4: &EffectiveBind,
    ) -> bool {
        ipv6.servers.iter().collect::<HashSet<_>>() == ipv4.servers.iter().collect::<HashSet<_>>()
            && ipv6.default_server == ipv4.default_server
            && ipv6.servers.iter().all(|server| {
                listen_ssl(Self::server_listen(servers, *server, &ipv6.endpoint))
                    == listen_ssl(Self::server_listen(servers, *server, &ipv4.endpoint))
            })
    }

    fn resolve_bind_names(&mut self, servers: &[EffectiveServer], binds: &mut [EffectiveBind]) {
        for bind in binds {
            let mut claimed_names = HashMap::new();
            for server_id in &bind.servers {
                let server = servers
                    .iter()
                    .find(|server| server.origin.occurrence == *server_id)
                    .expect("bind server came from the effective server list");
                if server.server_names.is_empty() {
                    let identity = (ServerNameKind::Exact, Vec::new());
                    if let Some((_, first)) = claimed_names.get(&identity).copied() {
                        self.warn_related(
                            *server_id,
                            E_DUPLICATE_IDENTITY,
                            "duplicate empty server name is ignored; nginx keeps the first-loaded virtual server",
                            first,
                        );
                    } else {
                        claimed_names.insert(identity, (*server_id, *server_id));
                    }
                    continue;
                }
                for name in &server.server_names {
                    if !is_supported_server_name(name.kind) {
                        continue;
                    }
                    let mut accepted = Vec::new();
                    let mut conflict = None;
                    for claim in server_name_claims(name) {
                        let identity = (claim.kind, claim.normalized.clone());
                        if let Some((first_server, first_origin)) =
                            claimed_names.get(&identity).copied()
                        {
                            if first_server != *server_id && conflict.is_none() {
                                conflict = Some(first_origin);
                            }
                        } else {
                            claimed_names.insert(identity, (*server_id, name.origin.occurrence));
                            accepted.push(claim);
                        }
                    }
                    if let Some(first_origin) = conflict {
                        self.warn_related(
                            name.origin.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "duplicate virtual server name claim is ignored; nginx keeps the first-loaded virtual server",
                            first_origin,
                        );
                    }
                    if name.kind == ServerNameKind::LeadingWildcardAndExact && accepted.len() == 2 {
                        accepted.clear();
                        accepted.push(name.clone());
                    }
                    bind.names
                        .extend(accepted.into_iter().map(|name| BoundServerName {
                            server: *server_id,
                            name,
                        }));
                }
            }
        }
    }
}
