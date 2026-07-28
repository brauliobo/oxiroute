use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use oxiroute_config::{Config, Protocol, Stats, validate_config};

use crate::{
    Diagnostic, DiagnosticStage, E_INVALID_VALUE, OperationalOverlayKind,
    OperationalOverlayRequirement, Report, Severity,
};

use super::{CanonicalCandidate, Lowerer};

use crate::haproxy::BindAddress;
use crate::haproxy::{
    EffectiveConfiguration, EffectiveFrontend, EffectiveListen, HaproxyImportOptions,
};

/// Lowers an `HAProxy` semantic report into a canonical draft and conditionally finalized config.
#[must_use]
pub(crate) fn lower(
    resolution: &Report<EffectiveConfiguration>,
    options: &HaproxyImportOptions,
) -> Report<CanonicalCandidate> {
    Lowerer::new(resolution, options).run()
}

impl<'a> Lowerer<'a> {
    fn new(
        resolution: &'a Report<EffectiveConfiguration>,
        options: &'a HaproxyImportOptions,
    ) -> Self {
        Self {
            effective: resolution.value(),
            diagnostics: resolution.diagnostics().to_vec(),
            draft: crate::CanonicalDraft::default(),
            provenance: Vec::new(),
            lowered_pools: std::collections::HashSet::new(),
            certificate_names: std::collections::HashMap::new(),
            deployment_requirements: Vec::new(),
            activation_requirements: Vec::new(),
            operational_overlays: Vec::new(),
            options,
            used_connection_lifecycle_overlays: std::collections::HashSet::new(),
        }
    }

    fn run(mut self) -> Report<CanonicalCandidate> {
        self.deployment_requirements
            .clone_from(&self.effective.deployment_requirements);
        self.activation_requirements
            .clone_from(&self.effective.activation_requirements);
        self.lower_operational_stats();
        if let Some(maxconn) = self.effective.global.maxconn.as_ref() {
            if maxconn.value == 0 {
                self.block_value(
                    maxconn,
                    "HAProxy global maxconn zero selects an environment-derived process admission limit",
                );
            } else {
                self.draft.max_connections = Some(maxconn.value);
                self.record(
                    super::provenance::CanonicalPath::root("max_connections"),
                    super::provenance::provenance_sources(&maxconn.provenance),
                );
            }
        }
        for blocker in &self.effective.global.semantic_blockers {
            self.block_provenance(
                &blocker.provenance,
                super::policy::semantic_blocker_message(blocker.value.kind),
            );
        }

        for backend in &self.effective.backends {
            self.lower_pool(&backend.section, &backend.settings, &backend.servers);
        }
        for listen in &self.effective.listens {
            if !(self
                .effective
                .activation_only_sections
                .contains(&listen.section.id)
                && listen.settings.default_backend.is_none()
                && listen.use_backends.is_empty()
                && listen.servers.is_empty()
                && listen.settings.http_request_rules.is_empty())
            {
                self.lower_pool(&listen.section, &listen.settings, &listen.servers);
            }
        }
        for frontend in &self.effective.frontends {
            if !(self
                .effective
                .activation_only_sections
                .contains(&frontend.section.id)
                && frontend.settings.default_backend.is_none()
                && frontend.use_backends.is_empty()
                && frontend.settings.http_request_rules.is_empty())
            {
                self.lower_frontend(frontend);
            }
        }
        for listen in &self.effective.listens {
            if !(self
                .effective
                .activation_only_sections
                .contains(&listen.section.id)
                && listen.settings.default_backend.is_none()
                && listen.use_backends.is_empty()
                && listen.servers.is_empty()
                && listen.settings.http_request_rules.is_empty())
            {
                self.lower_listen(listen);
            }
        }
        self.finish_connection_lifecycle_overlays();

        let draft = self.draft.clone();
        let config = self.finalize(&draft);
        Report::new(
            CanonicalCandidate {
                draft,
                provenance: self.provenance,
                deployment_requirements: self.deployment_requirements,
                activation_requirements: self.activation_requirements,
                operational_overlays: self.operational_overlays,
                source_metadata: crate::SourceImportMetadata::default(),
                config,
            },
            self.diagnostics,
        )
    }

