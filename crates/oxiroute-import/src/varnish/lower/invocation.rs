#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_invocation(&mut self) {
        if self.source_invocation.truncated {
            self.block(
                LoweringBlocker::Invocation,
                E_SOURCE_LIMIT,
                DiagnosticStage::Source,
                "varnishd invocation was truncated before exact lowering",
                self.root_span(),
            );
        }
        if !self.source_invocation.unsupported_arguments.is_empty() {
            for argument in self.source_invocation.unsupported_arguments.clone() {
                self.block(
                    LoweringBlocker::Invocation,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    format!("unsupported varnishd argument `{argument}` blocks lowering"),
                    self.root_span(),
                );
            }
        }

        let mut listeners = Vec::new();
        let mut default_ttl_ms = DEFAULT_TTL_MS;
        let mut default_grace_ms = DEFAULT_GRACE_MS;
        let mut default_keep_ms = DEFAULT_KEEP_MS;
        for startup in self.source_invocation.startup.clone() {
            match startup {
                super::super::StartupFact::Listen(value) => match parse_listen(&value) {
                    Some(address) => listeners.push(address),
                    None => self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        format!("varnishd listen address `{value}` is not a canonical IP socket"),
                        self.root_span(),
                    ),
                },
                super::super::StartupFact::Parameter(setting) => {
                    let target = match setting.name.as_str() {
                        "default_ttl" => &mut default_ttl_ms,
                        "default_grace" => &mut default_grace_ms,
                        "default_keep" => &mut default_keep_ms,
                        _ => {
                            self.block(
                                LoweringBlocker::Invocation,
                                E_VCL_LOWERING_BLOCKED,
                                DiagnosticStage::Lower,
                                format!(
                                    "varnishd parameter `{}` has no exact canonical mapping",
                                    setting.name
                                ),
                                self.root_span(),
                            );
                            continue;
                        }
                    };
                    let Some(value) = setting.value.as_deref().and_then(parse_duration_ms_bytes)
                    else {
                        self.block(
                            LoweringBlocker::Invocation,
                            E_VCL_SEMANTIC_MISMATCH,
                            DiagnosticStage::Lower,
                            format!("varnishd parameter `{}` is not a finite millisecond duration", setting.name),
                            self.root_span(),
                        );
                        continue;
                    };
                    *target = value;
                }
                super::super::StartupFact::Foreground => {}
                super::super::StartupFact::Vcl(value) => {
                    if self
                        .graph
                        .root
                        .and_then(|root| self.graph.source(root))
                        .and_then(|source| source.canonical_path.as_deref())
                        .is_some_and(|path| path.to_string_lossy() != value)
                    {
                        self.block(
                            LoweringBlocker::Invocation,
                            E_VCL_SEMANTIC_MISMATCH,
                            DiagnosticStage::Lower,
                            "varnishd -f does not identify the imported VCL root",
                            self.root_span(),
                        );
                    }
                }
                super::super::StartupFact::Management(_)
                | super::super::StartupFact::Instance(_)
                | super::super::StartupFact::Jail(_)
                | super::super::StartupFact::Secret(_)
                | super::super::StartupFact::PidFile(_) => self.block(
                    LoweringBlocker::Invocation,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    "varnishd deployment or management startup options are not part of the exact canonical runtime",
                    self.root_span(),
                ),
            }
        }
        listeners.sort_unstable();
        listeners.dedup();
        if listeners.is_empty() {
            listeners.push(DEFAULT_LISTEN);
        }
        if default_grace_ms > default_keep_ms {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "Varnish default grace exceeds default keep and cannot be represented by the canonical cache timeline",
                self.root_span(),
            );
        }

        let storage = self.lower_storage();
        self.invocation_policy = Some(InvocationPolicy {
            listeners,
            storage,
            default_ttl_ms,
            default_grace_ms,
            default_keep_ms,
        });
    }

    fn lower_storage(&mut self) -> Option<CacheStore> {
        if self.source_invocation.storage.len() > 1 {
            self.block(
                LoweringBlocker::Invocation,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "multiple varnishd storage backends do not have one canonical cache-store identity",
                self.root_span(),
            );
            return None;
        }
        let Some(storage) = self.source_invocation.storage.first().cloned() else {
            return Some(CacheStore::memory("varnish-memory", DEFAULT_MEMORY_BYTES));
        };
        let name = storage
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "varnish-cache".into());
        match storage.kind {
            super::super::StorageKind::Malloc => {
                let Some(max_bytes) = storage
                    .arguments
                    .first()
                    .and_then(|value| parse_size(value))
                else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "malloc storage requires one exact bounded size",
                        self.root_span(),
                    );
                    return None;
                };
                Some(CacheStore::memory(name, max_bytes))
            }
            super::super::StorageKind::File => {
                let Some(path) = storage.arguments.first().and_then(canonical_storage_path) else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "file storage requires one safe absolute canonical root",
                        self.root_span(),
                    );
                    return None;
                };
                let Some(max_bytes) = storage.arguments.get(1).and_then(|value| parse_size(value))
                else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "file storage requires one exact bounded size",
                        self.root_span(),
                    );
                    return None;
                };
                Some(CacheStore::disk(name, path, max_bytes))
            }
            super::super::StorageKind::None => None,
            super::super::StorageKind::Persistent
            | super::super::StorageKind::DeprecatedPersistent
            | super::super::StorageKind::Umem
            | super::super::StorageKind::Unknown(_) => {
                self.block(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    "selected varnishd storage backend is outside the exact memory/disk cache subset",
                    self.root_span(),
                );
                None
            }
        }
    }
}
