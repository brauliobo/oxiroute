use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::{
    CanonicalCandidate as SharedCanonicalCandidate, CanonicalDraft, CanonicalProvenance,
    Diagnostic, ProvenanceSpan,
};

use super::{
    EffectiveBackend, EffectiveConfiguration, EffectiveListen, EffectiveSection, ProxySettings,
    SectionId,
};

mod endpoint;
mod http;
mod policy;
mod provenance;
mod report;
mod tcp;
mod tls;

pub type CanonicalCandidate = SharedCanonicalCandidate<ProvenanceSpan>;

pub(super) use report::lower;

pub(super) struct Lowerer<'a> {
    effective: &'a EffectiveConfiguration,
    diagnostics: Vec<Diagnostic>,
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<ProvenanceSpan>>,
    lowered_pools: HashSet<SectionId>,
    certificate_names: HashMap<(PathBuf, PathBuf), (String, usize)>,
}

pub(super) struct Representability {
    complete: bool,
}

impl Representability {
    pub(super) const fn new(complete: bool) -> Self {
        Self { complete }
    }

    pub(super) fn require(&mut self, condition: bool) {
        self.complete &= condition;
    }

    pub(super) const fn is_complete(&self) -> bool {
        self.complete
    }
}

pub(super) enum BackendView<'a> {
    Backend(&'a EffectiveBackend),
    Listen(&'a EffectiveListen),
}

impl BackendView<'_> {
    pub(super) const fn section(&self) -> &EffectiveSection {
        match self {
            Self::Backend(backend) => &backend.section,
            Self::Listen(listen) => &listen.section,
        }
    }

    pub(super) const fn settings(&self) -> &ProxySettings {
        match self {
            Self::Backend(backend) => &backend.settings,
            Self::Listen(listen) => &listen.settings,
        }
    }
}

impl Lowerer<'_> {
    pub(super) fn backend_view(&self, id: SectionId) -> Option<BackendView<'_>> {
        self.effective
            .backends
            .iter()
            .find(|backend| backend.section.id == id)
            .map(BackendView::Backend)
            .or_else(|| {
                self.effective
                    .listens
                    .iter()
                    .find(|listen| listen.section.id == id)
                    .map(BackendView::Listen)
            })
    }

    pub(super) fn section_name(&self, id: SectionId) -> Option<String> {
        let backend = self.backend_view(id)?;
        canonical_string(backend.section().name.as_deref()?)
    }

    pub(super) fn canonical_name(
        &mut self,
        section: &EffectiveSection,
        kind: &str,
    ) -> Option<String> {
        let Some(name) = section.name.as_deref().and_then(canonical_string) else {
            self.block_section(
                section,
                &format!("HAProxy {kind} name is not representable as canonical UTF-8"),
            );
            return None;
        };
        Some(name)
    }
}

fn canonical_string(value: &[u8]) -> Option<String> {
    std::str::from_utf8(value).ok().map(str::to_owned)
}
