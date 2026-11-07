#[allow(clippy::wildcard_imports)]
use super::*;

pub(in super::super) fn load_memory(
    root: &SourceFile,
    includes: &[SourceFile],
) -> Report<SourceGraph> {
    let mut loader = MemoryLoader {
        root,
        includes,
        graph: SourceGraph {
            root: Some(root.id()),
            snapshot_stable: true,
            ..SourceGraph::default()
        },
        diagnostics: Vec::new(),
        active: Vec::new(),
        loaded: HashMap::new(),
        aggregate_bytes: 0,
    };
    loader.expand(root, &[], None);
    Report::new(loader.graph, loader.diagnostics)
}

struct MemoryLoader<'a> {
    root: &'a SourceFile,
    includes: &'a [SourceFile],
    graph: SourceGraph,
    diagnostics: Vec<Diagnostic>,
    active: Vec<SourceId>,
    loaded: HashMap<SourceId, usize>,
    aggregate_bytes: usize,
}

impl MemoryLoader<'_> {
    fn expand(
        &mut self,
        source: &SourceFile,
        include_stack: &[Span],
        inherited: Option<VclVersion>,
    ) {
        if self.active.contains(&source.id()) {
            return;
        }
        if include_stack.len() > MAX_INCLUDE_DEPTH {
            self.memory_limit(
                "VCL include depth limit exceeded",
                source.full_span(),
                include_stack,
            );
            return;
        }
        self.active.push(source.id());
        let document = if let Some(index) = self.loaded.get(&source.id()).copied() {
            self.graph.sources[index].document.clone()
        } else {
            if self.loaded.len() == MAX_SOURCE_FILES {
                self.memory_limit(
                    "VCL source count limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            if source.len() > MAX_SOURCE_BYTES {
                self.memory_limit(
                    "VCL source byte limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            if self.aggregate_bytes.saturating_add(source.len()) > MAX_AGGREGATE_SOURCE_BYTES {
                self.memory_limit(
                    "VCL aggregate source byte limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            let (document, diagnostics) = super::super::parse(source).into_parts();
            self.diagnostics.extend(
                diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.with_include_stack(include_stack.iter().copied())),
            );
            let index = self.graph.sources.len();
            self.loaded.insert(source.id(), index);
            self.aggregate_bytes += source.len();
            self.graph.sources.push(ParsedSource {
                source: source.clone(),
                canonical_path: None,
                document: document.clone(),
            });
            document
        };
        let mut version = inherited;
        for declaration in document.declarations {
            if self.graph.expanded.len() == MAX_EXPANDED_DIRECTIVES {
                self.memory_limit(
                    "VCL expanded declaration limit exceeded",
                    declaration.span(),
                    include_stack,
                );
                break;
            }
            if let Declaration::Version { value, .. } = &declaration {
                version = Some(VclVersion::from_bytes(&value.bytes));
            }
            let provenance = Provenance {
                span: declaration.span(),
                include_stack: include_stack.to_vec(),
            };
            let Declaration::Include(include) = &declaration else {
                self.graph.expanded.push(LoadedDeclaration {
                    declaration,
                    provenance,
                    effective_version: version.clone(),
                    include_resolved: None,
                });
                continue;
            };
            self.expand_memory_include(include, declaration.clone(), &provenance, version.as_ref());
        }
        self.active.pop();
    }

    fn expand_memory_include(
        &mut self,
        include: &IncludeDeclaration,
        declaration: Declaration,
        provenance: &Provenance,
        version: Option<&VclVersion>,
    ) {
        let mut matches = std::iter::once(self.root)
            .chain(self.includes)
            .filter(|source| {
                if include.glob {
                    glob_matches(&include.path.bytes, source.name().as_bytes())
                } else {
                    source.name().as_bytes() == include.path.bytes
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.name().as_bytes().cmp(right.name().as_bytes()));
        let ambiguous = !include.glob && matches.len() > 1;
        let resolved = if include.glob {
            !matches.is_empty()
        } else {
            matches.len() == 1
        };
        let truncated = matches.len() > MAX_GLOB_MATCHES;
        if truncated {
            matches.truncate(MAX_GLOB_MATCHES);
            self.memory_limit(
                "VCL in-memory glob match limit exceeded",
                include.path.span,
                &provenance.include_stack,
            );
        }
        self.graph.expanded.push(LoadedDeclaration {
            declaration,
            provenance: provenance.clone(),
            effective_version: version.cloned(),
            include_resolved: Some(resolved),
        });
        let edge_index = self.graph.includes.len();
        self.graph.includes.push(IncludeEdge {
            source: provenance.span.source(),
            span: provenance.span,
            glob: include.glob,
            pattern: include.path.bytes.clone(),
            targets: Vec::new(),
            truncated,
            failure: truncated.then_some(E_SOURCE_LIMIT),
        });
        if !resolved {
            self.graph.includes[edge_index].failure = Some(E_VCL_INCLUDE_NOT_FOUND);
            self.diagnostics.push(
                Diagnostic::new(
                    E_VCL_INCLUDE_NOT_FOUND,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    if ambiguous {
                        "VCL exact include is ambiguous in the in-memory source catalog"
                    } else {
                        "VCL include did not match an in-memory source"
                    },
                )
                .with_primary_span(include.path.span)
                .with_include_stack(provenance.include_stack.iter().copied()),
            );
            return;
        }
        if truncated {
            return;
        }
        for target in matches {
            let cycle = self.active.contains(&target.id());
            self.graph.includes[edge_index].targets.push(IncludeTarget {
                requested_path: PathBuf::from(target.name()),
                canonical_path: None,
                status: if cycle {
                    IncludeTargetStatus::Cycle(target.id())
                } else {
                    IncludeTargetStatus::Expanded(target.id())
                },
            });
            if cycle {
                self.graph.includes[edge_index].failure = Some(E_VCL_INCLUDE_CYCLE);
                self.diagnostics.push(
                    Diagnostic::new(
                        E_VCL_INCLUDE_CYCLE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "VCL include cycle detected",
                    )
                    .with_primary_span(include.span)
                    .with_include_stack(provenance.include_stack.iter().copied()),
                );
            } else {
                let mut stack = provenance.include_stack.clone();
                stack.push(include.span);
                self.expand(&target, &stack, version.cloned());
            }
        }
    }

    fn memory_limit(&mut self, message: &'static str, span: Span, include_stack: &[Span]) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                message,
            )
            .with_primary_span(span)
            .with_include_stack(include_stack.iter().copied()),
        );
    }
}
