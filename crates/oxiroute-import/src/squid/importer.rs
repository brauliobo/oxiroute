use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use oxiroute_config::{
    Config, DownstreamTimeoutPolicy, ForwardAccessAction, ForwardAccessCondition,
    ForwardAccessMatcher, ForwardAccessPolicy, ForwardAccessRule, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion,
    ForwardPortRange, ForwardProxyAuth, ForwardProxyService, ForwardResolverPolicy,
    ForwardViaPolicy, ForwardedForPolicy, Listener, ListenerBind, Protocol,
};

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Severity,
};

use super::{
    AccessAction, AccessListKind, AclMatcher, AclReferenceResolution, AclTerm, Activation,
    AuthenticationScheme, BuiltinAcl, DecisionLedger, DecisionOutcome, DirectiveOrigin,
    EffectiveAcl, EffectiveConfiguration, ForwardedForMode, LogDestination, OccurrenceId,
    PortEndpoint, PortKind, PrivacyDirective, ProxyAuthMatcher, RootSelection, SemanticBlockerKind,
    SourceGraph, analyze, load, load_selected,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedCapability {
    pub kind: SemanticBlockerKind,
    pub occurrences: Vec<OccurrenceId>,
    pub diagnostic_code: DiagnosticCode,
}

/// Complete Squid source, semantic, blocker, and canonicalization evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub source_graph: SourceGraph,
    pub effective: EffectiveConfiguration,
    pub decision_ledger: DecisionLedger,
    pub blocked_capabilities: Vec<BlockedCapability>,
    pub draft: CanonicalDraft,
    pub canonical_provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    pub config: Option<Config>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedImportReport {
    pub selection: RootSelection,
    pub import: ImportReport,
}

/// Schema-independent input handed to a future canonical lowering implementation.
#[derive(Clone, Copy, Debug)]
pub struct LoweringView<'a> {
    pub effective: &'a EffectiveConfiguration,
    pub decision_ledger: &'a DecisionLedger,
    pub blocked_capabilities: &'a [BlockedCapability],
}

/// Adapter boundary for canonical forward-proxy/cache schema implementations.
pub trait SquidLoweringAdapter {
    type Output;
    type Error;

    /// # Errors
    ///
    /// Returns an adapter-defined error when the target schema cannot represent the typed view.
    fn lower(&self, source: LoweringView<'_>) -> Result<Self::Output, Self::Error>;
}

impl ImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }

    /// # Errors
    ///
    /// Returns the error produced by `adapter`.
    pub fn lower_with<A: SquidLoweringAdapter>(&self, adapter: &A) -> Result<A::Output, A::Error> {
        adapter.lower(LoweringView {
            effective: &self.effective,
            decision_ledger: &self.decision_ledger,
            blocked_capabilities: &self.blocked_capabilities,
        })
    }
}

/// Loads, classifies, and audits one active Squid configuration graph.
#[must_use]
pub fn import(root: &Path) -> ImportReport {
    let (graph, diagnostics) = load(root).into_parts();
    import_graph(graph, diagnostics)
}

/// Discovers the active native CLI root, then runs the report-preserving import pipeline.
#[must_use]
pub fn import_selected(arguments: &[OsString], compiled_default: &Path) -> SelectedImportReport {
    let (selected, diagnostics) = load_selected(arguments, compiled_default).into_parts();
    let import = import_graph(selected.graph, diagnostics);
    SelectedImportReport {
        selection: selected.selection,
        import,
    }
}

