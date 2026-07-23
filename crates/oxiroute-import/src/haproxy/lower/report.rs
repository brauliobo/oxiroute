use oxiroute_config::{Config, Protocol, validate_config};

use crate::{Diagnostic, DiagnosticStage, E_INVALID_VALUE, Report, Severity};

use super::{CanonicalCandidate, Lowerer};

use crate::haproxy::{EffectiveConfiguration, EffectiveFrontend, EffectiveListen};

/// Lowers an `HAProxy` semantic report into a canonical draft and conditionally finalized config.
#[must_use]
pub(crate) fn lower(resolution: &Report<EffectiveConfiguration>) -> Report<CanonicalCandidate> {
    Lowerer::new(resolution).run()
}

impl<'a> Lowerer<'a> {
    fn new(resolution: &'a Report<EffectiveConfiguration>) -> Self {
        Self {
            effective: resolution.value(),
            diagnostics: resolution.diagnostics().to_vec(),
            draft: crate::CanonicalDraft::default(),
            provenance: Vec::new(),
            lowered_pools: std::collections::HashSet::new(),
            certificate_names: std::collections::HashMap::new(),
        }
    }

    fn run(mut self) -> Report<CanonicalCandidate> {
        if let Some(maxconn) = self.effective.global.maxconn.as_ref() {
            if maxconn.value == 0 {
                self.block_value(
                    maxconn,
                    "HAProxy global maxconn zero selects an environment-derived process admission limit",
                );
            } else {
                self.block_value(
                    maxconn,
                    "HAProxy global maxconn is an aggregate process limit, not a per-listener limit",
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
            self.lower_pool(&listen.section, &listen.settings, &listen.servers);
        }
        for frontend in &self.effective.frontends {
            self.lower_frontend(frontend);
        }
        for listen in &self.effective.listens {
            self.lower_listen(listen);
        }

        let draft = self.draft.clone();
        let config = self.finalize(&draft);
        Report::new(
            CanonicalCandidate {
                draft,
                provenance: self.provenance,
                config,
            },
            self.diagnostics,
        )
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
                frontend.settings.maxconn.as_ref(),
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
            frontend.settings.maxconn.as_ref(),
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
                listen.settings.maxconn.as_ref(),
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
            listen.settings.maxconn.as_ref(),
            &mode,
        ) && !self.lower_tcp_listen(listen, &name, &mode.sources)
        {
            self.draft = draft;
            self.provenance = provenance;
            self.certificate_names = certificate_names;
        }
    }
}
