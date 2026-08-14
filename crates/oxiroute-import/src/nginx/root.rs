use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use oxiroute_config::{ConfigDraft, RtmpRecorderTimezone, UpstreamTls};

use crate::{
    ActivationRequirement, ActivationRequirementKind, CanonicalCandidate, CanonicalProvenance,
    DeploymentRequirement, DeploymentRequirementKind, Diagnostic, DiagnosticStage,
    E_DUPLICATE_IDENTITY, E_INVALID_VALUE, E_UNSUPPORTED_FEATURE, OperationalOverlayKind,
    OperationalOverlayRequirement, Report, Severity, SourceImportMetadata,
    candidate::{CanonicalCandidateState, finalize_candidate},
};

use super::{
    BlockedRtmpService, BlockedService, BlockedStreamService, DirectiveOrigin, ExpandedDirective,
    NginxValue, OccurrenceDecision, OccurrenceDisposition, OccurrenceId, Provenance, SourceGraph,
    Word, load,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxImportReport {
    pub source_graph: SourceGraph,
    pub root_occurrence_ledger: Vec<RootOccurrenceDecision>,
    pub http_occurrence_ledger: Vec<OccurrenceDecision>,
    pub rtmp_occurrence_ledger: Vec<OccurrenceDecision>,
    pub stream_occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub blocked_http_services: Vec<BlockedService>,
    pub blocked_rtmp_services: Vec<BlockedRtmpService>,
    pub blocked_stream_services: Vec<BlockedStreamService>,
    pub candidate: CanonicalCandidate<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootOccurrenceDecision {
    pub occurrence: OccurrenceId,
    pub parent: Option<OccurrenceId>,
    pub name: NginxValue,
    pub arguments: Vec<NginxValue>,
    pub provenance: Provenance,
    pub disposition: RootOccurrenceDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootOccurrenceDisposition {
    Http,
    Rtmp,
    Stream,
    Deployment(Vec<DeploymentRequirementKind>),
    Activation(Vec<ActivationRequirementKind>),
    OperationalOverlay(Vec<OperationalOverlayKind>),
    Structural,
    Blocking(crate::DiagnosticCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxUpstreamTlsOverlay {
    pub authority: String,
    pub tls: UpstreamTls,
    pub require_connectivity_activation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxBearerTokenOverlay {
    pub server_name: String,
    pub token_file_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxHostTimezoneOverlay {
    pub timezone: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxDefaultAccessLogOverlay {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxRecordingRootOverlay {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxDefaultErrorPageOverlay {
    pub server: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NginxImportOptions {
    pub upstream_tls: Vec<NginxUpstreamTlsOverlay>,
    pub bearer_tokens: Vec<NginxBearerTokenOverlay>,
    pub host_timezones: Vec<NginxHostTimezoneOverlay>,
    pub default_access_log: Option<NginxDefaultAccessLogOverlay>,
    pub recording_root: Option<NginxRecordingRootOverlay>,
    pub default_error_page: Option<NginxDefaultErrorPageOverlay>,
    pub x_accel_controls_absent: bool,
}

#[derive(Default)]
struct OverlayOrigins {
    upstream_tls: HashMap<Vec<u8>, DirectiveOrigin>,
    bearer_tokens: HashMap<Vec<u8>, DirectiveOrigin>,
}

impl NginxImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

/// Imports one complete nginx root through a single include graph.
#[must_use]
pub fn import_root(root: &Path, root_prefix: &Path) -> NginxImportReport {
    import_root_with_options(root, root_prefix, &NginxImportOptions::default())
}

/// Imports a complete root with explicit security overlays for native policy that is not safe to
/// infer from an IP-only authority.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "complete-root lowering and evidence finalization must share one consistent result"
)]
pub fn import_root_with_options(
    root: &Path,
    root_prefix: &Path,
    options: &NginxImportOptions,
) -> NginxImportReport {
    let (graph, mut diagnostics) = load(root, root_prefix).into_parts();
    let upstream_tls_counts = value_counts(
        options
            .upstream_tls
            .iter()
            .map(|overlay| overlay.authority.as_bytes().to_ascii_lowercase()),
    );
    let bearer_token_counts = value_counts(
        options
            .bearer_tokens
            .iter()
            .map(|overlay| overlay.server_name.as_bytes().to_ascii_lowercase()),
    );
    reject_duplicate_overlay_keys(
        &upstream_tls_counts,
        "upstream TLS authority",
        &mut diagnostics,
    );
    reject_duplicate_overlay_keys(
        &bearer_token_counts,
        "bearer-token server name",
        &mut diagnostics,
    );
    let upstream_tls = options
        .upstream_tls
        .iter()
        .map(|overlay| {
            (
                overlay.authority.as_bytes().to_ascii_lowercase(),
                overlay.tls.clone(),
            )
        })
        .collect();
    let bearer_tokens = options
        .bearer_tokens
        .iter()
        .map(|overlay| {
            (
                overlay.server_name.as_bytes().to_ascii_lowercase(),
                overlay.token_file_path.clone(),
            )
        })
        .collect();
    let http = super::lower::lower_http_root_with_overlays(
        Report::new(graph.clone(), Vec::new()),
        upstream_tls,
        bearer_tokens,
        options
            .default_access_log
            .as_ref()
            .map(|overlay| overlay.path.clone()),
        options
            .default_error_page
            .as_ref()
            .map(|overlay| overlay.server.clone()),
        options.x_accel_controls_absent,
    );
    if options.host_timezones.len() > 1 {
        diagnostics.push(Diagnostic::new(
            E_DUPLICATE_IDENTITY,
            Severity::Error,
            DiagnosticStage::Lower,
            "nginx host timezone overlay must have one unique host-wide value",
        ));
    }
    let rtmp = super::rtmp_lower::lower_rtmp_root(
        Report::new(graph.clone(), Vec::new()),
        options
            .host_timezones
            .first()
            .filter(|_| options.host_timezones.len() == 1)
            .map(|overlay| overlay.timezone.as_str()),
        options
            .recording_root
            .as_ref()
            .map(|overlay| overlay.path.as_path()),
    );
    let stream = super::stream_lower::lower_stream_root(Report::new(graph.clone(), Vec::new()));

    diagnostics.extend(http.diagnostics.iter().cloned());
    diagnostics.extend(rtmp.diagnostics.iter().cloned());
    diagnostics.extend(stream.diagnostics.iter().cloned());
    let mut deployment_requirements = Vec::new();
    let mut activation_requirements = Vec::<ActivationRequirement<DirectiveOrigin>>::new();
    let mut operational_overlays = Vec::new();
    let mut overlay_origins = OverlayOrigins::default();
    scan_root(
        &graph,
        &mut deployment_requirements,
        &mut operational_overlays,
        &mut activation_requirements,
        options,
        &http.used_upstream_tls_overlays,
        &http.used_bearer_token_overlays,
        &http.used_certificate_overlays,
        &http.used_htpasswd_overlays,
        &mut overlay_origins,
        &mut diagnostics,
    );
    append_supplied_overlays(
        options,
        &upstream_tls_counts,
        &bearer_token_counts,
        &http.used_upstream_tls_overlays,
        &http.used_bearer_token_overlays,
        &overlay_origins,
        &mut operational_overlays,
        &mut diagnostics,
    );
    append_host_timezone_overlays(
        options,
        rtmp.draft(),
        &mut operational_overlays,
        &mut diagnostics,
    );
    append_default_access_log_overlay(
        options,
        http.used_default_access_log_overlay,
        &mut operational_overlays,
        &mut diagnostics,
    );
    append_recording_root_overlay(
        options,
        rtmp.used_recording_root_overlay,
        &mut operational_overlays,
        &mut diagnostics,
    );
    append_default_error_page_overlay(
        options,
        http.used_default_error_overlay,
        &mut operational_overlays,
        &mut diagnostics,
    );
    reject_unsatisfied_overlays(&operational_overlays, &mut diagnostics);

    let http_listener_count = http.draft().listeners.len();
    let rtmp_listener_count = rtmp.draft().listeners.len();
    let http_pool_count = http.draft().upstream_pools.len();
    let http_l4_count = http.draft().l4_services.len();
    let mut draft = http.draft().clone();
    merge_draft(&mut draft, rtmp.draft().clone());
    merge_draft(&mut draft, stream.draft().clone());
    let mut provenance = http.provenance.clone();
    provenance.extend(
        rtmp.provenance
            .iter()
            .cloned()
            .map(|entry| rebase_listener_provenance(entry, http_listener_count)),
    );
    provenance.extend(stream.provenance.iter().cloned().map(|entry| {
        rebase_stream_provenance(
            entry,
            http_listener_count + rtmp_listener_count,
            http_pool_count,
            http_l4_count,
        )
    }));

    deduplicate_diagnostics(&mut diagnostics);
    let blocked_http_services = http.blocked_services;
    let blocked_rtmp_services = rtmp.blocked_services;
    let blocked_stream_services = stream.blocked_services;
    let state = finalize(
        draft,
        &provenance,
        &mut diagnostics,
        blocked_http_services.is_empty()
            && blocked_rtmp_services.is_empty()
            && blocked_stream_services.is_empty()
            && operational_overlays.iter().all(|overlay| overlay.satisfied),
    );
    let root_occurrence_ledger = root_occurrence_ledger(
        &graph,
        &http.occurrence_ledger,
        &rtmp.occurrence_ledger,
        &stream.occurrence_ledger,
        &deployment_requirements,
        &activation_requirements,
        &operational_overlays,
        &diagnostics,
    );

    NginxImportReport {
        source_graph: graph,
        root_occurrence_ledger,
        http_occurrence_ledger: http.occurrence_ledger,
        rtmp_occurrence_ledger: rtmp.occurrence_ledger,
        stream_occurrence_ledger: stream.occurrence_ledger,
        diagnostics,
        blocked_http_services,
        blocked_rtmp_services,
        blocked_stream_services,
        candidate: CanonicalCandidate::new(
            state,
            provenance,
            deployment_requirements,
            activation_requirements,
            operational_overlays,
            SourceImportMetadata::default(),
        ),
    }
}

fn append_default_access_log_overlay(
    options: &NginxImportOptions,
    used: bool,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(overlay) = &options.default_access_log else {
        return;
    };
    overlays.push(OperationalOverlayRequirement {
        id: "nginx-default-access-log-migration".into(),
        kind: OperationalOverlayKind::StructuredAccessLogMigration,
        origin: None,
        redacted_evidence: false,
        values: vec![format!("path={}", overlay.path.display())],
        satisfied: used,
    });
    if !used {
        diagnostics.push(Diagnostic::new(
            E_INVALID_VALUE,
            Severity::Error,
            DiagnosticStage::Lower,
            "nginx default access-log migration overlay matches no uniquely lowerable omitted access_log policy",
        ));
    }
}

fn append_recording_root_overlay(
    options: &NginxImportOptions,
    used: bool,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(overlay) = &options.recording_root else {
        return;
    };
    overlays.push(OperationalOverlayRequirement {
        id: "nginx-recording-root-migration".into(),
        kind: OperationalOverlayKind::RecordingRootMigration,
        origin: None,
        redacted_evidence: false,
        values: vec![format!("path={}", overlay.path.display())],
        satisfied: used,
    });
    if !used {
        diagnostics.push(Diagnostic::new(
            E_INVALID_VALUE,
            Severity::Error,
            DiagnosticStage::Lower,
            "nginx recording-root migration requires exactly one lowerable native recording root",
        ));
    }
}

fn append_default_error_page_overlay(
    options: &NginxImportOptions,
    used: bool,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(overlay) = &options.default_error_page else {
        return;
    };
    overlays.push(OperationalOverlayRequirement {
        id: "nginx-default-error-page-migration".into(),
        kind: OperationalOverlayKind::DefaultErrorPageMigration,
        origin: None,
        redacted_evidence: false,
        values: vec![format!("server={}", overlay.server)],
        satisfied: used,
    });
    if !used {
        diagnostics.push(Diagnostic::new(
            E_INVALID_VALUE,
            Severity::Error,
            DiagnosticStage::Lower,
            "nginx default error-page migration overlay matches no lowerable static 404 policy",
        ));
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "root occurrence dispositions reconcile all importer ledgers and evidence"
)]
fn root_occurrence_ledger(
    graph: &SourceGraph,
    http: &[OccurrenceDecision],
    rtmp: &[OccurrenceDecision],
    stream: &[OccurrenceDecision],
    deployment: &[DeploymentRequirement<DirectiveOrigin>],
    activation: &[ActivationRequirement<DirectiveOrigin>],
    overlays: &[OperationalOverlayRequirement<DirectiveOrigin>],
    diagnostics: &[Diagnostic],
) -> Vec<RootOccurrenceDecision> {
    graph
        .expanded_occurrences
        .iter()
        .map(|occurrence| {
            let http_disposition = http
                .iter()
                .find(|decision| decision.occurrence == occurrence.id)
                .map(|decision| decision.disposition);
            let rtmp_disposition = rtmp
                .iter()
                .find(|decision| decision.occurrence == occurrence.id)
                .map(|decision| decision.disposition);
            let stream_disposition = stream
                .iter()
                .find(|decision| decision.occurrence == occurrence.id)
                .map(|decision| decision.disposition);
            let mut disposition = match (http_disposition, rtmp_disposition, stream_disposition) {
                (Some(OccurrenceDisposition::Blocking(code)), _, _)
                | (_, Some(OccurrenceDisposition::Blocking(code)), _)
                | (_, _, Some(OccurrenceDisposition::Blocking(code))) => {
                    RootOccurrenceDisposition::Blocking(code)
                }
                (Some(OccurrenceDisposition::Resolved), _, _) => RootOccurrenceDisposition::Http,
                (_, Some(OccurrenceDisposition::Resolved), _) => RootOccurrenceDisposition::Rtmp,
                (_, _, Some(OccurrenceDisposition::Resolved)) => RootOccurrenceDisposition::Stream,
                _ => RootOccurrenceDisposition::Structural,
            };
            let deployment_kinds = deployment
                .iter()
                .filter(|requirement| requirement.origin.occurrence == occurrence.id)
                .map(|requirement| requirement.kind)
                .collect::<Vec<_>>();
            if !deployment_kinds.is_empty() {
                disposition = RootOccurrenceDisposition::Deployment(deployment_kinds);
            }
            let overlay_kinds = overlays
                .iter()
                .filter(|overlay| {
                    overlay
                        .origin
                        .as_ref()
                        .is_some_and(|origin| origin.occurrence == occurrence.id)
                })
                .map(|overlay| overlay.kind)
                .collect::<Vec<_>>();
            if !overlay_kinds.is_empty() {
                disposition = RootOccurrenceDisposition::OperationalOverlay(overlay_kinds);
            }
            let activation_kinds = activation
                .iter()
                .filter(|requirement| requirement.origin.occurrence == occurrence.id)
                .map(|requirement| requirement.kind)
                .collect::<Vec<_>>();
            if !activation_kinds.is_empty() {
                disposition = RootOccurrenceDisposition::Activation(activation_kinds);
            }
            if let Some(code) = diagnostics.iter().find_map(|diagnostic| {
                (diagnostic.severity() == Severity::Error
                    && diagnostic.primary_span().is_some_and(|span| {
                        span.source() == occurrence.directive.span.source()
                            && occurrence
                                .directive
                                .span
                                .range()
                                .contains(span.range().start())
                            && span.range().end() <= occurrence.directive.span.range().end()
                    }))
                .then(|| diagnostic.code())
            }) {
                disposition = RootOccurrenceDisposition::Blocking(code);
            }
            RootOccurrenceDecision {
                occurrence: occurrence.id,
                parent: occurrence.parent,
                name: root_value(graph, &occurrence.directive.name),
                arguments: occurrence
                    .directive
                    .arguments
                    .iter()
                    .map(|argument| root_value(graph, argument))
                    .collect(),
                provenance: occurrence.provenance.clone(),
                disposition,
            }
        })
        .collect()
}

fn root_value(graph: &SourceGraph, word: &Word) -> NginxValue {
    let raw = graph
        .source(word.span.source())
        .and_then(|source| source.source.slice(word.span.range()))
        .map_or_else(|| word.value.clone(), <[u8]>::to_vec);
    NginxValue {
        value: word.value.clone(),
        raw,
        span: word.span,
    }
}

fn value_counts(values: impl IntoIterator<Item = Vec<u8>>) -> HashMap<Vec<u8>, usize> {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    counts
}

fn reject_duplicate_overlay_keys(
    counts: &HashMap<Vec<u8>, usize>,
    identity: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (value, count) in counts {
        if *count > 1 {
            diagnostics.push(Diagnostic::new(
                E_DUPLICATE_IDENTITY,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "nginx import options repeat {identity} `{}` {count} times",
                    display_bytes(value)
                ),
            ));
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "supplied overlay identity, use, provenance, and evidence are validated together"
)]
fn append_supplied_overlays(
    options: &NginxImportOptions,
    upstream_tls_counts: &HashMap<Vec<u8>, usize>,
    bearer_token_counts: &HashMap<Vec<u8>, usize>,
    used_upstream_tls: &HashSet<Vec<u8>>,
    used_bearer_tokens: &HashSet<Vec<u8>>,
    origins: &OverlayOrigins,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, overlay) in options.upstream_tls.iter().enumerate() {
        let identity = overlay.authority.as_bytes().to_ascii_lowercase();
        let unique = upstream_tls_counts.get(&identity) == Some(&1);
        let used = used_upstream_tls.contains(&identity);
        if unique && !used {
            diagnostics.push(Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "nginx upstream TLS overlay authority `{}` matches no lowered proxy origin",
                    overlay.authority
                ),
            ));
        }
        let mut values = vec![
            format!("authority={}", overlay.authority),
            format!("server_name={}", overlay.tls.server_name),
        ];
        if let Some(path) = &overlay.tls.ca_certificate_path {
            values.push(format!("ca_certificate_path={}", path.display()));
        }
        values.push(format!(
            "require_connectivity_activation={}",
            overlay.require_connectivity_activation
        ));
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-upstream-tls-option-{index}"),
            kind: OperationalOverlayKind::UpstreamTlsPolicy,
            origin: origins.upstream_tls.get(&identity).cloned(),
            redacted_evidence: false,
            values,
            satisfied: unique && used,
        });
    }
    for (index, overlay) in options.bearer_tokens.iter().enumerate() {
        let identity = overlay.server_name.as_bytes().to_ascii_lowercase();
        let unique = bearer_token_counts.get(&identity) == Some(&1);
        let used = used_bearer_tokens.contains(&identity);
        if unique && !used {
            diagnostics.push(Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "nginx bearer-token overlay server name `{}` matches no lowered authorization rule",
                    overlay.server_name
                ),
            ));
        }
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-bearer-token-option-{index}"),
            kind: OperationalOverlayKind::BearerTokenFile,
            origin: origins.bearer_tokens.get(&identity).cloned(),
            redacted_evidence: true,
            values: vec![
                format!("server_name={}", overlay.server_name),
                format!("token_file_path={}", overlay.token_file_path.display()),
            ],
            satisfied: unique && used,
        });
    }
}