    fn finish_connection_lifecycle_overlays(&mut self) {
        let mut seen = std::collections::HashSet::new();
        for (index, overlay) in self.options.one_request_per_connection.iter().enumerate() {
            let unique = !overlay.backend.is_empty() && seen.insert(overlay.backend.as_str());
            let used = self.used_connection_lifecycle_overlays.contains(&index);
            let satisfied = unique && used;
            self.operational_overlays
                .push(OperationalOverlayRequirement {
                    id: format!("haproxy.one-request-per-connection:{}", overlay.backend),
                    kind: OperationalOverlayKind::OneRequestPerConnection,
                    origin: None,
                    redacted_evidence: false,
                    values: vec![
                        format!("backend={}", overlay.backend),
                        "lifecycle=one_request_per_connection".into(),
                    ],
                    satisfied,
                });
            if !satisfied {
                self.diagnostics.push(Diagnostic::new(
                    E_INVALID_VALUE,
                    Severity::Error,
                    DiagnosticStage::Lower,
                    format!(
                        "HAProxy one-request-per-connection overlay for backend `{}` must have one unique matching connection-sensitive HTTP backend",
                        overlay.backend
                    ),
                ));
            }
        }
    }

    fn finalize(&mut self, draft: &crate::CanonicalDraft) -> Option<Config> {
        if self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
        {
            return None;
        }
        let mut config = draft.to_config();
        if let Err(error) = validate_config(&mut config) {
            let mut diagnostic = Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Validate,
                format!("lowered HAProxy canonical draft is invalid: {error}"),
            );
            if let Some(span) = self
                .provenance
                .first()
                .and_then(|provenance| provenance.origins.first())
                .map(|origin| origin.span)
            {
                diagnostic = diagnostic.with_primary_span(span);
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        Some(config)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "stats migration validation and overlay accounting are one atomic decision"
    )]
    fn lower_operational_stats(&mut self) {
        let overlay_counts = self.options.prometheus_migrations.iter().fold(
            std::collections::HashMap::<&str, usize>::new(),
            |mut counts, overlay| {
                *counts.entry(overlay.section.as_str()).or_default() += 1;
                counts
            },
        );
        let mut binds = Vec::new();
        for overlay in &self.options.prometheus_migrations {
            let matching_frontends = self.effective.frontends.iter().filter(|frontend| {
                self.effective
                    .supported_stats_sections
                    .contains(&frontend.section.id)
                    && frontend.section.name.as_deref() == Some(overlay.section.as_bytes())
                    && frontend.settings.default_backend.is_none()
                    && frontend.use_backends.is_empty()
                    && frontend.settings.http_request_rules.is_empty()
            });
            let matching_listens = self.effective.listens.iter().filter(|listen| {
                self.effective
                    .supported_stats_sections
                    .contains(&listen.section.id)
                    && listen.section.name.as_deref() == Some(overlay.section.as_bytes())
                    && listen.settings.default_backend.is_none()
                    && listen.use_backends.is_empty()
                    && listen.servers.is_empty()
                    && listen.settings.http_request_rules.is_empty()
            });
            let matching_binds = matching_frontends
                .flat_map(|frontend| frontend.binds.iter())
                .chain(matching_listens.flat_map(|listen| listen.binds.iter()))
                .map(|bind| stats_socket(&bind.address.value))
                .collect::<Option<Vec<_>>>();
            let section_matches = self
                .effective
                .frontends
                .iter()
                .filter(|frontend| {
                    self.effective
                        .supported_stats_sections
                        .contains(&frontend.section.id)
                        && frontend.section.name.as_deref() == Some(overlay.section.as_bytes())
                        && frontend.settings.default_backend.is_none()
                        && frontend.use_backends.is_empty()
                        && frontend.settings.http_request_rules.is_empty()
                })
                .count()
                + self
                    .effective
                    .listens
                    .iter()
                    .filter(|listen| {
                        self.effective
                            .supported_stats_sections
                            .contains(&listen.section.id)
                            && listen.section.name.as_deref() == Some(overlay.section.as_bytes())
                            && listen.settings.default_backend.is_none()
                            && listen.use_backends.is_empty()
                            && listen.servers.is_empty()
                            && listen.settings.http_request_rules.is_empty()
                    })
                    .count();
            let unique = !overlay.section.is_empty()
                && overlay_counts.get(overlay.section.as_str()) == Some(&1)
                && section_matches == 1;
            let satisfied = unique
                && matching_binds
                    .as_ref()
                    .is_some_and(|matching_binds| !matching_binds.is_empty());
            self.operational_overlays
                .push(OperationalOverlayRequirement {
                    id: format!("haproxy.prometheus-migration:{}", overlay.section),
                    kind: OperationalOverlayKind::PrometheusMigration,
                    origin: None,
                    redacted_evidence: false,
                    values: vec![
                        format!("section={}", overlay.section),
                        "native_route=/metrics".into(),
                        "runtime_contract=oxiroute_stats".into(),
                    ],
                    satisfied,
                });
            if satisfied {
                binds.extend(matching_binds.expect("satisfied overlay has socket binds"));
            } else {
                self.diagnostics.push(Diagnostic::new(
                    E_INVALID_VALUE,
                    Severity::Error,
                    DiagnosticStage::Lower,
                    format!(
                        "HAProxy Prometheus migration overlay for section `{}` must uniquely match one exact, dedicated Prometheus service with only IP socket binds",
                        overlay.section
                    ),
                ));
            }
        }
        binds.sort_unstable();
        binds.dedup();
        if binds.is_empty() {
            return;
        }
        self.draft.stats = Some(Stats {
            binds,
            admin_token_file: None,
        });
    }

