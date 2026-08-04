use oxiroute_config::L4Service;

use crate::ProvenanceSpan;

use super::Lowerer;
use crate::haproxy::{EffectiveFrontend, EffectiveListen};

use super::provenance::{
    CanonicalPath, extend_sources, extend_tcp_policy_sources, section_sources,
};

impl Lowerer<'_> {
    pub(super) fn lower_tcp_frontend(
        &mut self,
        frontend: &EffectiveFrontend,
        name: &str,
        mode_sources: &[ProvenanceSpan],
    ) -> bool {
        if !frontend.use_backends.is_empty() {
            self.block_section(
                &frontend.section,
                "canonical TCP services cannot represent ordered HAProxy use_backend rules",
            );
            return false;
        }
        let Some(reference) = &frontend.settings.default_backend else {
            self.block_section(
                &frontend.section,
                "HAProxy TCP frontend has no default_backend; no fallback pool will be inserted",
            );
            return false;
        };
        let target = reference.value.target;
        if !self.lowered_pools.contains(&target) {
            self.block_value(
                reference,
                "HAProxy TCP backend did not lower to a complete canonical pool; no fallback pool will be inserted",
            );
            return false;
        }
        let Some(backend) = self.backend_view(target) else {
            self.block_value(
                reference,
                "HAProxy TCP backend target is not available for lowering",
            );
            return false;
        };
        let backend_settings = backend.settings().clone();
        let Some((connect_timeout_ms, idle_timeout_ms)) =
            self.lower_tcp_policy(&frontend.section, &frontend.settings, &backend_settings)
        else {
            return false;
        };
        let Some(pool) = self.section_name(target) else {
            self.block_value(
                reference,
                "HAProxy TCP backend target has no canonical name",
            );
            return false;
        };
        let service_index = self.draft.l4_services.len();
        self.draft.l4_services.push(L4Service {
            name: name.to_owned(),
            upstream_pool: pool,
            connect_timeout_ms,
            idle_timeout_ms,
            lifetime_timeout_ms: None,
            udp: None,
        });
        let mut sources = section_sources(&frontend.section);
        sources.extend_from_slice(mode_sources);
        extend_sources(&mut sources, &reference.provenance);
        extend_tcp_policy_sources(&mut sources, &frontend.settings, &backend_settings);
        let service_path = CanonicalPath::indexed("l4_services", service_index);
        self.record(service_path.clone(), sources.clone());
        self.record(service_path.field("upstream_pool"), sources);
        true
    }

    pub(super) fn lower_tcp_listen(
        &mut self,
        listen: &EffectiveListen,
        name: &str,
        mode_sources: &[ProvenanceSpan],
    ) -> bool {
        if !listen.use_backends.is_empty() {
            self.block_section(
                &listen.section,
                "canonical TCP services cannot represent ordered HAProxy use_backend rules",
            );
            return false;
        }
        let target = listen
            .settings
            .default_backend
            .as_ref()
            .map_or(listen.section.id, |reference| reference.value.target);
        if !self.lowered_pools.contains(&target) {
            self.block_section(
                &listen.section,
                "HAProxy listen backend did not lower to a complete canonical pool; no fallback pool will be inserted",
            );
            return false;
        }
        let Some(backend) = self.backend_view(target) else {
            self.block_section(
                &listen.section,
                "HAProxy listen backend target is not available for lowering",
            );
            return false;
        };
        let backend_settings = backend.settings().clone();
        let Some((connect_timeout_ms, idle_timeout_ms)) =
            self.lower_tcp_policy(&listen.section, &listen.settings, &backend_settings)
        else {
            return false;
        };
        let Some(pool) = self.section_name(target) else {
            self.block_section(
                &listen.section,
                "HAProxy listen backend has no canonical name",
            );
            return false;
        };
        let service_index = self.draft.l4_services.len();
        self.draft.l4_services.push(L4Service {
            name: name.to_owned(),
            upstream_pool: pool,
            connect_timeout_ms,
            idle_timeout_ms,
            lifetime_timeout_ms: None,
            udp: None,
        });
        let mut sources = section_sources(&listen.section);
        sources.extend_from_slice(mode_sources);
        if let Some(reference) = &listen.settings.default_backend {
            extend_sources(&mut sources, &reference.provenance);
        }
        extend_tcp_policy_sources(&mut sources, &listen.settings, &backend_settings);
        let service_path = CanonicalPath::indexed("l4_services", service_index);
        self.record(service_path.clone(), sources.clone());
        self.record(service_path.field("upstream_pool"), sources);
        true
    }
}
