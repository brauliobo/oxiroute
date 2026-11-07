#[allow(clippy::wildcard_imports)]
use super::*;

impl ImportReportEnvelope {
    /// Builds the shared evidence envelope for a complete nginx root import.
    #[must_use]
    pub fn from_nginx(report: &crate::nginx::NginxImportReport) -> Self {
        let candidate = candidate_evidence(&report.candidate, nginx_origin);
        let blockers = nginx_blockers(report);
        Self::assemble(
            source_metadata("nginx", "nginx-root"),
            nginx_graph(&report.source_graph),
            source_metadata_evidence(&graph_source_metadata(
                report
                    .source_graph
                    .sources
                    .iter()
                    .map(|source| &source.source),
            )),
            candidate,
            shared_requirements(&report.candidate, nginx_origin),
            shared_overlays(&report.candidate, nginx_origin),
            &report.diagnostics,
            blockers,
        )
    }

    /// Builds the shared evidence envelope for an ordered `HAProxy` root import.
    #[must_use]
    pub fn from_haproxy<P: AsRef<Path>>(
        report: &Report<crate::haproxy::CanonicalCandidate>,
        roots: &[P],
    ) -> Self {
        let candidate = report.value();
        Self::assemble(
            source_metadata("haproxy", "haproxy-strict"),
            haproxy_graph(&candidate.source_metadata, roots),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, haproxy_origin),
            shared_requirements(candidate, haproxy_origin),
            shared_overlays(candidate, haproxy_origin),
            report.diagnostics(),
            Vec::new(),
        )
    }

    /// Builds the shared evidence envelope for a Squid forward-proxy import.
    #[cfg(unix)]
    #[must_use]
    pub fn from_squid(report: &crate::squid::ImportReport) -> Self {
        let candidate = candidate_evidence(&report.candidate, squid_origin);
        let mut envelope = Self::assemble(
            source_metadata_with_version(
                "squid",
                crate::squid::SQUID_CAPABILITY_PROFILE_ID,
                crate::squid::SQUID_CAPABILITY_PROFILE_VERSION,
                None,
                None,
            ),
            squid_graph(&report.source_graph),
            source_metadata_evidence(&report.candidate.source_metadata),
            candidate,
            RequirementsEvidence::default(),
            Vec::new(),
            &report.diagnostics,
            squid_blockers(report),
        );
        envelope.capabilities = Some(report.capabilities);
        envelope
    }

    /// Builds the shared evidence envelope for a static Apache httpd import.
    #[must_use]
    pub fn from_apache(report: &crate::apache::ApacheImportReport) -> Self {
        let candidate = &report.candidate;
        Self::assemble(
            source_metadata("apache", "apache-static-reverse-proxy"),
            apache_graph(&report.source_graph),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, apache_origin),
            shared_requirements(candidate, apache_origin),
            shared_overlays(candidate, apache_origin),
            &report.diagnostics,
            apache_blockers(report),
        )
    }

    /// Builds the shared evidence envelope for an exact Varnish VCL import.
    #[must_use]
    pub fn from_varnish(report: &crate::varnish::ImportReport) -> Self {
        let candidate = &report.candidate;
        Self::assemble(
            varnish_source_metadata(report),
            varnish_graph(&report.source_graph),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, varnish_origin),
            shared_requirements(candidate, varnish_origin),
            shared_overlays(candidate, varnish_origin),
            &report.diagnostics,
            varnish_blockers(report),
        )
    }

    /// Serializes the envelope as one stable compact JSON document.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the canonical candidate contains a value that JSON cannot
    /// represent, such as a non-UTF-8 path.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serializes the envelope as one newline-terminated JSON document for CLI output.
    ///
    /// # Errors
    ///
    /// Returns the serialization error from [`Self::to_json`].
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut json = self.to_json()?;
        json.push('\n');
        Ok(json)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the envelope keeps independent evidence sections in one stable constructor"
    )]
    fn assemble(
        source: ImportSourceMetadata,
        source_graph: SourceGraphEvidence,
        source_metadata: SourceMetadataEvidence,
        candidate: CandidateEvidence,
        requirements: RequirementsEvidence,
        overlays: Vec<OverlayEvidence>,
        diagnostics: &[Diagnostic],
        typed_blockers: Vec<ImportBlocker>,
    ) -> Self {
        let sorted = sorted_diagnostics(diagnostics);
        let mut blockers = diagnostic_blockers(&sorted);
        blockers.extend(typed_blockers);
        blockers.sort_by(|left, right| {
            (
                left.kind.as_str(),
                left.scope.as_deref().unwrap_or_default(),
                left.code.as_str(),
                left.id.as_str(),
            )
                .cmp(&(
                    right.kind.as_str(),
                    right.scope.as_deref().unwrap_or_default(),
                    right.code.as_str(),
                    right.id.as_str(),
                ))
        });
        Self {
            schema_version: IMPORT_REPORT_SCHEMA_VERSION,
            source,
            source_graph,
            source_metadata,
            candidate,
            blockers,
            requirements,
            overlays,
            diagnostics: sorted.iter().map(diagnostic_evidence).collect(),
            #[cfg(unix)]
            capabilities: None,
        }
    }
}

