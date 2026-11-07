#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn append_default_access_log_overlay(
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

pub(super) fn append_recording_root_overlay(
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

pub(super) fn append_default_error_page_overlay(
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

pub(super) fn reject_duplicate_overlay_keys(
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
pub(super) fn append_supplied_overlays(
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

pub(super) fn append_host_timezone_overlays(
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

pub(super) fn reject_unsatisfied_overlays(
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
