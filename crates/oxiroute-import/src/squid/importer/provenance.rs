#[allow(clippy::wildcard_imports)]
use super::*;

struct ProvenanceRecorder {
    ledger: CanonicalProvenanceLedger<DirectiveOrigin>,
}

impl Default for ProvenanceRecorder {
    fn default() -> Self {
        Self {
            ledger: CanonicalProvenanceLedger::new(EmptyOriginPolicy::Require),
        }
    }
}

impl ProvenanceRecorder {
    fn record(&mut self, path: String, origins: Vec<DirectiveOrigin>) {
        self.ledger
            .record(path, origins, |origin| origin.occurrence);
    }

    fn into_entries(self) -> Vec<CanonicalProvenance<DirectiveOrigin>> {
        self.ledger.into_entries()
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn lower_provenance(
    effective: &EffectiveConfiguration,
    draft: &ConfigDraft,
) -> Vec<CanonicalProvenance<DirectiveOrigin>> {
    let mut recorder = ProvenanceRecorder::default();
    let http_ports = effective
        .ports
        .iter()
        .filter(|port| port.kind == PortKind::Http)
        .collect::<Vec<_>>();
    let port_origins = http_ports
        .iter()
        .map(|port| port.origin.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        draft.listeners.len(),
        http_ports.len(),
        "successful Squid lowering must retain one provenance source per listener"
    );
    recorder.record("/listeners".into(), port_origins.clone());

    for (index, (listener, port)) in draft.listeners.iter().zip(http_ports).enumerate() {
        let path = format!("/listeners/{index}");
        record_fields(
            &mut recorder,
            &path,
            ["", "/name", "/bind", "/bind/type", "/protocol", "/service"],
            std::slice::from_ref(&port.origin),
        );
        if matches!(listener.bind, ListenerBind::Socket { .. }) {
            recorder.record(format!("{path}/bind/address"), vec![port.origin.clone()]);
        }
    }

    let service = draft
        .forward_proxy_services
        .first()
        .expect("successful Squid lowering produces one forward-proxy service");
    let policy = effective
        .access_policies
        .iter()
        .find(|policy| policy.kind == AccessListKind::Http && policy.selector.is_none())
        .expect("successful Squid access lowering retains its HTTP policy");
    let lowered_policy = service
        .access_policy
        .as_ref()
        .expect("successful Squid lowering retains its access policy");
    let policy_origins = policy
        .rules
        .iter()
        .flat_map(|rule| access_rule_origins(effective, rule))
        .collect::<Vec<_>>();
    let header_origins = effective
        .privacy
        .iter()
        .filter_map(|privacy| match privacy {
            PrivacyDirective::ForwardedFor {
                origin,
                mode: ForwardedForMode::Delete,
            }
            | PrivacyDirective::Via {
                origin,
                enabled: false,
            } => Some(origin.clone()),
            PrivacyDirective::ForwardedFor { .. }
            | PrivacyDirective::Via { .. }
            | PrivacyDirective::HeaderReplace { .. } => None,
        })
        .collect::<Vec<_>>();
    let resolver_origins = effective
        .dns_nameservers
        .iter()
        .map(|directive| directive.origin.clone())
        .collect::<Vec<_>>();
    let audit_origins = effective
        .logging
        .iter()
        .filter(|logging| logging.destination == LogDestination::Disabled)
        .map(|logging| logging.origin.clone())
        .collect::<Vec<_>>();
    let peer_origins = effective
        .cache_peers
        .iter()
        .map(|peer| peer.origin.clone())
        .collect::<Vec<_>>();
    let direct_origins = effective
        .access_policies
        .iter()
        .filter(|policy| {
            matches!(
                policy.kind,
                AccessListKind::AlwaysDirect | AccessListKind::NeverDirect
            ) && policy.selector.is_none()
        })
        .flat_map(|policy| policy.rules.iter().map(|rule| rule.origin.clone()))
        .collect::<Vec<_>>();
    let auth_origins = effective
        .authentication_schemes
        .first()
        .map(|scheme| origins_for_occurrences(effective, scheme.parameters.iter().copied()))
        .unwrap_or_default();
    let service_origins = port_origins
        .iter()
        .cloned()
        .chain(policy_origins.iter().cloned())
        .chain(header_origins.iter().cloned())
        .chain(resolver_origins.iter().cloned())
        .chain(audit_origins.iter().cloned())
        .chain(peer_origins.iter().cloned())
        .chain(direct_origins.iter().cloned())
        .chain(auth_origins.iter().cloned())
        .collect::<Vec<_>>();
    let service_path = "/forward_proxy_services/0";
    recorder.record(service_path.into(), service_origins);
    record_fields(&mut recorder, service_path, ["/name"], &port_origins);
    record_fields(
        &mut recorder,
        service_path,
        ["/enabled_versions", "/tls_required"],
        &port_origins,
    );
    for version_index in 0..service.enabled_versions.len() {
        recorder.record(
            format!("{service_path}/enabled_versions/{version_index}"),
            port_origins.clone(),
        );
    }
    record_fields(
        &mut recorder,
        service_path,
        ["/connect", "/connect/enabled", "/connect/allowed_ports"],
        &policy_origins,
    );
    for port_index in 0..service.connect.allowed_ports.len() {
        recorder.record(
            format!("{service_path}/connect/allowed_ports/{port_index}"),
            policy_origins.clone(),
        );
    }
    record_fields(
        &mut recorder,
        service_path,
        [
            "/header_policy",
            "/header_policy/forwarded_for",
            "/header_policy/via",
        ],
        &header_origins,
    );
    if !resolver_origins.is_empty() {
        record_fields(
            &mut recorder,
            service_path,
            ["/resolver", "/resolver/nameservers"],
            &resolver_origins,
        );
        for nameserver_index in 0..service.resolver.nameservers.len() {
            recorder.record(
                format!("{service_path}/resolver/nameservers/{nameserver_index}"),
                resolver_origins.clone(),
            );
        }
    }
    record_fields(&mut recorder, service_path, ["/audit_mode"], &audit_origins);

    // CanonicalProvenance records native source contributions. Fields populated only by
    // OxiRoute defaults intentionally have no entry because this origin type has no default role.
    if service.auth.is_some() {
        let scheme = effective
            .authentication_schemes
            .first()
            .expect("successful Squid authentication lowering retains its scheme");
        let program_origins =
            authentication_parameter_origins(effective, scheme, AuthenticationSetting::Program);
        let realm_origins =
            authentication_parameter_origins(effective, scheme, AuthenticationSetting::Realm);
        let credential_ttl_origins = authentication_parameter_origins(
            effective,
            scheme,
            AuthenticationSetting::CredentialTtl,
        );
        let case_sensitive_origins = authentication_parameter_origins(
            effective,
            scheme,
            AuthenticationSetting::CaseSensitive,
        );
        let auth_path = format!("{service_path}/auth");
        record_fields(&mut recorder, &auth_path, ["", "/type"], &auth_origins);
        record_fields(
            &mut recorder,
            &auth_path,
            ["/htpasswd_file_path"],
            &program_origins,
        );
        record_fields(&mut recorder, &auth_path, ["/realm"], &realm_origins);
        record_fields(
            &mut recorder,
            &auth_path,
            ["/credential_ttl_ms"],
            &credential_ttl_origins,
        );
        if !case_sensitive_origins.is_empty() {
            record_fields(
                &mut recorder,
                &auth_path,
                ["/username_case_sensitive"],
                &case_sensitive_origins,
            );
        }
    }
    record_access_policy(
        &mut recorder,
        effective,
        policy,
        lowered_policy,
        service_path,
    );
    record_peer_policy(
        &mut recorder,
        &peer_origins,
        &direct_origins,
        &service.peer_policy,
        service_path,
    );

    recorder.into_entries()
}

fn record_peer_policy(
    recorder: &mut ProvenanceRecorder,
    peer_origins: &[DirectiveOrigin],
    direct_origins: &[DirectiveOrigin],
    policy: &ForwardPeerPolicy,
    service_path: &str,
) {
    if peer_origins.is_empty() && direct_origins.is_empty() {
        return;
    }
    let path = format!("{service_path}/peer_policy");
    let mut origins = peer_origins.to_vec();
    origins.extend(direct_origins.iter().cloned());
    recorder.record(path.clone(), origins.clone());
    if !peer_origins.is_empty() {
        recorder.record(format!("{path}/peers"), peer_origins.to_vec());
    }
    if !direct_origins.is_empty() {
        recorder.record(format!("{path}/direct_fallback"), direct_origins.to_vec());
    }
    if !peer_origins.is_empty() {
        recorder.record(format!("{path}/max_retries"), peer_origins.to_vec());
    }
    for (index, _peer) in policy.peers.iter().enumerate() {
        let peer_path = format!("{path}/peers/{index}");
        record_fields(recorder, &peer_path, ["", "/host", "/port"], peer_origins);
    }
}

fn record_access_policy(
    recorder: &mut ProvenanceRecorder,
    effective: &EffectiveConfiguration,
    policy: &super::super::AccessPolicy,
    lowered: &ForwardAccessPolicy,
    service_path: &str,
) {
    let policy_path = format!("{service_path}/access_policy");
    let policy_origins = policy
        .rules
        .iter()
        .flat_map(|rule| access_rule_origins(effective, rule))
        .collect::<Vec<_>>();
    assert_eq!(
        policy.rules.len(),
        lowered.rules.len(),
        "lowered Squid policy must retain every native rule"
    );
    recorder.record(policy_path.clone(), policy_origins.clone());
    recorder.record(format!("{policy_path}/rules"), policy_origins.clone());
    recorder.record(
        format!("{policy_path}/default_action"),
        policy_origins.clone(),
    );

    for (rule_index, (rule, lowered_rule)) in policy.rules.iter().zip(&lowered.rules).enumerate() {
        let rule_path = format!("{policy_path}/rules/{rule_index}");
        let rule_origins = access_rule_origins(effective, rule);
        record_fields(
            recorder,
            &rule_path,
            ["", "/action", "/conditions"],
            &rule_origins,
        );
        for (condition_index, (term, condition)) in
            rule.terms.iter().zip(&lowered_rule.conditions).enumerate()
        {
            let condition_path = format!("{rule_path}/conditions/{condition_index}");
            let condition_origins = access_term_origins(effective, rule, term);
            record_fields(
                recorder,
                &condition_path,
                ["", "/negated", "/type"],
                &condition_origins,
            );
            match &condition.matcher {
                ForwardAccessMatcher::Methods { methods } => {
                    recorder.record(
                        format!("{condition_path}/methods"),
                        condition_origins.clone(),
                    );
                    for index in 0..methods.len() {
                        recorder.record(
                            format!("{condition_path}/methods/{index}"),
                            condition_origins.clone(),
                        );
                    }
                }
                ForwardAccessMatcher::SourceCidrs { cidrs } => {
                    recorder.record(format!("{condition_path}/cidrs"), condition_origins.clone());
                    for index in 0..cidrs.len() {
                        recorder.record(
                            format!("{condition_path}/cidrs/{index}"),
                            condition_origins.clone(),
                        );
                    }
                }
                ForwardAccessMatcher::DestinationPorts { ranges } => {
                    recorder.record(
                        format!("{condition_path}/ranges"),
                        condition_origins.clone(),
                    );
                    for (range_index, _) in ranges.iter().enumerate() {
                        let range_path = format!("{condition_path}/ranges/{range_index}");
                        record_fields(
                            recorder,
                            &range_path,
                            ["", "/start", "/end"],
                            &condition_origins,
                        );
                    }
                }
                ForwardAccessMatcher::All
                | ForwardAccessMatcher::Authenticated
                | ForwardAccessMatcher::DestinationLocal
                | ForwardAccessMatcher::DestinationLinkLocal
                | ForwardAccessMatcher::Manager => {}
            }
        }
    }
}

fn access_rule_origins(
    effective: &EffectiveConfiguration,
    rule: &super::super::AccessRule,
) -> Vec<DirectiveOrigin> {
    let mut origins = vec![rule.origin.clone()];
    for term in &rule.terms {
        origins.extend(access_term_origins(effective, rule, term));
    }
    origins
}

fn access_term_origins(
    effective: &EffectiveConfiguration,
    rule: &super::super::AccessRule,
    term: &AclTerm,
) -> Vec<DirectiveOrigin> {
    let mut origins = vec![rule.origin.clone()];
    if let AclReferenceResolution::Defined(definitions) = &term.resolution {
        origins.extend(origins_for_occurrences(
            effective,
            definitions.iter().copied(),
        ));
    }
    origins
}

fn origins_for_occurrences(
    effective: &EffectiveConfiguration,
    occurrences: impl IntoIterator<Item = OccurrenceId>,
) -> Vec<DirectiveOrigin> {
    occurrences
        .into_iter()
        .map(|occurrence| {
            effective
                .ledger
                .decision(occurrence)
                .unwrap_or_else(|| panic!("Squid provenance occurrence {occurrence:?} is missing"))
                .origin
                .clone()
        })
        .collect()
}

fn authentication_parameter_origins(
    effective: &EffectiveConfiguration,
    scheme: &AuthenticationScheme,
    setting: AuthenticationSetting,
) -> Vec<DirectiveOrigin> {
    scheme
        .parameters
        .iter()
        .filter_map(|occurrence| {
            effective
                .authentication
                .iter()
                .find(|parameter| {
                    parameter.origin.occurrence == *occurrence && parameter.setting == setting
                })
                .map(|parameter| parameter.origin.clone())
        })
        .collect()
}

fn record_fields<const N: usize>(
    recorder: &mut ProvenanceRecorder,
    base: &str,
    suffixes: [&str; N],
    origins: &[DirectiveOrigin],
) {
    for suffix in suffixes {
        recorder.record(format!("{base}{suffix}"), origins.to_vec());
    }
}