    fn lower_frontend(&mut self, frontend: &EffectiveFrontend) {
        if self.block_semantic_settings(&frontend.settings) {
            return;
        }
        let Some(name) = self.canonical_name(&frontend.section, "service") else {
            return;
        };
        let Some(mode) = self.frontend_mode_selection(frontend) else {
            return;
        };
        if mode.protocol == Protocol::Http {
            let Some(service) = self.lower_http_frontend(frontend, &name) else {
                return;
            };
            if self.lower_listeners(
                &frontend.section,
                &name,
                &frontend.binds,
                &frontend.settings,
                &mode,
            ) {
                self.commit_http_service(service);
            }
            return;
        }
        let draft = self.draft.clone();
        let provenance = self.provenance.clone();
        let certificate_names = self.certificate_names.clone();
        if self.lower_listeners(
            &frontend.section,
            &name,
            &frontend.binds,
            &frontend.settings,
            &mode,
        ) && !self.lower_tcp_frontend(frontend, &name, &mode.sources)
        {
            self.draft = draft;
            self.provenance = provenance;
            self.certificate_names = certificate_names;
        }
    }

    fn lower_listen(&mut self, listen: &EffectiveListen) {
        if self.block_semantic_settings(&listen.settings) {
            return;
        }
        let Some(name) = self.canonical_name(&listen.section, "service") else {
            return;
        };
        let Some(mode) = self.listen_mode_selection(listen) else {
            return;
        };
        if mode.protocol == Protocol::Http {
            let Some(service) = self.lower_http_listen(listen, &name) else {
                return;
            };
            if self.lower_listeners(
                &listen.section,
                &name,
                &listen.binds,
                &listen.settings,
                &mode,
            ) {
                self.commit_http_service(service);
            }
            return;
        }
        let draft = self.draft.clone();
        let provenance = self.provenance.clone();
        let certificate_names = self.certificate_names.clone();
        if self.lower_listeners(
            &listen.section,
            &name,
            &listen.binds,
            &listen.settings,
            &mode,
        ) && !self.lower_tcp_listen(listen, &name, &mode.sources)
        {
            self.draft = draft;
            self.provenance = provenance;
            self.certificate_names = certificate_names;
        }
    }
}

fn stats_socket(bind: &BindAddress) -> Option<SocketAddr> {
    let BindAddress::Tcp { host, port } = bind else {
        return None;
    };
    let ip = match host.as_slice() {
        b"" | b"*" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        b"::" | b"[::]" => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        host => std::str::from_utf8(host).ok()?.parse().ok()?,
    };
    Some(SocketAddr::new(ip, *port))
}