fn append_host_timezone_overlays(
    options: &NginxImportOptions,
    rtmp_draft: &ConfigDraft,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, overlay) in options.host_timezones.iter().enumerate() {
        let used = options.host_timezones.len() == 1
            && rtmp_draft
                .rtmp_services
                .iter()
                .flat_map(|service| &service.applications)
                .flat_map(|application| &application.recorders)
                .any(|recorder| {
                    matches!(
                        &recorder.timezone,
                        RtmpRecorderTimezone::Iana(timezone) if timezone == &overlay.timezone
                    )
                });
        if !used && options.host_timezones.len() == 1 {
            diagnostics.push(Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "nginx host timezone overlay `{}` matches no lowered recording policy",
                    overlay.timezone
                ),
            ));
        }
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-host-timezone-option-{index}"),
            kind: OperationalOverlayKind::HostTimezone,
            origin: None,
            redacted_evidence: false,
            values: vec![format!("timezone={}", overlay.timezone)],
            satisfied: used,
        });
    }
}

fn reject_unsatisfied_overlays(
    overlays: &[OperationalOverlayRequirement<DirectiveOrigin>],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for overlay in overlays.iter().filter(|overlay| !overlay.satisfied) {
        let mut diagnostic = Diagnostic::new(
            E_INVALID_VALUE,
            Severity::Error,
            DiagnosticStage::Resolve,
            format!(
                "nginx operational overlay `{}` was not uniquely matched and consumed",
                overlay.id
            ),
        );
        if let Some(origin) = &overlay.origin {
            diagnostic = diagnostic.with_primary_span(origin.span);
        }
        diagnostics.push(diagnostic);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "root evidence and each lowering-consumption set are reconciled together"
)]
fn scan_root(
    graph: &SourceGraph,
    requirements: &mut Vec<DeploymentRequirement<DirectiveOrigin>>,
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    activation: &mut Vec<ActivationRequirement<DirectiveOrigin>>,
    options: &NginxImportOptions,
    used_upstream_tls: &HashSet<Vec<u8>>,
    used_bearer_tokens: &HashSet<Vec<u8>>,
    used_certificates: &HashSet<OccurrenceId>,
    used_htpasswd: &HashSet<OccurrenceId>,
    overlay_origins: &mut OverlayOrigins,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for directive in &graph.expanded_directives {
        scan_overlays(
            directive,
            &[],
            overlays,
            activation,
            options,
            used_upstream_tls,
            used_bearer_tokens,
            used_certificates,
            used_htpasswd,
            overlay_origins,
        );
        match directive.directive.name.value.as_slice() {
            b"http" | b"rtmp" | b"stream" | b"include" => {}
            b"events" => scan_events(directive, requirements, diagnostics),
            b"user" => {
                push_requirement(
                    requirements,
                    directive,
                    DeploymentRequirementKind::ProcessUser,
                );
                if directive.directive.arguments.len() > 1 {
                    let mut group = requirement(directive, DeploymentRequirementKind::ProcessGroup);
                    group.value = vec![display_bytes(&directive.directive.arguments[1].value)];
                    requirements.push(group);
                }
            }
            b"worker_processes" | b"worker_cpu_affinity" | b"worker_priority" => {
                push_requirement(
                    requirements,
                    directive,
                    DeploymentRequirementKind::WorkerModel,
                );
            }
            b"worker_rlimit_nofile" => push_requirement(
                requirements,
                directive,
                DeploymentRequirementKind::EventCapacity,
            ),
            b"load_module" => push_requirement(
                requirements,
                directive,
                DeploymentRequirementKind::ModuleLoad,
            ),
            b"error_log" => push_requirement(
                requirements,
                directive,
                DeploymentRequirementKind::ErrorLogging,
            ),
            b"daemon" | b"master_process" | b"pid" => push_requirement(
                requirements,
                directive,
                DeploymentRequirementKind::Daemonization,
            ),
            _ => diagnostics.push(
                Diagnostic::new(
                    E_UNSUPPORTED_FEATURE,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "unsupported directive `{}` in complete nginx root",
                        display_bytes(&directive.directive.name.value)
                    ),
                )
                .with_primary_span(directive.directive.span)
                .with_include_stack(
                    directive
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
            ),
        }
    }
}

