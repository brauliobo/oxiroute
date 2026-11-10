#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct LoweredCanonical {
    pub(super) draft: ConfigDraft,
    pub(super) provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
}

#[allow(clippy::too_many_lines)]
pub(super) fn lower(
    effective: &EffectiveConfiguration,
) -> Result<LoweredCanonical, SemanticBlockerKind> {
    let http_ports = effective
        .ports
        .iter()
        .filter(|port| port.kind == PortKind::Http)
        .collect::<Vec<_>>();
    if http_ports.is_empty() {
        return Err(SemanticBlockerKind::ForwardProxyListener);
    }
    if http_ports.iter().any(|port| !port.options.is_empty()) {
        return Err(SemanticBlockerKind::UnsupportedPortOption);
    }
    let peer_policy = lower_peer_policy(effective)?;
    if !effective.authentication_controls.is_empty() {
        return Err(SemanticBlockerKind::ProxyAuthentication);
    }
    let auth = lower_authentication(&effective.authentication_schemes)
        .map_err(|()| SemanticBlockerKind::ProxyAuthentication)?;
    let access_policy =
        lower_access_policy(effective).ok_or(SemanticBlockerKind::OrderedHttpAccess)?;
    let connect_ports =
        connect_ports(&access_policy).ok_or(SemanticBlockerKind::DestinationPortAcl)?;
    let resolver = ForwardResolverPolicy {
        nameservers: effective
            .dns_nameservers
            .iter()
            .flat_map(|directive| directive.addresses.iter().copied())
            .collect(),
        ..ForwardResolverPolicy::default()
    };
    let mut header_policy = ForwardHeaderPolicy::default();
    let mut forwarded_for = false;
    let mut via = false;
    for privacy in &effective.privacy {
        match privacy {
            PrivacyDirective::ForwardedFor {
                mode: ForwardedForMode::Delete,
                ..
            } => {
                header_policy.forwarded_for = ForwardedForPolicy::Delete;
                forwarded_for = true;
            }
            PrivacyDirective::Via { enabled: false, .. } => {
                header_policy.via = ForwardViaPolicy::Delete;
                via = true;
            }
            PrivacyDirective::ForwardedFor { .. } => {
                return Err(SemanticBlockerKind::ForwardedForPolicy);
            }
            PrivacyDirective::Via { .. } => return Err(SemanticBlockerKind::ViaPolicy),
            PrivacyDirective::HeaderReplace { .. } => {
                return Err(SemanticBlockerKind::HeaderPrivacyPolicy);
            }
        }
    }
    if !forwarded_for {
        return Err(SemanticBlockerKind::ForwardedForPolicy);
    }
    if !via {
        return Err(SemanticBlockerKind::ViaPolicy);
    }
    let audit_mode = if !effective.logging.is_empty()
        && effective
            .logging
            .iter()
            .all(|logging| logging.destination == LogDestination::Disabled)
    {
        ForwardAuditMode::Off
    } else {
        return Err(SemanticBlockerKind::AccessLoggingPolicy);
    };
    let service_name = "squid-forward".to_owned();
    let listeners = http_ports
        .iter()
        .enumerate()
        .map(|(index, port)| {
            let address = match &port.endpoint {
                PortEndpoint::Wildcard { port } => {
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), *port)
                }
                PortEndpoint::Ip { address, port } => SocketAddr::new(*address, *port),
                PortEndpoint::Host { .. } => {
                    return Err(SemanticBlockerKind::ForwardProxyListener);
                }
            };
            Ok(Listener {
                name: format!("squid-forward-{index}"),
                bind: ListenerBind::Socket { address },
                protocol: Protocol::ForwardHttp1,
                service: Some(service_name.clone()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let service = ForwardProxyService {
        drain_deadline_ms: None,
        name: service_name,
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: false,
        connect: ForwardConnectPolicy {
            enabled: !connect_ports.is_empty(),
            allowed_ports: connect_ports,
        },
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy,
        auth,
        access_policy: Some(access_policy),
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy,
        connect_timeout_ms: 10_000,
        idle_timeout_ms: 300_000,
        lifetime_timeout_ms: 3_600_000,
        max_request_body_bytes: Some(10 * 1024 * 1024),
        max_header_bytes: 64 * 1024,
        max_connections: 10_000,
        resolver,
        audit_mode,
    };
    let draft = ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        listeners,
        forward_proxy_services: vec![service],
        ..empty_config()
    };
    Ok(LoweredCanonical {
        provenance: lower_provenance(effective, &draft),
        draft,
    })
}

pub(super) fn lower_authentication(
    schemes: &[AuthenticationScheme],
) -> Result<Option<ForwardProxyAuth>, ()> {
    if schemes.is_empty() {
        return Ok(None);
    }
    let [scheme] = schemes else {
        return Err(());
    };
    if !scheme.scheme.eq_ignore_ascii_case(b"basic") {
        return Err(());
    }
    if scheme.unsupported_settings {
        return Err(());
    }
    let program = scheme.basic_program.as_ref().ok_or(())?;
    let [helper, htpasswd] = program.arguments.as_slice() else {
        return Err(());
    };
    if Path::new(OsStr::from_bytes(helper)).file_name() != Some(OsStr::new("basic_ncsa_auth")) {
        return Err(());
    }
    let path = PathBuf::from(OsString::from_vec(htpasswd.clone()));
    if !path.is_absolute() {
        return Err(());
    }
    let realm = std::str::from_utf8(&scheme.realm_value.as_ref().ok_or(())?.value)
        .map_err(|_| ())?
        .to_owned();
    let credential_ttl_ms =
        u64::try_from(scheme.credential_ttl.ok_or(())?.as_millis()).map_err(|_| ())?;
    Ok(Some(ForwardProxyAuth::BasicHtpasswdFile {
        htpasswd_file_path: path,
        realm,
        credential_ttl_ms: Some(credential_ttl_ms),
        username_case_sensitive: scheme.case_sensitive.unwrap_or(false),
    }))
}

pub(super) fn lower_peer_policy(
    effective: &EffectiveConfiguration,
) -> Result<ForwardPeerPolicy, SemanticBlockerKind> {
    let direct_fallback = lower_direct_policy(effective)?;
    let mut peers = Vec::with_capacity(effective.cache_peers.len());
    let mut identities = BTreeSet::new();
    for peer in &effective.cache_peers {
        if peer.peer_type != super::super::CachePeerType::Parent
            || peer.http_port == 0
            || peer.icp_port != 0
            || !peer.options.is_empty()
        {
            return Err(SemanticBlockerKind::CachePeerHierarchy);
        }
        let host = ip_address(&peer.host.value)
            .map(|address| address.to_string())
            .or_else(|| dns_name(&peer.host.value))
            .ok_or(SemanticBlockerKind::CachePeerHierarchy)?;
        if !identities.insert((host.clone(), peer.http_port)) {
            return Err(SemanticBlockerKind::CachePeerHierarchy);
        }
        peers.push(ForwardPeer {
            host,
            port: peer.http_port,
        });
    }
    if peers.is_empty() && direct_fallback == ForwardDirectFallback::Denied {
        return Err(SemanticBlockerKind::DirectRoutingPolicy);
    }
    let max_retries = u8::try_from(peers.len().saturating_sub(1))
        .map_err(|_| SemanticBlockerKind::CachePeerHierarchy)?;
    Ok(ForwardPeerPolicy {
        peers,
        direct_fallback,
        max_retries,
    })
}

fn lower_direct_policy(
    effective: &EffectiveConfiguration,
) -> Result<ForwardDirectFallback, SemanticBlockerKind> {
    let always = lower_direct_policy_list(effective, AccessListKind::AlwaysDirect)?;
    let never = lower_direct_policy_list(effective, AccessListKind::NeverDirect)?;
    if always && never {
        return Err(SemanticBlockerKind::DirectRoutingPolicy);
    }
    Ok(if always {
        ForwardDirectFallback::Required
    } else if never {
        ForwardDirectFallback::Denied
    } else {
        ForwardDirectFallback::Allowed
    })
}

fn lower_direct_policy_list(
    effective: &EffectiveConfiguration,
    kind: AccessListKind,
) -> Result<bool, SemanticBlockerKind> {
    let policies = effective
        .access_policies
        .iter()
        .filter(|policy| policy.kind == kind && policy.selector.is_none())
        .collect::<Vec<_>>();
    if policies.is_empty() {
        return Ok(false);
    }
    let [policy] = policies.as_slice() else {
        return Err(SemanticBlockerKind::DirectRoutingPolicy);
    };
    let [rule] = policy.rules.as_slice() else {
        return Err(SemanticBlockerKind::DirectRoutingPolicy);
    };
    if rule.terms.len() != 1
        || rule.terms[0].negated
        || !matches!(
            rule.terms[0].resolution,
            AclReferenceResolution::Builtin(super::super::BuiltinAcl::All)
        )
    {
        return Err(SemanticBlockerKind::DirectRoutingPolicy);
    }
    Ok(rule.action == AccessAction::Allow)
}

pub(super) fn lower_access_policy(
    effective: &EffectiveConfiguration,
) -> Option<ForwardAccessPolicy> {
    let policies = effective
        .access_policies
        .iter()
        .filter(|policy| policy.kind == AccessListKind::Http && policy.selector.is_none())
        .collect::<Vec<_>>();
    let [policy] = policies.as_slice() else {
        return None;
    };
    let rules = policy
        .rules
        .iter()
        .map(|rule| {
            Some(ForwardAccessRule {
                action: access_action(rule.action),
                conditions: rule
                    .terms
                    .iter()
                    .map(|term| lower_term(term, &effective.acls))
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ForwardAccessPolicy {
        rules,
        default_action: access_action(policy.default_action),
    })
}

fn lower_term(term: &AclTerm, acls: &[EffectiveAcl]) -> Option<ForwardAccessCondition> {
    let matcher = match &term.resolution {
        AclReferenceResolution::Builtin(BuiltinAcl::All) => ForwardAccessMatcher::All,
        AclReferenceResolution::Builtin(BuiltinAcl::Connect) => ForwardAccessMatcher::Methods {
            methods: vec!["CONNECT".into()],
        },
        AclReferenceResolution::Builtin(BuiltinAcl::Localhost) => {
            ForwardAccessMatcher::SourceCidrs {
                cidrs: vec!["127.0.0.0/8".into(), "::1/128".into()],
            }
        }
        AclReferenceResolution::Builtin(BuiltinAcl::Manager) => ForwardAccessMatcher::Manager,
        AclReferenceResolution::Builtin(BuiltinAcl::ToLocalhost) => {
            ForwardAccessMatcher::DestinationLocal
        }
        AclReferenceResolution::Builtin(BuiltinAcl::ToLinkLocal) => {
            ForwardAccessMatcher::DestinationLinkLocal
        }
        AclReferenceResolution::Defined(_) => {
            let acl = acls.iter().find(|acl| acl.name == term.name.value)?;
            lower_acl(acl)?
        }
        AclReferenceResolution::Unresolved => return None,
    };
    Some(ForwardAccessCondition {
        negated: term.negated,
        matcher,
    })
}

fn lower_acl(acl: &EffectiveAcl) -> Option<ForwardAccessMatcher> {
    let first = acl.matchers.first()?;
    match first {
        AclMatcher::Source(_) => Some(ForwardAccessMatcher::SourceCidrs {
            cidrs: acl
                .matchers
                .iter()
                .map(|matcher| match matcher {
                    AclMatcher::Source(network) => {
                        Some(format!("{}/{}", network.address, network.prefix_length))
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?,
        }),
        AclMatcher::Port(_) => Some(ForwardAccessMatcher::DestinationPorts {
            ranges: acl
                .matchers
                .iter()
                .map(|matcher| match matcher {
                    AclMatcher::Port(range) => Some(ForwardPortRange {
                        start: range.start,
                        end: range.end,
                    }),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?,
        }),
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Required) => {
            if acl.matchers.len() == 1 {
                Some(ForwardAccessMatcher::Authenticated)
            } else {
                None
            }
        }
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Identity(_)) => None,
    }
}

fn connect_ports(policy: &ForwardAccessPolicy) -> Option<Vec<u16>> {
    let mut guards = policy.rules.iter().enumerate().filter_map(|(index, rule)| {
        if rule.action != ForwardAccessAction::Deny || rule.conditions.len() != 2 {
            return None;
        }
        rule.conditions.iter().find(|condition| {
            !condition.negated
                && matches!(
                    &condition.matcher,
                    ForwardAccessMatcher::Methods { methods }
                        if methods.as_slice() == ["CONNECT"]
                )
        })?;
        let ports = rule.conditions.iter().find(|condition| {
            condition.negated
                && matches!(
                    condition.matcher,
                    ForwardAccessMatcher::DestinationPorts { .. }
                )
        })?;
        let ForwardAccessMatcher::DestinationPorts { ranges } = &ports.matcher else {
            unreachable!("guard selected a destination-port condition");
        };
        Some((index, ranges))
    });
    let (guard_index, ranges) = guards.next()?;
    if guards.next().is_some() || ranges.iter().any(|range| range.start != range.end) {
        return None;
    }
    if policy.rules[..guard_index].iter().any(|rule| {
        rule.action == ForwardAccessAction::Allow
            && rule.conditions.iter().all(|condition| {
                !matches!(
                    &condition.matcher,
                    ForwardAccessMatcher::Methods { methods }
                        if (!condition.negated && !methods.iter().any(|method| method == "CONNECT"))
                            || (condition.negated
                                && methods.iter().any(|method| method == "CONNECT"))
                )
            })
    }) {
        return None;
    }
    let mut ports = ranges.iter().map(|range| range.start).collect::<Vec<_>>();
    if ports.is_empty() {
        return None;
    }
    ports.sort_unstable();
    ports.dedup();
    Some(ports)
}

const fn access_action(action: AccessAction) -> ForwardAccessAction {
    match action {
        AccessAction::Allow => ForwardAccessAction::Allow,
        AccessAction::Deny => ForwardAccessAction::Deny,
    }
}
