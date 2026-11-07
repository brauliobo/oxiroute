#[allow(clippy::wildcard_imports)]
use super::*;

impl ForwardHttp1ServicePlan {
    pub(super) async fn authorize_request<B>(
        &self,
        request: &Request<B>,
        protocol: Protocol,
        client_addr: Option<SocketAddr>,
    ) -> Result<AuthorizedRequest, RequestFailure> {
        let result = self
            .authorize_request_inner(request, protocol, client_addr)
            .await;
        let access_result = result.as_ref().map_or_else(
            |error| error.access_result(),
            |_| ForwardAccessResult::Allowed,
        );
        self.access_metrics.record(access_result);
        if self.audit_mode == ForwardAuditMode::Metadata {
            let destination = result
                .as_ref()
                .ok()
                .map(|request| request.approved.destination().authority());
            let authenticated = result.as_ref().is_ok_and(|request| request.authenticated);
            let event = serde_json::json!({
                "event": "forward_access",
                "service": self.name,
                "protocol": protocol_name(protocol),
                "method": request.method().as_str(),
                "destination": destination,
                "result": access_result.as_str(),
                "reason": result
                    .as_ref()
                    .err()
                    .map_or("authorized", |error| error.reason()),
                "status": result.as_ref().err().map(|error| error.status().as_u16()),
                "authenticated": authenticated,
                "clientIp": client_addr.map(|address| address.ip().to_string()),
            });
            log::info!(target: "oxiroute::forward_proxy", "{event}");
        }
        result
    }