fn scan_events(
    events: &ExpandedDirective,
    requirements: &mut Vec<DeploymentRequirement<DirectiveOrigin>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !events.directive.arguments.is_empty() || events.children.is_none() {
        diagnostics.push(
            Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Resolve,
                "nginx events must be a block without arguments",
            )
            .with_primary_span(events.directive.span),
        );
        return;
    }
    for directive in events.children.as_deref().unwrap_or_default() {
        match directive.directive.name.value.as_slice() {
            b"worker_connections" => push_requirement(
                requirements,
                directive,
                DeploymentRequirementKind::EventCapacity,
            ),
            b"use" | b"multi_accept" | b"accept_mutex" | b"accept_mutex_delay" => {
                push_requirement(
                    requirements,
                    directive,
                    DeploymentRequirementKind::WorkerModel,
                );
            }
            _ => diagnostics.push(
                Diagnostic::new(
                    E_UNSUPPORTED_FEATURE,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "unsupported nginx events directive `{}`",
                        display_bytes(&directive.directive.name.value)
                    ),
                )
                .with_primary_span(directive.directive.span),
            ),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "source overlay inventory and each lowering-consumption set are reconciled together"
)]
fn scan_overlays(
    directive: &ExpandedDirective,
    inherited_server_names: &[Vec<u8>],
    overlays: &mut Vec<OperationalOverlayRequirement<DirectiveOrigin>>,
    activation: &mut Vec<ActivationRequirement<DirectiveOrigin>>,
    options: &NginxImportOptions,
    used_upstream_tls: &HashSet<Vec<u8>>,
    used_bearer_tokens: &HashSet<Vec<u8>>,
    used_certificates: &HashSet<OccurrenceId>,
    used_htpasswd: &HashSet<OccurrenceId>,
    overlay_origins: &mut OverlayOrigins,
) {
    let name = directive.directive.name.value.as_slice();
    let server_names = if name == b"server" {
        directive
            .children
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|child| child.directive.name.value == b"server_name")
            .flat_map(|child| &child.directive.arguments)
            .map(|argument| argument.value.to_ascii_lowercase())
            .collect::<Vec<_>>()
    } else {
        inherited_server_names.to_vec()
    };
    if matches!(name, b"ssl_certificate" | b"ssl_certificate_key") {
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-certificate-material-{}", directive.occurrence.get()),
            kind: OperationalOverlayKind::CertificateMaterial,
            origin: Some(origin(directive)),
            redacted_evidence: false,
            values: directive
                .directive
                .arguments
                .iter()
                .map(|argument| display_bytes(&argument.value))
                .collect(),
            satisfied: directive.directive.arguments.len() == 1
                && used_certificates.contains(&directive.occurrence),
        });
    }
    if name == b"auth_basic_user_file" {
        let values = directive
            .directive
            .arguments
            .iter()
            .map(|argument| display_bytes(&argument.value))
            .collect::<Vec<_>>();
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-htpasswd-file-{}", directive.occurrence.get()),
            kind: OperationalOverlayKind::HtpasswdFile,
            origin: Some(origin(directive)),
            redacted_evidence: values.iter().any(|value| value.contains("<redacted>")),
            satisfied: values.len() == 1
                && !values[0].contains("<redacted>")
                && used_htpasswd.contains(&directive.occurrence),
            values,
        });
    }
    let authorization_check = name == b"if"
        && directive.directive.arguments.iter().any(|argument| {
            argument
                .value
                .windows(b"authorization".len())
                .any(|part| part.eq_ignore_ascii_case(b"authorization"))
        });
    let redacted = directive.directive.arguments.iter().any(|argument| {
        argument
            .value
            .windows(b"<redacted>".len())
            .any(|part| part == b"<redacted>")
    });
    let matched_bearer = (authorization_check && redacted)
        .then(|| {
            server_names
                .iter()
                .find(|name| used_bearer_tokens.contains(*name))
        })
        .flatten();
    if let Some(name) = matched_bearer {
        overlay_origins
            .bearer_tokens
            .entry(name.clone())
            .or_insert_with(|| origin(directive));
    }
    if authorization_check && redacted && matched_bearer.is_none() {
        overlays.push(OperationalOverlayRequirement {
            id: format!("nginx-bearer-token-{}", directive.occurrence.get()),
            kind: OperationalOverlayKind::BearerTokenFile,
            origin: Some(origin(directive)),
            redacted_evidence: true,
            values: server_names
                .iter()
                .map(|name| display_bytes(name))
                .collect(),
            satisfied: false,
        });
    }
    if name == b"proxy_pass" && directive.directive.arguments.len() == 1 {
        let proxy_value = &directive.directive.arguments[0].value;
        let value = proxy_value
            .strip_prefix(b"https://")
            .or_else(|| proxy_value.strip_prefix(b"$scheme://"));
        let Some(value) = value else {
            for child in directive.children.as_deref().unwrap_or_default() {
                scan_overlays(
                    child,
                    &server_names,
                    overlays,
                    activation,
                    options,
                    used_upstream_tls,
                    used_bearer_tokens,
                    used_certificates,
                    used_htpasswd,
                    overlay_origins,
                );
            }
            return;
        };
        let authority_end = value
            .iter()
            .position(|byte| matches!(*byte, b'/' | b'?' | b'#'))
            .unwrap_or(value.len());
        let authority = &value[..authority_end];
        let host = authority
            .strip_prefix(b"[")
            .and_then(|value| value.split(|byte| *byte == b']').next())
            .unwrap_or_else(|| {
                authority
                    .split(|byte| *byte == b':')
                    .next()
                    .unwrap_or(authority)
            });
        let ip_only = std::str::from_utf8(host)
            .ok()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some();
        let normalized_authority = authority.to_ascii_lowercase();
        let supplied = used_upstream_tls.contains(&normalized_authority);
        if supplied {
            overlay_origins
                .upstream_tls
                .entry(normalized_authority.clone())
                .or_insert_with(|| origin(directive));
        }
        if let Some(overlay) = options.upstream_tls.iter().find(|overlay| {
            overlay.authority.as_bytes().eq_ignore_ascii_case(authority)
                && overlay.require_connectivity_activation
                && supplied
        }) {
            activation.push(ActivationRequirement {
                kind: ActivationRequirementKind::UpstreamConnectivity,
                directive: format!("proxy_pass {}", overlay.authority),
                origin: origin(directive),
                equivalent_runtime_endpoint: false,
            });
        }
        if ip_only && !supplied {
            overlays.push(OperationalOverlayRequirement {
                id: format!("nginx-upstream-tls-{}", directive.occurrence.get()),
                kind: OperationalOverlayKind::UpstreamTlsPolicy,
                origin: Some(origin(directive)),
                redacted_evidence: false,
                values: vec![display_bytes(authority)],
                satisfied: false,
            });
        }
    }
    for child in directive.children.as_deref().unwrap_or_default() {
        scan_overlays(
            child,
            &server_names,
            overlays,
            activation,
            options,
            used_upstream_tls,
            used_bearer_tokens,
            used_certificates,
            used_htpasswd,
            overlay_origins,
        );
    }
}