fn source_metadata(product: &str, profile: &str) -> ImportSourceMetadata {
    source_metadata_with_version(product, profile, 1, None, None)
}

fn source_metadata_with_version(
    product: &str,
    profile: &str,
    profile_version: u32,
    version: Option<String>,
    version_source: Option<String>,
) -> ImportSourceMetadata {
    ImportSourceMetadata {
        product: product.into(),
        version,
        version_source,
        capability_profile: CapabilityProfileMetadata {
            id: profile.into(),
            version: profile_version,
        },
    }
}

fn varnish_source_metadata(report: &crate::varnish::ImportReport) -> ImportSourceMetadata {
    let version = report.declarations.iter().find_map(|declaration| {
        declaration
            .version
            .effective
            .as_ref()
            .and_then(|version| String::from_utf8(version.as_bytes().to_vec()).ok())
            .map(|version| (version, declaration.version.origin))
    });
    source_metadata_with_version(
        "varnish",
        crate::varnish::VARNISH_CAPABILITY_PROFILE_ID,
        crate::varnish::VARNISH_CAPABILITY_PROFILE_VERSION,
        version.as_ref().map(|(version, _)| version.clone()),
        version.map(|(_, origin)| snake_case(&format!("{origin:?}"))),
    )
}

fn candidate_evidence<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> CandidateEvidence {
    candidate_evidence_parts(
        candidate.summary(),
        candidate.validated(),
        &candidate.provenance,
        origin,
    )
}

fn candidate_evidence_parts<O>(
    summary: CanonicalCandidateSummary,
    config: Option<&oxiroute_config::ValidatedConfig>,
    provenance: &[CanonicalProvenance<O>],
    origin: impl Fn(&O) -> OriginEvidence,
) -> CandidateEvidence {
    CandidateEvidence {
        finalized: config.is_some(),
        config: config.cloned(),
        draft: draft_evidence(summary),
        provenance: provenance
            .iter()
            .map(|entry| CanonicalProvenanceEvidence {
                path: entry.path.clone(),
                origins: entry.origins.iter().map(&origin).collect(),
            })
            .collect(),
    }
}

fn draft_evidence(summary: CanonicalCandidateSummary) -> CandidateDraftEvidence {
    CandidateDraftEvidence {
        version: summary.version,
        max_connections: summary.max_connections,
        management: summary.management,
        stats: summary.stats,
        certificates: summary.certificates,
        tls_profiles: summary.tls_profiles,
        listeners: summary.listeners,
        upstream_pools: summary.upstream_pools,
        http_services: summary.http_services,
        cache_stores: summary.cache_stores,
        forward_proxy_services: summary.forward_proxy_services,
        rtmp_services: summary.rtmp_services,
        l4_services: summary.l4_services,
    }
}

fn shared_requirements<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> RequirementsEvidence {
    RequirementsEvidence {
        deployment: candidate
            .deployment_requirements
            .iter()
            .map(|requirement| deployment_requirement(requirement, &origin))
            .collect(),
        activation: candidate
            .activation_requirements
            .iter()
            .map(|requirement| activation_requirement(requirement, &origin))
            .collect(),
    }
}

fn deployment_requirement<O>(
    requirement: &DeploymentRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> RequirementEvidence {
    RequirementEvidence {
        kind: snake_case(&format!("{:?}", requirement.kind)),
        directive: requirement.directive.clone(),
        values: requirement.value.clone(),
        origins: vec![origin(&requirement.origin)],
        equivalent_runtime_endpoint: None,
    }
}

fn activation_requirement<O>(
    requirement: &ActivationRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> RequirementEvidence {
    RequirementEvidence {
        kind: snake_case(&format!("{:?}", requirement.kind)),
        directive: requirement.directive.clone(),
        values: Vec::new(),
        origins: vec![origin(&requirement.origin)],
        equivalent_runtime_endpoint: Some(requirement.equivalent_runtime_endpoint),
    }
}

fn shared_overlays<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> Vec<OverlayEvidence> {
    candidate
        .operational_overlays
        .iter()
        .map(|overlay| overlay_evidence(overlay, &origin))
        .collect()
}

fn overlay_evidence<O>(
    overlay: &OperationalOverlayRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> OverlayEvidence {
    OverlayEvidence {
        id: overlay.id.clone(),
        kind: snake_case(&format!("{:?}", overlay.kind)),
        origin: overlay.origin.as_ref().map(origin),
        redacted_evidence: overlay.redacted_evidence,
        values: overlay.values.clone(),
        satisfied: overlay.satisfied,
    }
}