#[allow(clippy::too_many_lines)]
fn import_graph(graph: SourceGraph, mut diagnostics: Vec<Diagnostic>) -> ImportReport {
    let (effective, semantic_diagnostics) = analyze(&graph).into_parts();
    diagnostics.extend(semantic_diagnostics);
    if let Some(pattern) = effective.refresh_policy.patterns.first() {
        diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FEATURE,
                Severity::Warning,
                DiagnosticStage::Lower,
                "Squid cache freshness rules are externalized because OxiRoute forward proxying is direct and non-caching",
            )
            .with_primary_span(pattern.origin.directive_span),
        );
    }

    let lowered = lower(&effective);
    let lowered_occurrences = consumed_occurrences(&effective);
    let mut grouped = BTreeMap::<SemanticBlockerKind, Vec<OccurrenceId>>::new();
    for decision in &effective.ledger.decisions {
        let DecisionOutcome::Classified {
            activation: Activation::Blocked(kind),
            ..
        } = decision.outcome
        else {
            continue;
        };
        if !lowered_occurrences.contains(&decision.origin.occurrence) {
            grouped
                .entry(kind)
                .or_default()
                .push(decision.origin.occurrence);
        }
    }

    let mut blocked_capabilities: Vec<BlockedCapability> = grouped
        .into_iter()
        .map(|(kind, occurrences)| {
            let diagnostic_code = blocker_code(kind);
            if is_schema_blocker(kind) {
                let first = occurrences
                    .first()
                    .and_then(|occurrence| effective.ledger.decision(*occurrence));
                let mut diagnostic = Diagnostic::new(
                    diagnostic_code,
                    Severity::Error,
                    DiagnosticStage::Lower,
                    blocker_message(kind),
                );
                if let Some(first) = first {
                    diagnostic = diagnostic
                        .with_primary_span(first.origin.directive_span)
                        .with_include_stack(
                            first
                                .origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        );
                }
                diagnostics.push(diagnostic);
            }
            BlockedCapability {
                kind,
                occurrences,
                diagnostic_code,
            }
        })
        .collect();
    if let Err(kind) = &lowered {
        if !blocked_capabilities
            .iter()
            .any(|capability| capability.kind == *kind)
        {
            let occurrences = effective
                .ledger
                .decisions
                .iter()
                .filter_map(|decision| match decision.outcome {
                    DecisionOutcome::Classified {
                        activation: Activation::Blocked(blocked),
                        ..
                    } if blocked == *kind => Some(decision.origin.occurrence),
                    DecisionOutcome::Classified { .. } => None,
                })
                .collect::<Vec<_>>();
            diagnostics.push(lower_blocker_diagnostic(*kind, &effective, &graph));
            blocked_capabilities.push(BlockedCapability {
                kind: *kind,
                occurrences,
                diagnostic_code: blocker_code(*kind),
            });
            blocked_capabilities.sort_unstable_by_key(|capability| capability.kind);
        }
    }
    let decision_ledger = effective.ledger.clone();
    let (draft, canonical_provenance) = lowered.map_or_else(
        |_| (CanonicalDraft::default(), Vec::new()),
        |lowered| (lowered.draft, lowered.provenance),
    );
    let mut config = draft.to_config();
    let config = if blocked_capabilities.is_empty()
        && !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    {
        match oxiroute_config::validate_config(&mut config) {
            Ok(()) => Some(config),
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    Severity::Error,
                    DiagnosticStage::Validate,
                    format!("lowered Squid candidate failed canonical validation: {error}"),
                ));
                None
            }
        }
    } else {
        None
    };
    let ((), diagnostics) = crate::Report::new((), diagnostics).into_parts();

    ImportReport {
        source_graph: graph,
        effective,
        decision_ledger,
        blocked_capabilities,
        draft,
        canonical_provenance,
        config,
        diagnostics,
    }
}