fn push_requirement(
    requirements: &mut Vec<DeploymentRequirement<DirectiveOrigin>>,
    directive: &ExpandedDirective,
    kind: DeploymentRequirementKind,
) {
    requirements.push(requirement(directive, kind));
}

fn requirement(
    directive: &ExpandedDirective,
    kind: DeploymentRequirementKind,
) -> DeploymentRequirement<DirectiveOrigin> {
    DeploymentRequirement {
        kind,
        directive: display_bytes(&directive.directive.name.value),
        value: directive
            .directive
            .arguments
            .iter()
            .map(|argument| display_bytes(&argument.value))
            .collect(),
        origin: origin(directive),
    }
}

fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
    DirectiveOrigin {
        occurrence: directive.occurrence,
        span: directive.directive.span,
        provenance: directive.provenance.clone(),
    }
}

fn merge_draft(target: &mut ConfigDraft, source: ConfigDraft) {
    target.certificates.extend(source.certificates);
    target.tls_profiles.extend(source.tls_profiles);
    target.listeners.extend(source.listeners);
    target.upstream_pools.extend(source.upstream_pools);
    target.http_services.extend(source.http_services);
    target.cache_stores.extend(source.cache_stores);
    target
        .forward_proxy_services
        .extend(source.forward_proxy_services);
    target.rtmp_services.extend(source.rtmp_services);
    target.l4_services.extend(source.l4_services);
}

