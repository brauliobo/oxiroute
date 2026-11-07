#[allow(clippy::wildcard_imports)]
use super::*;

impl Builder {
    pub(super) fn new(
        graph: SourceGraph,
        invocation: InvocationFacts,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            graph,
            invocation,
            declarations: Vec::new(),
            imports: Vec::new(),
            vmod_objects: Vec::new(),
            acls: Vec::new(),
            probes: Vec::new(),
            backends: Vec::new(),
            directors: Vec::new(),
            modern_directors: Vec::new(),
            subroutines: Vec::new(),
            statements: Vec::new(),
            import_aliases: BTreeMap::new(),
            vmod_object_names: BTreeMap::new(),
            acl_names: BTreeMap::new(),
            probe_names: BTreeMap::new(),
            backend_names: BTreeMap::new(),
            director_names: BTreeMap::new(),
            modern_director_names: BTreeMap::new(),
            subroutine_names: BTreeMap::new(),
            call_edges: Vec::new(),
            call_truncated: false,
            diagnostics,
        }
    }

    pub(super) fn build(mut self) -> ImportReport {
        self.collect_symbols();
        for item in self.graph.expanded.clone() {
            self.classify_declaration(item);
        }
        let compositions = self.compose_subroutines();
        let call_graph = self.finish_call_graph();
        self.validate_unique_symbols();
        self.validate_versions();
        let (candidate, lowering, diagnostics) = super::super::super::lower::lower(
            &self.graph,
            &self.invocation,
            &self.backends,
            &self.directors,
            &self.probes,
            &self.modern_directors,
            &self.subroutines,
            &self.statements,
            &self.imports,
            &self.vmod_objects,
            self.diagnostics,
        );
        let ((), diagnostics) = Report::new((), diagnostics).into_parts();
        let sources = self
            .graph
            .sources
            .iter()
            .map(|source| source.source.clone())
            .collect();
        ImportReport {
            source_graph: self.graph,
            sources,
            declarations: self.declarations,
            imports: self.imports,
            vmod_objects: self.vmod_objects,
            acls: self.acls,
            probes: self.probes,
            backends: self.backends,
            directors: self.directors,
            modern_directors: self.modern_directors,
            subroutines: self.subroutines,
            compositions,
            call_graph,
            statements: self.statements,
            invocation: self.invocation,
            diagnostics,
            candidate,
            lowering,
        }
    }

    fn compose_subroutines(&self) -> Vec<SubroutineComposition> {
        self.subroutine_names
            .iter()
            .map(|(name, fragments)| SubroutineComposition {
                name: name.clone(),
                fragments: fragments.clone(),
                built_in: if SubroutineKind::from_name(name).has_builtin() {
                    BuiltinComposition::AppendedAfterUserFragments
                } else {
                    BuiltinComposition::None
                },
            })
            .collect()
    }

    fn finish_call_graph(&mut self) -> CallGraph {
        let adjacency = self.call_edges.iter().fold(
            BTreeMap::<Vec<u8>, Vec<Vec<u8>>>::new(),
            |mut graph, edge| {
                graph
                    .entry(edge.caller.clone())
                    .or_default()
                    .push(edge.callee.clone());
                graph
            },
        );
        let mut cycles = BTreeSet::new();
        let mut depth_limited = false;
        let mut walk = 0;
        for node in adjacency.keys() {
            detect_cycles(
                node,
                &adjacency,
                &mut Vec::new(),
                &mut cycles,
                &mut depth_limited,
                &mut walk,
            );
        }
        CallGraph {
            edges: std::mem::take(&mut self.call_edges),
            cycles: cycles.into_iter().collect(),
            truncated: self.call_truncated,
            depth_limited,
        }
    }

    fn validate_versions(&mut self) {
        if let Some(root) = self.graph.root.and_then(|root| self.graph.source(root))
            && !root
                .document
                .declarations
                .iter()
                .any(|declaration| matches!(declaration, Declaration::Version { .. }))
        {
            self.diagnostics.push(
                Diagnostic::new(
                    E_VCL_VERSION,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "root VCL source does not declare a version",
                )
                .with_primary_span(root.source.full_span()),
            );
        }
        for source in &self.graph.sources {
            let versions = source
                .document
                .declarations
                .iter()
                .filter_map(|declaration| match declaration {
                    Declaration::Version { span, .. } => Some(*span),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if versions.len() > 1 {
                self.diagnostics.extend(versions.into_iter().map(|span| {
                    Diagnostic::new(
                        E_VCL_VERSION,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "source declares more than one VCL version",
                    )
                    .with_primary_span(span)
                }));
            }
        }
        for declaration in &self.declarations {
            match &declaration.version.effective {
                None => self.diagnostics.push(
                    Diagnostic::new(
                        E_VCL_VERSION,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "VCL declaration has no declared or inherited version",
                    )
                    .with_primary_span(declaration.provenance.span)
                    .with_include_stack(declaration.provenance.include_stack.iter().copied()),
                ),
                Some(VclVersion::Other(_)) => self.diagnostics.push(
                    Diagnostic::new(
                        E_VCL_VERSION,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "only VCL 4.0 and 4.1 are supported by the strict parser",
                    )
                    .with_primary_span(declaration.provenance.span),
                ),
                Some(VclVersion::V4_0 | VclVersion::V4_1) => {}
            }
        }
    }

    pub(super) fn version_context(&self, item: &LoadedDeclaration) -> VersionContext {
        let origin = if matches!(item.declaration, Declaration::Version { .. }) {
            VersionOrigin::Declared
        } else if item.effective_version.is_none() {
            VersionOrigin::Missing
        } else {
            let source_declared = self
                .graph
                .source(item.provenance.span.source())
                .is_some_and(|source| {
                    source.document.declarations.iter().any(|declaration| {
                        matches!(declaration, Declaration::Version { .. })
                            && declaration.span().range().start()
                                < item.provenance.span.range().start()
                    })
                });
            if source_declared {
                VersionOrigin::SourceDeclared
            } else {
                VersionOrigin::IncludeInherited
            }
        };
        VersionContext {
            effective: item.effective_version.clone(),
            origin,
        }
    }

    fn validate_unique_symbols(&mut self) {
        let mut duplicates = Vec::new();
        collect_duplicate_spans(
            &self.acl_names,
            &self.acls,
            |value| value.provenance.span,
            "ACL",
            &mut duplicates,
        );
        collect_duplicate_spans(
            &self.probe_names,
            &self.probes,
            |value| value.provenance.span,
            "probe",
            &mut duplicates,
        );
        collect_duplicate_spans(
            &self.backend_names,
            &self.backends,
            |value| value.provenance.span,
            "backend",
            &mut duplicates,
        );
        collect_duplicate_spans(
            &self.director_names,
            &self.directors,
            |value| value.provenance.span,
            "legacy director",
            &mut duplicates,
        );
        collect_duplicate_spans(
            &self.modern_director_names,
            &self.modern_directors,
            |value| value.provenance.span,
            "modern director",
            &mut duplicates,
        );
        collect_duplicate_spans(
            &self.vmod_object_names,
            &self.vmod_objects,
            |value| value.provenance.span,
            "VMOD object",
            &mut duplicates,
        );
        for legacy_name in self.director_names.keys() {
            if self.modern_director_names.contains_key(legacy_name) {
                duplicates.extend(
                    self.director_names[legacy_name]
                        .iter()
                        .map(|index| ("director", self.directors[*index].provenance.span)),
                );
                duplicates.extend(
                    self.modern_director_names[legacy_name]
                        .iter()
                        .map(|index| ("director", self.modern_directors[*index].provenance.span)),
                );
            }
        }
        for (kind, span) in duplicates {
            self.diagnostics.push(
                Diagnostic::new(
                    E_DUPLICATE_IDENTITY,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!("duplicate VCL {kind} identity"),
                )
                .with_primary_span(span),
            );
        }
    }

    pub(super) fn unsupported(
        &mut self,
        span: Span,
        include_stack: &[Span],
        message: &'static str,
    ) {
        self.diagnostics.push(
            Diagnostic::new(
                E_VCL_UNSUPPORTED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                message,
            )
            .with_primary_span(span)
            .with_include_stack(include_stack.iter().copied()),
        );
    }
}
