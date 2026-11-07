#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn consumed_occurrences(effective: &EffectiveConfiguration) -> BTreeSet<OccurrenceId> {
    let mut occurrences = BTreeSet::new();
    occurrences.extend(
        effective
            .ports
            .iter()
            .filter(|port| {
                port.kind == PortKind::Http
                    && port.options.is_empty()
                    && !matches!(port.endpoint, PortEndpoint::Host { .. })
            })
            .map(|port| port.origin.occurrence),
    );
    if lower_peer_policy(effective).is_ok() {
        extend_consumed_peer_occurrences(effective, &mut occurrences);
    }
    occurrences.extend(
        effective
            .acl_definitions
            .iter()
            .filter(|definition| {
                effective
                    .ledger
                    .decision(definition.origin.occurrence)
                    .is_some_and(|decision| {
                        matches!(
                            decision.outcome,
                            DecisionOutcome::Classified {
                                activation: Activation::Blocked(
                                    SemanticBlockerKind::SourceAddressAcl
                                        | SemanticBlockerKind::DestinationPortAcl
                                        | SemanticBlockerKind::ProxyAuthenticationAcl
                                ),
                                ..
                            }
                        )
                    })
            })
            .map(|definition| definition.origin.occurrence),
    );
    if let Some(policy) = lower_access_policy(effective).and_then(|_| {
        effective
            .access_policies
            .iter()
            .find(|policy| policy.kind == AccessListKind::Http && policy.selector.is_none())
    }) {
        for rule in &policy.rules {
            occurrences.insert(rule.origin.occurrence);
            for term in &rule.terms {
                if let AclReferenceResolution::Defined(definitions) = &term.resolution {
                    occurrences.extend(definitions.iter().copied());
                }
            }
        }
    }
    if effective.authentication_controls.is_empty()
        && lower_authentication(&effective.authentication_schemes).is_ok()
    {
        occurrences.extend(
            effective
                .authentication_schemes
                .iter()
                .flat_map(|scheme| scheme.parameters.iter().copied()),
        );
    }
    if !effective.logging.is_empty()
        && effective
            .logging
            .iter()
            .all(|logging| logging.destination == LogDestination::Disabled)
    {
        occurrences.extend(
            effective
                .logging
                .iter()
                .map(|logging| logging.origin.occurrence),
        );
    }
    extend_consumed_dns_occurrences(effective, &mut occurrences);
    occurrences.extend(
        effective
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
                } => Some(origin.occurrence),
                PrivacyDirective::ForwardedFor { .. }
                | PrivacyDirective::Via { .. }
                | PrivacyDirective::HeaderReplace { .. } => None,
            }),
    );
    occurrences
}

fn extend_consumed_peer_occurrences(
    effective: &EffectiveConfiguration,
    occurrences: &mut BTreeSet<OccurrenceId>,
) {
    occurrences.extend(
        effective
            .cache_peers
            .iter()
            .map(|peer| peer.origin.occurrence),
    );
    occurrences.extend(
        effective
            .access_policies
            .iter()
            .filter(|policy| {
                matches!(
                    policy.kind,
                    AccessListKind::AlwaysDirect | AccessListKind::NeverDirect
                ) && policy.selector.is_none()
            })
            .flat_map(|policy| policy.rules.iter().map(|rule| rule.origin.occurrence)),
    );
}

fn extend_consumed_dns_occurrences(
    effective: &EffectiveConfiguration,
    occurrences: &mut BTreeSet<OccurrenceId>,
) {
    const MAX_NAMESERVERS: usize = 8;
    let nameservers = effective
        .dns_nameservers
        .iter()
        .flat_map(|directive| directive.addresses.iter().copied())
        .collect::<Vec<_>>();
    let unique_nameservers = nameservers.iter().copied().collect::<BTreeSet<_>>();
    if nameservers.len() <= MAX_NAMESERVERS
        && unique_nameservers.len() == nameservers.len()
        && nameservers
            .iter()
            .all(|address| !address.is_unspecified() && !address.is_multicast())
    {
        occurrences.extend(
            effective
                .dns_nameservers
                .iter()
                .map(|nameservers| nameservers.origin.occurrence),
        );
    }
}