fn rebase_listener_provenance(
    mut provenance: CanonicalProvenance<DirectiveOrigin>,
    offset: usize,
) -> CanonicalProvenance<DirectiveOrigin> {
    let Some(remainder) = provenance.path.strip_prefix("/listeners/") else {
        return provenance;
    };
    let split = remainder.find('/').unwrap_or(remainder.len());
    let Ok(index) = remainder[..split].parse::<usize>() else {
        return provenance;
    };
    provenance.path = format!("/listeners/{}{}", index + offset, &remainder[split..]);
    provenance
}

fn rebase_stream_provenance(
    provenance: CanonicalProvenance<DirectiveOrigin>,
    listener_offset: usize,
    pool_offset: usize,
    l4_offset: usize,
) -> CanonicalProvenance<DirectiveOrigin> {
    let provenance = rebase_indexed_provenance(provenance, "/listeners/", listener_offset);
    let provenance = rebase_indexed_provenance(provenance, "/upstream_pools/", pool_offset);
    rebase_indexed_provenance(provenance, "/l4_services/", l4_offset)
}

fn rebase_indexed_provenance(
    mut provenance: CanonicalProvenance<DirectiveOrigin>,
    prefix: &str,
    offset: usize,
) -> CanonicalProvenance<DirectiveOrigin> {
    let Some(remainder) = provenance.path.strip_prefix(prefix) else {
        return provenance;
    };
    let split = remainder.find('/').unwrap_or(remainder.len());
    let Ok(index) = remainder[..split].parse::<usize>() else {
        return provenance;
    };
    provenance.path = format!("{prefix}{}{}", index + offset, &remainder[split..]);
    provenance
}

fn finalize(
    draft: ConfigDraft,
    provenance: &[CanonicalProvenance<DirectiveOrigin>],
    diagnostics: &mut Vec<Diagnostic>,
    services_complete: bool,
) -> CanonicalCandidateState {
    let eligible = services_complete
        && !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error);
    match finalize_candidate(&draft, eligible) {
        Ok(Some(config)) => CanonicalCandidateState::Validated(config),
        Ok(None) => CanonicalCandidateState::Blocked(draft),
        Err(error) => {
            let mut diagnostic = Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Validate,
                format!("lowered complete nginx configuration is invalid: {error}"),
            );
            if let Some(origin) = provenance.first().and_then(|entry| entry.origins.first()) {
                diagnostic = diagnostic.with_primary_span(origin.span);
            }
            diagnostics.push(diagnostic);
            CanonicalCandidateState::Blocked(draft)
        }
    }
}

fn deduplicate_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.primary_span(),
            diagnostic.severity(),
            diagnostic.code(),
            diagnostic.message().to_owned(),
        )
    });
    diagnostics.dedup();
}

fn display_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