fn consumed_occurrences(effective: &EffectiveConfiguration) -> BTreeSet<OccurrenceId> {
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

struct LoweredCanonical {
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
}

#[allow(clippy::too_many_lines)]
fn lower(effective: &EffectiveConfiguration) -> Result<LoweredCanonical, SemanticBlockerKind> {
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
    if !effective.cache_peers.is_empty() {
        return Err(SemanticBlockerKind::CachePeerHierarchy);
    }
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
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let service = ForwardProxyService {
        name: service_name,
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: false,
        connect: ForwardConnectPolicy {
            enabled: !connect_ports.is_empty(),
            allowed_ports: connect_ports,
        },
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
    let draft = CanonicalDraft {
        listeners,
        forward_proxy_services: vec![service],
        ..CanonicalDraft::default()
    };
    Ok(LoweredCanonical {
        provenance: lower_provenance(effective, &draft),
        draft,
    })
}

#[derive(Default)]
struct ProvenanceRecorder {
    entries: Vec<CanonicalProvenance<DirectiveOrigin>>,
}

impl ProvenanceRecorder {
    fn record(&mut self, path: String, mut origins: Vec<DirectiveOrigin>) {
        assert!(
            !origins.is_empty(),
            "finalized Squid canonical field lacks source provenance: {path}"
        );
        origins.sort_unstable_by_key(|origin| origin.occurrence);
        origins.dedup_by_key(|origin| origin.occurrence);
        if let Some(existing) = self.entries.iter_mut().find(|entry| entry.path == path) {
            existing.origins.extend(origins);
            existing
                .origins
                .sort_unstable_by_key(|origin| origin.occurrence);
            existing.origins.dedup_by_key(|origin| origin.occurrence);
        } else {
            self.entries.push(CanonicalProvenance { path, origins });
        }
    }
}

#[allow(clippy::too_many_lines)]
fn lower_provenance(
    effective: &EffectiveConfiguration,
    draft: &CanonicalDraft,
) -> Vec<CanonicalProvenance<DirectiveOrigin>> {
    let mut recorder = ProvenanceRecorder::default();
    let all_origins = origins_for_occurrences(effective, consumed_occurrences(effective));
    let http_ports = effective
        .ports
        .iter()
        .filter(|port| port.kind == PortKind::Http)
        .collect::<Vec<_>>();
    assert_eq!(
        draft.listeners.len(),
        http_ports.len(),
        "successful Squid lowering must retain one provenance source per listener"
    );

    for (index, (listener, port)) in draft.listeners.iter().zip(http_ports).enumerate() {
        let path = format!("/listeners/{index}");
        record_fields(
            &mut recorder,
            &path,
            [
                "",
                "/name",
                "/bind",
                "/bind/type",
                "/protocol",
                "/service",
                "/tls_profile",
                "/max_connections",
                "/downstream_timeouts",
                "/downstream_timeouts/client_timeout_ms",
                "/downstream_timeouts/request_timeout_ms",
                "/downstream_timeouts/keepalive_timeout_ms",
            ],
            &[port.origin.clone()],
        );
        if matches!(listener.bind, ListenerBind::Socket { .. }) {
            recorder.record(format!("{path}/bind/address"), vec![port.origin.clone()]);
        }
    }

    let service = draft
        .forward_proxy_services
        .first()
        .expect("successful Squid lowering produces one forward-proxy service");
    let service_path = "/forward_proxy_services/0";
    record_fields(
        &mut recorder,
        service_path,
        [
            "",
            "/name",
            "/enabled_versions",
            "/allow_absolute_form",
            "/tls_required",
            "/connect",
            "/connect/enabled",
            "/connect/allowed_ports",
            "/destination_policy",
            "/destination_policy/allow_domains",
            "/destination_policy/deny_domains",
            "/destination_policy/allow_cidrs",
            "/destination_policy/deny_cidrs",
            "/destination_policy/deny_private",
            "/header_policy",
            "/header_policy/forwarded_for",
            "/header_policy/via",
            "/connect_timeout_ms",
            "/idle_timeout_ms",
            "/lifetime_timeout_ms",
            "/max_request_body_bytes",
            "/max_header_bytes",
            "/max_connections",
            "/resolver",
            "/resolver/nameservers",
            "/resolver/max_cache_entries",
            "/resolver/max_concurrent_queries",
            "/resolver/max_addresses_per_name",
            "/resolver/min_ttl_ms",
            "/resolver/max_ttl_ms",
            "/resolver/negative_ttl_ms",
            "/resolver/revalidate_on_connect",
            "/audit_mode",
        ],
        &all_origins,
    );
    for version_index in 0..service.enabled_versions.len() {
        recorder.record(
            format!("{service_path}/enabled_versions/{version_index}"),
            all_origins.clone(),
        );
    }
    for port_index in 0..service.connect.allowed_ports.len() {
        recorder.record(
            format!("{service_path}/connect/allowed_ports/{port_index}"),
            all_origins.clone(),
        );
    }
    for nameserver_index in 0..service.resolver.nameservers.len() {
        recorder.record(
            format!("{service_path}/resolver/nameservers/{nameserver_index}"),
            all_origins.clone(),
        );
    }

    if service.auth.is_some() {
        let scheme = effective
            .authentication_schemes
            .first()
            .expect("successful Squid authentication lowering retains its scheme");
        let origins = origins_for_occurrences(effective, scheme.parameters.iter().copied());
        let auth_path = format!("{service_path}/auth");
        record_fields(
            &mut recorder,
            &auth_path,
            [
                "",
                "/type",
                "/htpasswd_file_path",
                "/realm",
                "/credential_ttl_ms",
                "/username_case_sensitive",
            ],
            &origins,
        );
    }

    let policy = effective
        .access_policies
        .iter()
        .find(|policy| policy.kind == AccessListKind::Http && policy.selector.is_none())
        .expect("successful Squid access lowering retains its HTTP policy");
    let lowered_policy = service
        .access_policy
        .as_ref()
        .expect("successful Squid lowering retains its access policy");
    record_access_policy(
        &mut recorder,
        effective,
        policy,
        lowered_policy,
        service_path,
    );

    recorder.entries
}

fn record_access_policy(
    recorder: &mut ProvenanceRecorder,
    effective: &EffectiveConfiguration,
    policy: &super::AccessPolicy,
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
    rule: &super::AccessRule,
) -> Vec<DirectiveOrigin> {
    let mut origins = vec![rule.origin.clone()];
    for term in &rule.terms {
        origins.extend(access_term_origins(effective, rule, term));
    }
    origins
}

fn access_term_origins(
    effective: &EffectiveConfiguration,
    rule: &super::AccessRule,
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

fn lower_blocker_diagnostic(
    kind: SemanticBlockerKind,
    effective: &EffectiveConfiguration,
    graph: &SourceGraph,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        blocker_code(kind),
        Severity::Error,
        DiagnosticStage::Lower,
        blocker_message(kind),
    );
    if let Some(origin) = effective.ledger.decisions.iter().find_map(|decision| {
        matches!(
            decision.outcome,
            DecisionOutcome::Classified {
                activation: Activation::Blocked(blocked),
                ..
            } if blocked == kind
        )
        .then_some(&decision.origin)
    }) {
        diagnostic = diagnostic
            .with_primary_span(origin.directive_span)
            .with_include_stack(
                origin
                    .provenance
                    .include_stack
                    .iter()
                    .map(|frame| frame.directive_span),
            );
    } else if let Some(root) = graph.root.and_then(|source| graph.source(source)) {
        diagnostic = diagnostic.with_primary_span(root.source.full_span());
    }
    diagnostic
}

fn lower_authentication(schemes: &[AuthenticationScheme]) -> Result<Option<ForwardProxyAuth>, ()> {
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

fn lower_access_policy(effective: &EffectiveConfiguration) -> Option<ForwardAccessPolicy> {
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

const fn blocker_code(kind: SemanticBlockerKind) -> DiagnosticCode {
    match kind {
        SemanticBlockerKind::InvalidForm => super::E_UNSUPPORTED_FORM,
        SemanticBlockerKind::UnknownDirective => super::E_UNKNOWN_DIRECTIVE,
        SemanticBlockerKind::ConflictingAclType => E_DUPLICATE_IDENTITY,
        SemanticBlockerKind::UnresolvedAclReference => E_UNRESOLVED_REFERENCE,
        SemanticBlockerKind::IncludeExpansion
        | SemanticBlockerKind::UnsupportedPortOption
        | SemanticBlockerKind::UnsupportedAclType => E_UNSUPPORTED_FEATURE,
        _ => E_SEMANTICS_NOT_REPRESENTABLE,
    }
}

const fn is_schema_blocker(kind: SemanticBlockerKind) -> bool {
    matches!(
        kind,
        SemanticBlockerKind::ForwardProxyListener
            | SemanticBlockerKind::SourceAddressAcl
            | SemanticBlockerKind::DestinationPortAcl
            | SemanticBlockerKind::ProxyAuthenticationAcl
            | SemanticBlockerKind::OrderedHttpAccess
            | SemanticBlockerKind::HeaderAccessPolicy
            | SemanticBlockerKind::DirectRoutingPolicy
            | SemanticBlockerKind::CacheAccessPolicy
            | SemanticBlockerKind::CachePeerHierarchy
            | SemanticBlockerKind::RefreshPolicy
            | SemanticBlockerKind::CachePolicy
            | SemanticBlockerKind::StoragePolicy
            | SemanticBlockerKind::ProxyAuthentication
            | SemanticBlockerKind::AccessLoggingPolicy
            | SemanticBlockerKind::LoggingPolicy
            | SemanticBlockerKind::ResolverPolicy
            | SemanticBlockerKind::ForwardedForPolicy
            | SemanticBlockerKind::ViaPolicy
            | SemanticBlockerKind::HeaderPrivacyPolicy
    )
}

const fn blocker_message(kind: SemanticBlockerKind) -> &'static str {
    match kind {
        SemanticBlockerKind::IncludeExpansion => {
            "Squid include expansion did not produce a complete source graph"
        }
        SemanticBlockerKind::ForwardProxyListener => {
            "Squid forward-proxy listener semantics lack a canonical capability"
        }
        SemanticBlockerKind::SourceAddressAcl => {
            "Squid source-address ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::DestinationPortAcl => {
            "Squid destination-port ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::ProxyAuthenticationAcl => {
            "Squid proxy-authentication ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::OrderedHttpAccess => {
            "Squid ordered first-match HTTP access semantics lack a canonical capability"
        }
        SemanticBlockerKind::HeaderAccessPolicy => {
            "Squid header access semantics lack a canonical capability"
        }
        SemanticBlockerKind::DirectRoutingPolicy => {
            "Squid direct-routing semantics lack a canonical capability"
        }
        SemanticBlockerKind::CacheAccessPolicy => {
            "Squid cache access semantics lack a canonical capability"
        }
        SemanticBlockerKind::CachePeerHierarchy => {
            "Squid cache-peer hierarchy semantics lack a canonical capability"
        }
        SemanticBlockerKind::RefreshPolicy => {
            "Squid ordered refresh semantics lack a canonical capability"
        }
        SemanticBlockerKind::CachePolicy => {
            "Squid cache policy semantics lack a canonical capability"
        }
        SemanticBlockerKind::StoragePolicy => "Squid storage semantics lack a canonical capability",
        SemanticBlockerKind::ProxyAuthentication => {
            "Squid proxy authentication semantics lack a canonical capability"
        }
        SemanticBlockerKind::AccessLoggingPolicy => {
            "Squid access logging semantics lack a canonical capability"
        }
        SemanticBlockerKind::LoggingPolicy => "Squid logging semantics lack a canonical capability",
        SemanticBlockerKind::ResolverPolicy => {
            "Squid resolver selection semantics lack a canonical capability"
        }
        SemanticBlockerKind::ForwardedForPolicy => {
            "Squid forwarded-for privacy semantics lack a canonical capability"
        }
        SemanticBlockerKind::ViaPolicy => "Squid Via header semantics lack a canonical capability",
        SemanticBlockerKind::HeaderPrivacyPolicy => {
            "Squid header privacy semantics lack a canonical capability"
        }
        SemanticBlockerKind::UnsupportedPortOption => "Squid port option is not represented",
        SemanticBlockerKind::UnsupportedAclType => "Squid ACL type is not represented",
        SemanticBlockerKind::ConflictingAclType => {
            "same-name Squid ACL declarations use conflicting types"
        }
        SemanticBlockerKind::UnresolvedAclReference => {
            "Squid access rule references an unresolved ACL"
        }
        SemanticBlockerKind::InvalidForm => "Squid directive form is invalid",
        SemanticBlockerKind::UnknownDirective => "Squid directive is unknown",
    }
}
