#[allow(clippy::wildcard_imports)]
use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "root evidence and each lowering-consumption set are reconciled together"
)]
pub(super) fn scan_root(
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
    let bearer_gate = name == b"if" && bearer_gate_condition(&directive.directive);
    let matched_bearer = bearer_gate
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
    if bearer_gate && matched_bearer.is_none() {
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
