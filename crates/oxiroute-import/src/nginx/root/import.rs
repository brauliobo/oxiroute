#[allow(clippy::wildcard_imports)]
use super::*;

/// Imports one complete nginx root through a single include graph.
#[must_use]
pub fn import_root(root: &Path, root_prefix: &Path) -> NginxImportReport {
    import_root_with_options(root, root_prefix, &NginxImportOptions::default())
}

/// Imports a complete root with the operator overlays (upstream TLS, bearer token files, log and error-page
/// migration, timezone, recording root) that native policy cannot supply.
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
    let http = super::super::lower::lower_http_root_with_overlays(
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
    let rtmp = super::super::rtmp_lower::lower_rtmp_root(
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
    let stream =
        super::super::stream_lower::lower_stream_root(Report::new(graph.clone(), Vec::new()));

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

pub(super) fn display_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