    async fn authorize_request_inner<B>(
        &self,
        request: &Request<B>,
        protocol: Protocol,
        client_addr: Option<SocketAddr>,
    ) -> Result<AuthorizedRequest, RequestFailure> {
        if protocol == Protocol::Http3
            && request.method() == Method::CONNECT
            && request.extensions().get::<h3::ext::Protocol>().is_some()
        {
            // Extended CONNECT is not an authority-only TCP tunnel in this listener.
            return Err(RequestFailure::BadRequest);
        }
        let target = request.uri().to_string();
        let connect_udp = classify_connect_udp_request(request);
        let parsed = if request.method() == Method::CONNECT {
            parse_connect_authority(&target).map(ParsedTarget::Tunnel)
        } else if connect_udp == ConnectUdpRequestKind::Valid {
            parse_connect_udp_path(&target).map(ParsedTarget::UdpTunnel)
        } else if connect_udp == ConnectUdpRequestKind::Malformed {
            return Err(RequestFailure::BadRequest);
        } else if self.allow_absolute_form {
            parse_absolute_form(&target).map(ParsedTarget::Forward)
        } else {
            return Err(RequestFailure::Forbidden);
        };
        let parsed = parsed.map_err(|_| RequestFailure::BadRequest)?;
        let destination = match &parsed {
            ParsedTarget::Forward(target) => &target.destination,
            ParsedTarget::Tunnel(destination) | ParsedTarget::UdpTunnel(destination) => destination,
        };
        let lifetime_deadline = Instant::now() + self.lifetime_timeout;
        if matches!(parsed, ParsedTarget::Tunnel(_))
            && (!self.connect_enabled || !self.connect_ports.contains(&destination.port))
        {
            return Err(RequestFailure::Forbidden);
        }
        if matches!(parsed, ParsedTarget::UdpTunnel(_))
            && (!self.connect_udp_enabled || !self.connect_udp_ports.contains(&destination.port))
        {
            return Err(RequestFailure::Forbidden);
        }
        if self.pre_resolution_denied(request, client_addr, destination) {
            return Err(RequestFailure::Forbidden);
        }
        let principal = if self.auth.is_some()
            && !self.anonymous_access_possible(request, client_addr, destination)
        {
            match timeout_at(lifetime_deadline, self.authenticate(request.headers())).await {
                Ok(Ok(principal)) => Some(principal),
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        } else {
            None
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let addresses = match timeout_at(connect_deadline, self.resolve(destination)).await {
            Ok(Ok(addresses)) => addresses,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        };
        let addresses = if self.resolver_revalidate_on_connect {
            match timeout_at(connect_deadline, self.resolve(destination)).await {
                Ok(Ok(addresses)) => addresses,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        } else {
            addresses
        };
        let principal = match timeout_at(
            lifetime_deadline,
            self.authorize_access(request, client_addr, destination, &addresses, principal),
        )
        .await
        {
            Ok(Ok(principal)) => principal,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        };
        let policy_principal = principal
            .clone()
            .unwrap_or_else(|| Principal::new("anonymous"));
        let approved = self
            .destination_policy
            .approve(
                &PolicyContext {
                    protocol,
                    principal: policy_principal,
                },
                destination,
                &addresses,
            )
            .map_err(|_| RequestFailure::Forbidden)?;
        Ok(AuthorizedRequest {
            approved,
            authenticated: principal.is_some(),
            parsed,
            lifetime_deadline,
        })
    }
}

impl ForwardHttp1ServicePlan {
    pub(super) async fn resolve(
        &self,
        destination: &Destination,
    ) -> Result<Vec<IpAddr>, RequestFailure> {
        match &destination.host {
            Host::Ip(address) => Ok(vec![*address]),
            Host::Dns(name) => {
                let _query = self
                    .resolver_queries
                    .acquire()
                    .await
                    .map_err(|_| RequestFailure::BadGateway)?;
                let lookup = self
                    .resolver
                    .lookup_ip(name)
                    .await
                    .map_err(|_| RequestFailure::BadGateway)?;
                let addresses = lookup
                    .iter()
                    .take(self.resolver_addresses)
                    .collect::<Vec<_>>();
                if lookup.iter().nth(self.resolver_addresses).is_some() {
                    return Err(RequestFailure::Forbidden);
                }
                (!addresses.is_empty())
                    .then_some(addresses)
                    .ok_or(RequestFailure::BadGateway)
            }
        }
    }

    async fn authorize_access<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
        addresses: &[IpAddr],
        mut principal: Option<Principal>,
    ) -> Result<Option<Principal>, RequestFailure> {
        let Some(policy) = &self.access_policy else {
            return if principal.is_some() {
                Ok(principal)
            } else if self.auth.is_some() {
                self.authenticate(request.headers()).await.map(Some)
            } else {
                Ok(None)
            };
        };
        if principal.is_none() && self.auth.is_some() {
            principal = self.authenticate(request.headers()).await.ok();
        }
        for rule in &policy.rules {
            let mut matched = true;
            for condition in &rule.conditions {
                let value = self.condition_matches(
                    condition,
                    request,
                    client_addr,
                    destination,
                    addresses,
                    principal.as_ref(),
                );
                if value == condition.negated {
                    matched = false;
                    break;
                }
            }
            if matched {
                return match rule.action {
                    ForwardAccessAction::Allow => Ok(principal),
                    ForwardAccessAction::Deny => Err(RequestFailure::Forbidden),
                };
            }
        }
        match policy.default_action {
            ForwardAccessAction::Allow => Ok(principal),
            ForwardAccessAction::Deny => Err(RequestFailure::Forbidden),
        }
    }

    fn pre_resolution_denied<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
    ) -> bool {
        let Some(policy) = &self.access_policy else {
            return false;
        };
        for rule in &policy.rules {
            let mut matched = true;
            let mut unknown = false;
            for condition in &rule.conditions {
                let value = match &condition.matcher {
                    ForwardAccessMatcher::All => Some(true),
                    ForwardAccessMatcher::Methods { methods } => Some(
                        methods
                            .iter()
                            .any(|method| method == request.method().as_str()),
                    ),
                    ForwardAccessMatcher::SourceCidrs { cidrs } => {
                        Some(source_cidrs_match(cidrs, client_addr))
                    }
                    ForwardAccessMatcher::DestinationPorts { ranges } => Some(
                        ranges
                            .iter()
                            .any(|range| (range.start..=range.end).contains(&destination.port)),
                    ),
                    ForwardAccessMatcher::Manager => Some(false),
                    ForwardAccessMatcher::Authenticated
                    | ForwardAccessMatcher::DestinationLocal
                    | ForwardAccessMatcher::DestinationLinkLocal => None,
                };
                match value {
                    Some(value) if value == condition.negated => {
                        matched = false;
                        break;
                    }
                    Some(_) => {}
                    None => unknown = true,
                }
            }
            if matched && unknown {
                return false;
            }
            if matched {
                return rule.action == ForwardAccessAction::Deny;
            }
        }
        policy.default_action == ForwardAccessAction::Deny
    }

    fn anonymous_access_possible<B>(
        &self,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
    ) -> bool {
        let Some(policy) = &self.access_policy else {
            return self.auth.is_none();
        };
        for rule in &policy.rules {
            let mut matched = true;
            let mut address_dependent = false;
            for condition in &rule.conditions {
                let value = match &condition.matcher {
                    ForwardAccessMatcher::All => Some(true),
                    ForwardAccessMatcher::Methods { methods } => Some(
                        methods
                            .iter()
                            .any(|method| method == request.method().as_str()),
                    ),
                    ForwardAccessMatcher::SourceCidrs { cidrs } => {
                        Some(source_cidrs_match(cidrs, client_addr))
                    }
                    ForwardAccessMatcher::DestinationPorts { ranges } => Some(
                        ranges
                            .iter()
                            .any(|range| (range.start..=range.end).contains(&destination.port)),
                    ),
                    ForwardAccessMatcher::Authenticated | ForwardAccessMatcher::Manager => {
                        Some(false)
                    }
                    ForwardAccessMatcher::DestinationLocal
                    | ForwardAccessMatcher::DestinationLinkLocal => None,
                };
                match value {
                    Some(value) if value == condition.negated => {
                        matched = false;
                        break;
                    }
                    Some(_) => {}
                    None => address_dependent = true,
                }
            }
            if !matched {
                continue;
            }
            if address_dependent {
                if rule.action == ForwardAccessAction::Allow {
                    return true;
                }
                continue;
            }
            return rule.action == ForwardAccessAction::Allow;
        }
        policy.default_action == ForwardAccessAction::Allow
    }

    fn condition_matches<B>(
        &self,
        condition: &ForwardAccessCondition,
        request: &Request<B>,
        client_addr: Option<SocketAddr>,
        destination: &Destination,
        addresses: &[IpAddr],
        principal: Option<&Principal>,
    ) -> bool {
        match &condition.matcher {
            ForwardAccessMatcher::All => true,
            ForwardAccessMatcher::Methods { methods } => methods
                .iter()
                .any(|method| method == request.method().as_str()),
            ForwardAccessMatcher::SourceCidrs { cidrs } => source_cidrs_match(cidrs, client_addr),
            ForwardAccessMatcher::DestinationPorts { ranges } => ranges
                .iter()
                .any(|range| (range.start..=range.end).contains(&destination.port)),
            ForwardAccessMatcher::Authenticated => principal.is_some(),
            ForwardAccessMatcher::DestinationLocal => addresses
                .iter()
                .any(|address| address.is_loopback() || self.local_addresses.contains(address)),
            ForwardAccessMatcher::DestinationLinkLocal => {
                addresses.iter().copied().any(is_link_local)
            }
            ForwardAccessMatcher::Manager => false,
        }
    }

    async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, RequestFailure> {
        let name = header::PROXY_AUTHORIZATION;
        let credentials = match single_header(headers, &name) {
            HeaderCardinality::Single(value) => value.as_bytes(),
            HeaderCardinality::Missing | HeaderCardinality::Duplicate => {
                return Err(RequestFailure::Authentication);
            }
        };
        match self.auth.as_ref() {
            Some(ForwardAuthPlan::Bearer(token)) if token.authorizes(credentials) => {
                Ok(Principal::new("bearer"))
            }
            Some(ForwardAuthPlan::Basic(auth)) => auth
                .authenticate(credentials)
                .await
                .map(Principal::new)
                .ok_or(RequestFailure::Authentication),
            _ => Err(RequestFailure::Authentication),
        }
    }
}
