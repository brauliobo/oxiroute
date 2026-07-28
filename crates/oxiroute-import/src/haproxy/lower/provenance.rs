use std::collections::HashSet;

use crate::{
    CanonicalProvenance, Diagnostic, DiagnosticStage, E_SEMANTICS_NOT_REPRESENTABLE,
    ProvenanceRole, ProvenanceSpan, Severity,
};

use super::Lowerer;
use crate::haproxy::{EffectiveSection, Provenance, ProxySettings};

#[derive(Clone)]
pub(super) struct CanonicalPath(String);

impl CanonicalPath {
    pub(super) fn root(field: &str) -> Self {
        Self(format!("/{field}"))
    }

    pub(super) fn indexed(collection: &str, index: usize) -> Self {
        Self(format!("/{collection}/{index}"))
    }

    pub(super) fn field(&self, field: &str) -> Self {
        Self(format!("{}/{field}", self.0))
    }

    pub(super) fn index(&self, index: usize) -> Self {
        Self(format!("{}/{index}", self.0))
    }

    pub(super) fn into_string(self) -> String {
        self.0
    }
}

impl Lowerer<'_> {
    pub(super) fn block_value<T>(
        &mut self,
        value: &crate::haproxy::EffectiveValue<T>,
        message: &str,
    ) {
        self.block_provenance(&value.provenance, message);
    }

    pub(super) fn block_provenance(&mut self, provenance: &Provenance, message: &str) {
        let mut diagnostic = Diagnostic::new(
            E_SEMANTICS_NOT_REPRESENTABLE,
            Severity::Error,
            DiagnosticStage::Lower,
            message,
        )
        .with_primary_span(provenance.origin_span);
        for source in provenance_sources(provenance).into_iter().skip(1) {
            diagnostic =
                diagnostic.with_related_span(source.span, provenance_role_message(source.role));
        }
        self.diagnostics.push(diagnostic);
    }

    pub(super) fn block_section(&mut self, section: &EffectiveSection, message: &str) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SEMANTICS_NOT_REPRESENTABLE,
                Severity::Error,
                DiagnosticStage::Lower,
                message,
            )
            .with_primary_span(section.span),
        );
    }

    pub(super) fn record(&mut self, path: CanonicalPath, mut origins: Vec<ProvenanceSpan>) {
        deduplicate_sources(&mut origins);
        let path = path.into_string();
        if let Some(existing) = self
            .provenance
            .iter_mut()
            .find(|provenance| provenance.path == path)
        {
            existing.origins.extend(origins);
            deduplicate_sources(&mut existing.origins);
        } else {
            self.provenance.push(CanonicalProvenance { path, origins });
        }
    }
}

pub(super) fn section_sources(section: &EffectiveSection) -> Vec<ProvenanceSpan> {
    vec![ProvenanceSpan {
        role: ProvenanceRole::Declaration,
        span: section.span,
    }]
}

pub(super) fn provenance_sources(provenance: &Provenance) -> Vec<ProvenanceSpan> {
    let mut sources = vec![ProvenanceSpan {
        role: ProvenanceRole::Value,
        span: provenance.origin_span,
    }];
    for step in &provenance.inheritance {
        sources.push(ProvenanceSpan {
            role: ProvenanceRole::Inherited,
            span: step.reference_span.unwrap_or(provenance.origin_span),
        });
    }
    for reference in &provenance.references {
        sources.push(ProvenanceSpan {
            role: ProvenanceRole::Reference,
            span: reference.use_span,
        });
        sources.extend(reference.targets.iter().map(|target| ProvenanceSpan {
            role: ProvenanceRole::Reference,
            span: target.span,
        }));
    }
    deduplicate_sources(&mut sources);
    sources
}

pub(super) fn extend_sources(sources: &mut Vec<ProvenanceSpan>, provenance: &Provenance) {
    sources.extend(provenance_sources(provenance));
    deduplicate_sources(sources);
}

pub(super) fn extend_tcp_policy_sources(
    sources: &mut Vec<ProvenanceSpan>,
    frontend: &ProxySettings,
    backend: &ProxySettings,
) {
    for provenance in [
        frontend
            .timeouts
            .client
            .as_ref()
            .map(|value| &value.provenance),
        backend
            .timeouts
            .connect
            .as_ref()
            .map(|value| &value.provenance),
        backend
            .timeouts
            .server
            .as_ref()
            .map(|value| &value.provenance),
        backend.retries.as_ref().map(|value| &value.provenance),
        backend.redispatch.as_ref().map(|value| &value.provenance),
    ]
    .into_iter()
    .flatten()
    {
        extend_sources(sources, provenance);
    }
}

pub(super) fn deduplicate_sources(sources: &mut Vec<ProvenanceSpan>) {
    let mut seen = HashSet::new();
    sources.retain(|source| seen.insert((source.role as u8, source.span)));
}

const fn provenance_role_message(role: ProvenanceRole) -> &'static str {
    match role {
        ProvenanceRole::Declaration => "canonical object declared here",
        ProvenanceRole::Value => "contributing HAProxy value",
        ProvenanceRole::Inherited => "value inherited through defaults here",
        ProvenanceRole::Reference => "resolved HAProxy reference",
    }
}
