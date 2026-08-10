#[must_use]
pub fn analyze(root: &SourceFile, included_sources: &[SourceFile]) -> ImportReport {
    analyze_graph(
        load_memory(root, included_sources),
        InvocationFacts::default(),
    )
}

#[cfg(unix)]
#[must_use]
pub fn import(root: &Path, invocation: &VarnishdInvocation) -> ImportReport {
    analyze_graph(super::loader::load(root), invocation.facts())
}

#[must_use]
pub fn analyze_graph(loaded: Report<SourceGraph>, invocation: InvocationFacts) -> ImportReport {
    let (graph, diagnostics) = loaded.into_parts();
    Builder::new(graph, invocation, diagnostics).build()
}

struct Builder {
    graph: SourceGraph,
    invocation: InvocationFacts,
    declarations: Vec<DeclarationDecision>,
    imports: Vec<VmodImport>,
    vmod_objects: Vec<VmodObject>,
    acls: Vec<Acl>,
    probes: Vec<Probe>,
    backends: Vec<Backend>,
    directors: Vec<Director>,
    modern_directors: Vec<ModernDirector>,
    subroutines: Vec<Subroutine>,
    statements: Vec<StatementDecision>,
    import_aliases: BTreeMap<Vec<u8>, Vec<u8>>,
    vmod_object_names: BTreeMap<Vec<u8>, Vec<usize>>,
    acl_names: BTreeMap<Vec<u8>, Vec<usize>>,
    probe_names: BTreeMap<Vec<u8>, Vec<usize>>,
    backend_names: BTreeMap<Vec<u8>, Vec<usize>>,
    director_names: BTreeMap<Vec<u8>, Vec<usize>>,
    modern_director_names: BTreeMap<Vec<u8>, Vec<usize>>,
    subroutine_names: BTreeMap<Vec<u8>, Vec<usize>>,
    call_edges: Vec<CallEdge>,
    call_truncated: bool,
    diagnostics: Vec<Diagnostic>,
}

impl Builder {
    fn new(graph: SourceGraph, invocation: InvocationFacts, diagnostics: Vec<Diagnostic>) -> Self {
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

    fn build(mut self) -> ImportReport {
        self.collect_symbols();
        for item in self.graph.expanded.clone() {
            self.classify_declaration(item);
        }
        let compositions = self.compose_subroutines();
        let call_graph = self.finish_call_graph();
        self.validate_unique_symbols();
        self.validate_versions();
        let (candidate, lowering, diagnostics) = super::lower::lower(
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

    fn collect_symbols(&mut self) {
        let expanded = self.graph.expanded.clone();
        let mut acl = 0;
        let mut probe = 0;
        let mut backend = 0;
        let mut director = 0;
        let mut subroutine = 0;
        for item in &expanded {
            match &item.declaration {
                Declaration::Import(declaration) => {
                    let alias = declaration
                        .alias
                        .as_ref()
                        .unwrap_or(&declaration.module)
                        .bytes
                        .clone();
                    self.import_aliases
                        .insert(alias, declaration.module.bytes.clone());
                }
                Declaration::Acl(declaration) => {
                    self.acl_names
                        .entry(declaration.name.bytes.clone())
                        .or_default()
                        .push(acl);
                    acl += 1;
                }
                Declaration::Probe(declaration) => {
                    self.probe_names
                        .entry(declaration.name.bytes.clone())
                        .or_default()
                        .push(probe);
                    probe += 1;
                }
                Declaration::Backend(declaration) => {
                    self.backend_names
                        .entry(declaration.name.bytes.clone())
                        .or_default()
                        .push(backend);
                    backend += 1;
                }
                Declaration::Director(declaration) => {
                    self.director_names
                        .entry(declaration.name.bytes.clone())
                        .or_default()
                        .push(director);
                    director += 1;
                }
                Declaration::Subroutine(declaration) => {
                    self.subroutine_names
                        .entry(declaration.name.bytes.clone())
                        .or_default()
                        .push(subroutine);
                    subroutine += 1;
                }
                Declaration::Version { .. }
                | Declaration::Include(_)
                | Declaration::Unsupported { .. } => {}
            }
        }
        for item in expanded {
            if let Declaration::Subroutine(declaration) = item.declaration {
                self.collect_new_objects(&declaration.statements, &item.provenance);
            }
        }
    }

    fn collect_new_objects(&mut self, statements: &[Statement], provenance: &Provenance) {
        for statement in statements {
            match &statement.kind {
                StatementKind::New(new) => self.collect_new_object(new, statement.span, provenance),
                StatementKind::If(if_statement) => {
                    for branch in &if_statement.branches {
                        self.collect_new_objects(&branch.statements, provenance);
                    }
                    self.collect_new_objects(&if_statement.otherwise, provenance);
                }
                _ => {}
            }
        }
    }

    fn collect_new_object(
        &mut self,
        new: &NewObjectStatement,
        span: Span,
        provenance: &Provenance,
    ) {
        let Some((function, _)) = expression_call(&new.constructor) else {
            return;
        };
        let Some((prefix, method)) = split_method(function) else {
            return;
        };
        let Some(module) = self.import_aliases.get(prefix).cloned() else {
            return;
        };
        if module != b"directors" {
            let index = self.vmod_objects.len();
            self.vmod_object_names
                .entry(new.name.bytes.clone())
                .or_default()
                .push(index);
            self.vmod_objects.push(VmodObject {
                name: new.name.bytes.clone(),
                module,
                constructor: new.constructor.clone(),
                provenance: Provenance {
                    span,
                    include_stack: provenance.include_stack.clone(),
                },
            });
            return;
        }
        let index = self.modern_directors.len();
        self.modern_director_names
            .entry(new.name.bytes.clone())
            .or_default()
            .push(index);
        self.modern_directors.push(ModernDirector {
            name: new.name.bytes.clone(),
            kind: director_kind(method),
            constructor: new.constructor.clone(),
            methods: Vec::new(),
            provenance: Provenance {
                span,
                include_stack: provenance.include_stack.clone(),
            },
        });
    }

    fn classify_declaration(&mut self, item: LoadedDeclaration) {
        let sequence = self.declarations.len();
        let version = self.version_context(&item);
        let classification = match &item.declaration {
            Declaration::Version { value, .. } => {
                DeclarationClassification::Version(VclVersion::from_bytes(&value.bytes))
            }
            Declaration::Include(include) => DeclarationClassification::Include {
                path: include.path.bytes.clone(),
                glob: include.glob,
                resolved: item.include_resolved.unwrap_or(false),
            },
            Declaration::Import(declaration) => self.classify_import(declaration, &item.provenance),
            Declaration::Acl(declaration) => self.classify_acl(declaration, &item.provenance),
            Declaration::Probe(declaration) => self.classify_probe(declaration, &item.provenance),
            Declaration::Backend(declaration) => {
                self.classify_backend(declaration, &item.provenance)
            }
            Declaration::Director(declaration) => {
                self.classify_director(declaration, &item.provenance)
            }
            Declaration::Subroutine(declaration) => {
                self.classify_subroutine(declaration, &item.provenance)
            }
            Declaration::Unsupported { .. } => {
                self.unsupported(
                    item.provenance.span,
                    &item.provenance.include_stack,
                    "top-level VCL declaration is unsupported",
                );
                DeclarationClassification::Unsupported
            }
        };
        self.declarations.push(DeclarationDecision {
            sequence,
            provenance: item.provenance,
            version,
            classification,
        });
    }

    fn classify_import(
        &mut self,
        declaration: &ImportDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.imports.len();
        let alias = declaration
            .alias
            .as_ref()
            .unwrap_or(&declaration.module)
            .bytes
            .clone();
        self.imports.push(VmodImport {
            module: declaration.module.bytes.clone(),
            alias: alias.clone(),
            from: declaration.from.as_ref().map(|value| value.bytes.clone()),
            provenance: provenance.clone(),
        });
        DeclarationClassification::Import {
            module: declaration.module.bytes.clone(),
            alias,
            from: declaration.from.as_ref().map(|value| value.bytes.clone()),
            index,
        }
    }

    fn classify_acl(
        &mut self,
        declaration: &AclDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.acls.len();
        self.acls.push(Acl {
            name: declaration.name.bytes.clone(),
            entries: declaration
                .entries
                .iter()
                .map(|entry| AclEntry {
                    negated: entry.negated,
                    optional: entry.optional,
                    value: entry.value.bytes.clone(),
                    mask: entry.mask.as_ref().map(|value| value.bytes.clone()),
                    provenance: Provenance {
                        span: entry.span,
                        include_stack: provenance.include_stack.clone(),
                    },
                })
                .collect(),
            provenance: provenance.clone(),
        });
        DeclarationClassification::Acl {
            name: declaration.name.bytes.clone(),
            index,
        }
    }

    fn classify_probe(
        &mut self,
        declaration: &ProbeDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.probes.len();
        let properties = declaration
            .properties
            .iter()
            .map(classify_probe_property)
            .collect();
        self.probes.push(Probe {
            name: declaration.name.bytes.clone(),
            properties,
            provenance: provenance.clone(),
        });
        DeclarationClassification::Probe {
            name: declaration.name.bytes.clone(),
            index,
        }
    }

    fn classify_backend(
        &mut self,
        declaration: &BackendDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.backends.len();
        let properties = declaration
            .properties
            .iter()
            .map(|property| self.classify_backend_property(property, provenance))
            .collect::<Vec<_>>();
        let kind = backend_kind(declaration, &properties);
        for (assignment, property) in declaration.properties.iter().zip(&properties) {
            if matches!(property, BackendProperty::Unsupported { .. }) {
                self.unsupported(
                    assignment.span,
                    &provenance.include_stack,
                    "backend property is unsupported",
                );
            }
        }
        self.backends.push(Backend {
            name: declaration.name.bytes.clone(),
            kind,
            properties,
            provenance: provenance.clone(),
        });
        DeclarationClassification::Backend {
            name: declaration.name.bytes.clone(),
            index,
        }
    }

    fn classify_director(
        &mut self,
        declaration: &DirectorDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.directors.len();
        let mut members = Vec::new();
        let mut unsupported_properties = Vec::new();
        for property in declaration.properties.iter().chain(
            declaration
                .entries
                .iter()
                .flat_map(|entry| &entry.properties),
        ) {
            if property.target.bytes == b".backend" {
                members.push(self.backend_only_reference(&property.value, provenance));
            } else {
                unsupported_properties.push(property.target.bytes.clone());
                self.unsupported(
                    property.span,
                    &provenance.include_stack,
                    "legacy director property is unsupported",
                );
            }
        }
        self.directors.push(Director {
            name: declaration.name.bytes.clone(),
            policy: director_kind(&declaration.policy.bytes),
            members,
            unsupported_properties,
            provenance: provenance.clone(),
        });
        DeclarationClassification::Director {
            name: declaration.name.bytes.clone(),
            index,
        }
    }

    fn classify_subroutine(
        &mut self,
        declaration: &SubroutineDeclaration,
        provenance: &Provenance,
    ) -> DeclarationClassification {
        let index = self.subroutines.len();
        self.subroutines.push(Subroutine {
            kind: SubroutineKind::from_name(&declaration.name.bytes),
            name: declaration.name.bytes.clone(),
            statement_ids: Vec::new(),
            provenance: provenance.clone(),
        });
        let ids = self.classify_statements(
            index,
            &declaration.name.bytes,
            &declaration.statements,
            None,
            0,
            &provenance.include_stack,
        );
        self.subroutines[index].statement_ids = ids;
        DeclarationClassification::Subroutine {
            name: declaration.name.bytes.clone(),
            index,
        }
    }

    fn classify_statements(
        &mut self,
        subroutine: usize,
        subroutine_name: &[u8],
        statements: &[Statement],
        parent: Option<usize>,
        depth: usize,
        include_stack: &[Span],
    ) -> Vec<usize> {
        let mut ids = Vec::with_capacity(statements.len());
        for statement in statements {
            let id = self.statements.len();
            let provenance = Provenance {
                span: statement.span,
                include_stack: include_stack.to_vec(),
            };
            let classification = self.classify_statement(statement, subroutine_name, &provenance);
            self.statements.push(StatementDecision {
                id,
                subroutine,
                parent,
                depth,
                provenance: provenance.clone(),
                classification,
            });
            ids.push(id);
            if let StatementKind::If(if_statement) = &statement.kind {
                self.classify_if_children(
                    subroutine,
                    subroutine_name,
                    if_statement,
                    id,
                    depth + 1,
                    include_stack,
                );
            }
        }
        ids
    }

    fn classify_if_children(
        &mut self,
        subroutine: usize,
        subroutine_name: &[u8],
        if_statement: &IfStatement,
        parent: usize,
        depth: usize,
        include_stack: &[Span],
    ) {
        for branch in &if_statement.branches {
            self.classify_statements(
                subroutine,
                subroutine_name,
                &branch.statements,
                Some(parent),
                depth,
                include_stack,
            );
        }
        self.classify_statements(
            subroutine,
            subroutine_name,
            &if_statement.otherwise,
            Some(parent),
            depth,
            include_stack,
        );
    }

    fn classify_statement(
        &mut self,
        statement: &Statement,
        subroutine_name: &[u8],
        provenance: &Provenance,
    ) -> StatementClassification {
        let classification = match &statement.kind {
            StatementKind::If(if_statement) => StatementClassification::Conditional(
                if_statement
                    .branches
                    .iter()
                    .map(|branch| self.classify_condition(&branch.condition))
                    .collect(),
            ),
            StatementKind::Set(assignment) => self.classify_assignment(assignment, provenance),
            StatementKind::Unset(target) => Self::classify_unset(target, provenance),
            StatementKind::Return(expression) => self.classify_return(expression),
            StatementKind::Call(target) => {
                self.classify_subroutine_call(subroutine_name, target, provenance)
            }
            StatementKind::New(new) => self.classify_new(new),
            StatementKind::Expression(expression) => self.classify_call(expression, provenance),
            StatementKind::InlineC => {
                StatementClassification::Unsupported(UnsupportedBehavior::InlineC)
            }
            StatementKind::Invalid => StatementClassification::Invalid,
        };
        if matches!(
            classification,
            StatementClassification::Unsupported(_) | StatementClassification::Dynamic(_)
        ) {
            self.unsupported(
                statement.span,
                &provenance.include_stack,
                "VCL statement is retained without execution",
            );
        } else if let StatementClassification::Conditional(conditions) = &classification
            && conditions.iter().any(condition_is_non_static)
        {
            self.unsupported(
                statement.span,
                &provenance.include_stack,
                "VCL condition contains dynamic or unsupported behavior",
            );
        }
        classification
    }

    fn classify_assignment(
        &mut self,
        assignment: &Assignment,
        provenance: &Provenance,
    ) -> StatementClassification {
        if let Some((object, method, arguments)) = object_method_call(&assignment.value)
            && matches!(
                assignment.target.bytes.as_slice(),
                b"req.backend_hint" | b"bereq.backend"
            )
            && method == b"backend"
        {
            return StatementClassification::BackendSelection(self.modern_director_reference(
                object,
                arguments,
                &assignment.value,
                provenance,
            ));
        }
        if first_call(&assignment.value).is_some() {
            return self.unsupported_expression_call(&assignment.value);
        }
        if let Some(field) = cache_lifetime_field(&assignment.target.bytes) {
            return StatementClassification::CacheLifetime(CacheLifetime {
                field,
                operator: assignment.operator,
                value: assignment.value.clone(),
            });
        }
        match assignment.target.bytes.as_slice() {
            b"beresp.uncacheable" => {
                return StatementClassification::CacheFlag(CacheFlag::Uncacheable(
                    assignment.value.clone(),
                ));
            }
            b"bereq.is_bgfetch" => {
                return StatementClassification::CacheFlag(CacheFlag::BackgroundFetch(
                    assignment.value.clone(),
                ));
            }
            b"beresp.do_esi" => {
                return StatementClassification::Feature(FeatureBehavior::Esi {
                    enabled: assignment.value.clone(),
                });
            }
            b"beresp.do_gzip" => {
                return StatementClassification::Feature(FeatureBehavior::Compression {
                    operation: CompressionOperation::Gzip,
                    enabled: assignment.value.clone(),
                });
            }
            b"beresp.do_gunzip" => {
                return StatementClassification::Feature(FeatureBehavior::Compression {
                    operation: CompressionOperation::Gunzip,
                    enabled: assignment.value.clone(),
                });
            }
            b"req.backend_hint" | b"bereq.backend" => {
                return StatementClassification::BackendSelection(
                    self.backend_reference(&assignment.value, provenance),
                );
            }
            _ => {}
        }
        if let Some((scope, name)) = parse_header(&assignment.target.bytes) {
            let operation = match assignment.operator {
                AssignmentOperator::Set => HeaderOperation::Set,
                AssignmentOperator::Add => HeaderOperation::Append,
                _ => {
                    return StatementClassification::Unsupported(UnsupportedBehavior::Assignment {
                        target: assignment.target.bytes.clone(),
                    });
                }
            };
            return StatementClassification::HeaderMutation(HeaderMutation {
                scope,
                cookie: is_cookie(&name),
                name,
                operation,
                value: Some(assignment.value.clone()),
            });
        }
        StatementClassification::Dynamic(DynamicBehavior::Assignment {
            target: assignment.target.bytes.clone(),
            value: assignment.value.clone(),
        })
    }

    fn classify_unset(target: &super::Value, provenance: &Provenance) -> StatementClassification {
        parse_header(&target.bytes).map_or_else(
            || {
                StatementClassification::Dynamic(DynamicBehavior::Assignment {
                    target: target.bytes.clone(),
                    value: invalid_expression(provenance.span),
                })
            },
            |(scope, name)| {
                StatementClassification::HeaderMutation(HeaderMutation {
                    scope,
                    cookie: is_cookie(&name),
                    name,
                    operation: HeaderOperation::Remove,
                    value: None,
                })
            },
        )
    }

    fn classify_return(&self, expression: &Expression) -> StatementClassification {
        if let Some(name) = expression_name(expression) {
            return match name {
                b"lookup" => StatementClassification::CacheDecision(FlowAction::Lookup),
                b"hash" => StatementClassification::CacheDecision(FlowAction::Hash),
                b"pass" => StatementClassification::CacheDecision(FlowAction::Pass),
                b"pipe" => StatementClassification::CacheDecision(FlowAction::Pipe),
                b"miss" => StatementClassification::CacheDecision(FlowAction::Miss),
                b"deliver" => StatementClassification::CacheDecision(FlowAction::Deliver),
                b"abandon" => StatementClassification::CacheDecision(FlowAction::Abandon),
                b"restart" => StatementClassification::CacheDecision(FlowAction::Restart),
                b"retry" => StatementClassification::CacheDecision(FlowAction::Retry),
                b"fail" => StatementClassification::CacheDecision(FlowAction::Fail),
                b"purge" => StatementClassification::Invalidation(Invalidation::Purge),
                other => StatementClassification::Unsupported(UnsupportedBehavior::Return {
                    action: other.to_vec(),
                }),
            };
        }
        let Some((function, arguments)) = expression_call(expression) else {
            return StatementClassification::Unsupported(UnsupportedBehavior::Return {
                action: Vec::new(),
            });
        };
        if function == b"pass"
            && let [duration] = arguments
        {
            if first_call(duration).is_some() {
                return self.unsupported_expression_call(expression);
            }
            return StatementClassification::CacheFlag(CacheFlag::HitForPass {
                duration: duration.clone(),
            });
        }
        if function != b"synth"
            || arguments
                .iter()
                .any(|argument| first_call(argument).is_some())
        {
            return self.unsupported_expression_call(expression);
        }
        let status = arguments.first().and_then(literal_status);
        let reason = arguments.get(1).cloned();
        if let Some(status) = status.filter(|status| matches!(status, 301 | 302 | 303 | 307 | 308))
        {
            StatementClassification::Response(ResponseAction::Redirect { status, reason })
        } else {
            StatementClassification::Response(ResponseAction::Synth { status, reason })
        }
    }

    fn classify_subroutine_call(
        &mut self,
        caller: &[u8],
        target: &super::Value,
        provenance: &Provenance,
    ) -> StatementClassification {
        let targets = self
            .subroutine_names
            .get(&target.bytes)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            self.diagnostics.push(
                Diagnostic::new(
                    E_VCL_UNRESOLVED_REFERENCE,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "VCL subroutine call is unresolved",
                )
                .with_primary_span(target.span)
                .with_include_stack(provenance.include_stack.iter().copied()),
            );
        }
        if self.call_edges.len() == MAX_CALL_EDGES {
            self.call_truncated = true;
            self.diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "VCL call graph edge limit exceeded",
                )
                .with_primary_span(target.span),
            );
        } else if !self.call_truncated {
            self.call_edges.push(CallEdge {
                caller: caller.to_vec(),
                callee: target.bytes.clone(),
                targets: targets.clone(),
                provenance: provenance.clone(),
            });
        }
        StatementClassification::SubroutineCall {
            name: target.bytes.clone(),
            targets,
        }
    }

    fn classify_new(&self, new: &NewObjectStatement) -> StatementClassification {
        unique_index(&self.modern_director_names, &new.name.bytes).map_or_else(
            || self.unsupported_expression_call(&new.constructor),
            |object| StatementClassification::NewDirector { object },
        )
    }

    fn classify_call(
        &mut self,
        expression: &Expression,
        provenance: &Provenance,
    ) -> StatementClassification {
        let Some((function, arguments)) = expression_call(expression) else {
            return StatementClassification::Dynamic(DynamicBehavior::Call {
                function: Vec::new(),
                arguments: Vec::new(),
            });
        };
        if arguments
            .iter()
            .any(|argument| first_call(argument).is_some())
        {
            return self.unsupported_expression_call(expression);
        }
        match function {
            b"hash_data" if arguments.len() == 1 => {
                return StatementClassification::Hash(arguments[0].clone());
            }
            b"ban" if arguments.len() == 1 => {
                return StatementClassification::Invalidation(Invalidation::Ban(
                    arguments[0].clone(),
                ));
            }
            b"synthetic" if arguments.len() == 1 => {
                return StatementClassification::Response(ResponseAction::SyntheticBody(
                    arguments[0].clone(),
                ));
            }
            _ => {}
        }
        if let Some((object, method)) = split_method(function)
            && let Some(index) = unique_index(&self.modern_director_names, object)
        {
            if let Some(method) = self.director_method(method, arguments, provenance) {
                self.modern_directors[index].methods.push(method.clone());
                return StatementClassification::DirectorMethod {
                    object: index,
                    method,
                };
            }
            return StatementClassification::Unsupported(UnsupportedBehavior::DirectorMethod {
                object: object.to_vec(),
                method: method.to_vec(),
            });
        }
        self.unsupported_call(function, arguments)
    }

    fn director_method(
        &mut self,
        method: &[u8],
        arguments: &[Expression],
        provenance: &Provenance,
    ) -> Option<DirectorMethod> {
        match (method, arguments) {
            (b"add_backend", [backend]) => Some(DirectorMethod::AddBackend {
                backend: self.backend_only_reference(backend, provenance),
                weight: None,
            }),
            (b"add_backend", [backend, weight]) => Some(DirectorMethod::AddBackend {
                backend: self.backend_only_reference(backend, provenance),
                weight: Some(weight.clone()),
            }),
            (b"backend", _) => Some(DirectorMethod::BackendLookup {
                arguments: arguments.to_vec(),
            }),
            _ => None,
        }
    }

    fn unsupported_call(
        &self,
        function: &[u8],
        arguments: &[Expression],
    ) -> StatementClassification {
        if let Some((prefix, _)) = split_method(function) {
            if let Some(module) = self.import_aliases.get(prefix) {
                return StatementClassification::Unsupported(UnsupportedBehavior::VmodCall {
                    module: module.clone(),
                    function: function.to_vec(),
                });
            }
            if let Some(index) = unique_index(&self.vmod_object_names, prefix) {
                return StatementClassification::Unsupported(UnsupportedBehavior::VmodCall {
                    module: self.vmod_objects[index].module.clone(),
                    function: function.to_vec(),
                });
            }
        }
        if function.contains(&b'.') {
            StatementClassification::Dynamic(DynamicBehavior::Call {
                function: function.to_vec(),
                arguments: arguments.to_vec(),
            })
        } else {
            StatementClassification::Unsupported(UnsupportedBehavior::FunctionCall {
                function: function.to_vec(),
            })
        }
    }

    fn unsupported_expression_call(&self, expression: &Expression) -> StatementClassification {
        expression_call(expression).map_or_else(
            || {
                StatementClassification::Dynamic(DynamicBehavior::Call {
                    function: Vec::new(),
                    arguments: vec![expression.clone()],
                })
            },
            |(function, arguments)| self.unsupported_call(function, arguments),
        )
    }

    fn classify_condition(&self, expression: &Expression) -> Condition {
        if matches!(expression.kind, ExpressionKind::Call { .. }) {
            return self.classify_call_condition(expression);
        }
        match &expression.kind {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => Condition::Not(Box::new(self.classify_condition(operand))),
            ExpressionKind::Binary {
                left,
                operator: BinaryOperator::And,
                right,
            } => Condition::All(
                Box::new(self.classify_condition(left)),
                Box::new(self.classify_condition(right)),
            ),
            ExpressionKind::Binary {
                left,
                operator: BinaryOperator::Or,
                right,
            } => Condition::Any(
                Box::new(self.classify_condition(left)),
                Box::new(self.classify_condition(right)),
            ),
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => self.classify_comparison(expression, left, *operator, right),
            ExpressionKind::Name(value) => parse_header(&value.bytes).map_or_else(
                || Condition::Value(expression.clone()),
                |(scope, name)| header_condition(scope, name, ConditionOperator::Exists, None),
            ),
            ExpressionKind::Call { .. } | ExpressionKind::Invalid => {
                Condition::Dynamic(expression.clone())
            }
            ExpressionKind::Object(_) => Condition::Dynamic(expression.clone()),
            ExpressionKind::Literal(_) | ExpressionKind::Unary { .. } => {
                Condition::Value(expression.clone())
            }
        }
    }

    fn classify_comparison(
        &self,
        whole: &Expression,
        left: &Expression,
        operator: BinaryOperator,
        right: &Expression,
    ) -> Condition {
        if first_call(whole).is_some() {
            return self.classify_call_condition(whole);
        }
        let Some(operator) = condition_operator(operator) else {
            return Condition::Dynamic(whole.clone());
        };
        if let Some((scope, name)) = expression_name(left).and_then(parse_header) {
            return header_condition(scope, name, operator, Some(right.clone()));
        }
        if matches!(
            operator,
            ConditionOperator::Match | ConditionOperator::NotMatch
        ) && let Some(name) = expression_name(right)
            && self.acl_names.contains_key(name)
        {
            return Condition::Acl {
                value: left.clone(),
                name: name.to_vec(),
                declaration: unique_index(&self.acl_names, name),
                negated: operator == ConditionOperator::NotMatch,
            };
        }
        Condition::Comparison {
            left: left.clone(),
            operator,
            right: right.clone(),
        }
    }

    fn classify_call_condition(&self, expression: &Expression) -> Condition {
        let classification = first_call(expression).map_or_else(
            || self.unsupported_expression_call(expression),
            |function| self.unsupported_call(function, &[]),
        );
        match classification {
            StatementClassification::Unsupported(behavior) => Condition::UnsupportedCall {
                behavior,
                expression: expression.clone(),
            },
            _ => Condition::Dynamic(expression.clone()),
        }
    }

    fn backend_reference(
        &mut self,
        expression: &Expression,
        provenance: &Provenance,
    ) -> BackendReference {
        if expression_name(expression) == Some(b"none".as_slice()) {
            return BackendReference::None;
        }
        let Some(name) = expression_name(expression) else {
            return BackendReference::Dynamic(expression.clone());
        };
        let backend = unique_index(&self.backend_names, name);
        let legacy = unique_index(&self.director_names, name);
        let modern = unique_index(&self.modern_director_names, name);
        match (backend, legacy, modern) {
            (Some(declaration), None, None) => BackendReference::Backend {
                name: name.to_vec(),
                declaration,
            },
            (None, Some(declaration), None) => BackendReference::Director {
                name: name.to_vec(),
                declaration,
                modern: false,
                arguments: Vec::new(),
            },
            (None, None, Some(declaration)) => BackendReference::Director {
                name: name.to_vec(),
                declaration,
                modern: true,
                arguments: Vec::new(),
            },
            _ => self.unresolved_backend(name, expression.span, provenance),
        }
    }

    fn backend_only_reference(
        &mut self,
        expression: &Expression,
        provenance: &Provenance,
    ) -> BackendReference {
        let Some(name) = expression_name(expression) else {
            return BackendReference::Dynamic(expression.clone());
        };
        unique_index(&self.backend_names, name).map_or_else(
            || self.unresolved_backend(name, expression.span, provenance),
            |declaration| BackendReference::Backend {
                name: name.to_vec(),
                declaration,
            },
        )
    }

    fn modern_director_reference(
        &mut self,
        object: &[u8],
        arguments: &[Expression],
        expression: &Expression,
        provenance: &Provenance,
    ) -> BackendReference {
        unique_index(&self.modern_director_names, object).map_or_else(
            || self.unresolved_backend(object, expression.span, provenance),
            |declaration| BackendReference::Director {
                name: object.to_vec(),
                declaration,
                modern: true,
                arguments: arguments.to_vec(),
            },
        )
    }

    fn unresolved_backend(
        &mut self,
        name: &[u8],
        span: Span,
        provenance: &Provenance,
    ) -> BackendReference {
        self.diagnostics.push(
            Diagnostic::new(
                E_VCL_UNRESOLVED_REFERENCE,
                Severity::Error,
                DiagnosticStage::Resolve,
                "backend/director reference has no unique declaration",
            )
            .with_primary_span(span)
            .with_include_stack(provenance.include_stack.iter().copied()),
        );
        BackendReference::Unresolved {
            name: name.to_vec(),
        }
    }

    fn classify_backend_property(
        &mut self,
        assignment: &Assignment,
        provenance: &Provenance,
    ) -> BackendProperty {
        if assignment.target.bytes == b".probe" {
            return BackendProperty::Probe(self.probe_reference(&assignment.value, provenance));
        }
        classify_backend_property(assignment)
    }

    fn probe_reference(
        &mut self,
        expression: &Expression,
        provenance: &Provenance,
    ) -> ProbeReference {
        if let ExpressionKind::Object(properties) = &expression.kind {
            return ProbeReference::Inline(
                properties.iter().map(classify_probe_property).collect(),
            );
        }
        let Some(name) = expression_name(expression) else {
            return ProbeReference::Dynamic(expression.clone());
        };
        if let Some(declaration) = unique_index(&self.probe_names, name) {
            return ProbeReference::Named {
                name: name.to_vec(),
                declaration,
            };
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_VCL_UNRESOLVED_REFERENCE,
                Severity::Error,
                DiagnosticStage::Resolve,
                "probe reference has no unique declaration",
            )
            .with_primary_span(expression.span)
            .with_include_stack(provenance.include_stack.iter().copied()),
        );
        ProbeReference::Unresolved {
            name: name.to_vec(),
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

    fn version_context(&self, item: &LoadedDeclaration) -> VersionContext {
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

    fn unsupported(&mut self, span: Span, include_stack: &[Span], message: &'static str) {
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

fn collect_duplicate_spans<T, F>(
    names: &BTreeMap<Vec<u8>, Vec<usize>>,
    values: &[T],
    span: F,
    kind: &'static str,
    duplicates: &mut Vec<(&'static str, Span)>,
) where
    F: Fn(&T) -> Span,
{
    for indices in names.values().filter(|indices| indices.len() > 1) {
        duplicates.extend(indices.iter().map(|index| (kind, span(&values[*index]))));
    }
}

fn classify_probe_property(assignment: &Assignment) -> ProbeProperty {
    if assignment.operator != AssignmentOperator::Set {
        return ProbeProperty::Unsupported {
            name: assignment.target.bytes.clone(),
            value: assignment.value.clone(),
        };
    }
    match assignment.target.bytes.as_slice() {
        b".url" => ProbeProperty::Url(assignment.value.clone()),
        b".request" => ProbeProperty::Request(assignment.value.clone()),
        b".expected_response" => ProbeProperty::ExpectedResponse(assignment.value.clone()),
        b".timeout" => ProbeProperty::Timeout(assignment.value.clone()),
        b".interval" => ProbeProperty::Interval(assignment.value.clone()),
        b".window" => ProbeProperty::Window(assignment.value.clone()),
        b".threshold" => ProbeProperty::Threshold(assignment.value.clone()),
        b".initial" => ProbeProperty::Initial(assignment.value.clone()),
        _ => ProbeProperty::Unsupported {
            name: assignment.target.bytes.clone(),
            value: assignment.value.clone(),
        },
    }
}

fn classify_backend_property(assignment: &Assignment) -> BackendProperty {
    if first_call(&assignment.value).is_some() {
        return BackendProperty::Unsupported {
            name: assignment.target.bytes.clone(),
            value: assignment.value.clone(),
        };
    }
    match assignment.target.bytes.as_slice() {
        b".host" => BackendProperty::Host(assignment.value.clone()),
        b".port" => BackendProperty::Port(assignment.value.clone()),
        b".path" => BackendProperty::Path(assignment.value.clone()),
        b".connect_timeout" => BackendProperty::ConnectTimeout(assignment.value.clone()),
        b".first_byte_timeout" => BackendProperty::FirstByteTimeout(assignment.value.clone()),
        b".between_bytes_timeout" => BackendProperty::BetweenBytesTimeout(assignment.value.clone()),
        b".max_connections" => BackendProperty::MaxConnections(assignment.value.clone()),
        b".proxy_header" => BackendProperty::ProxyHeader(assignment.value.clone()),
        _ => BackendProperty::Unsupported {
            name: assignment.target.bytes.clone(),
            value: assignment.value.clone(),
        },
    }
}

fn backend_kind(declaration: &BackendDeclaration, properties: &[BackendProperty]) -> BackendKind {
    if declaration.kind == BackendDeclarationKind::None {
        return BackendKind::None;
    }
    let path = properties.iter().find_map(|property| match property {
        BackendProperty::Path(path) => Some(path.clone()),
        _ => None,
    });
    if let Some(path) = path {
        return BackendKind::Unix { path };
    }
    let host = properties.iter().find_map(|property| match property {
        BackendProperty::Host(host) => Some(host.clone()),
        _ => None,
    });
    let port = properties.iter().find_map(|property| match property {
        BackendProperty::Port(port) => Some(port.clone()),
        _ => None,
    });
    if host.is_some() {
        BackendKind::Network { host, port }
    } else {
        BackendKind::Dynamic
    }
}

fn director_kind(value: &[u8]) -> DirectorKind {
    match value {
        b"round-robin" | b"round_robin" => DirectorKind::RoundRobin,
        b"random" => DirectorKind::Random,
        b"fallback" => DirectorKind::Fallback,
        b"hash" => DirectorKind::Hash,
        value => DirectorKind::Unknown(value.to_vec()),
    }
}

fn cache_lifetime_field(target: &[u8]) -> Option<CacheLifetimeField> {
    match target {
        b"beresp.ttl" => Some(CacheLifetimeField::Ttl),
        b"beresp.grace" => Some(CacheLifetimeField::Grace),
        b"beresp.keep" => Some(CacheLifetimeField::Keep),
        _ => None,
    }
}

fn condition_operator(operator: BinaryOperator) -> Option<ConditionOperator> {
    Some(match operator {
        BinaryOperator::Equal => ConditionOperator::Equal,
        BinaryOperator::NotEqual => ConditionOperator::NotEqual,
        BinaryOperator::Match => ConditionOperator::Match,
        BinaryOperator::NotMatch => ConditionOperator::NotMatch,
        BinaryOperator::Less => ConditionOperator::Less,
        BinaryOperator::LessEqual => ConditionOperator::LessEqual,
        BinaryOperator::Greater => ConditionOperator::Greater,
        BinaryOperator::GreaterEqual => ConditionOperator::GreaterEqual,
        BinaryOperator::And
        | BinaryOperator::Or
        | BinaryOperator::Add
        | BinaryOperator::Subtract
        | BinaryOperator::Multiply
        | BinaryOperator::Divide
        | BinaryOperator::Concatenate => return None,
    })
}

fn header_condition(
    scope: HeaderScope,
    name: Vec<u8>,
    operator: ConditionOperator,
    value: Option<Expression>,
) -> Condition {
    if is_cookie(&name) {
        Condition::Cookie {
            scope,
            name,
            operator,
            value,
        }
    } else {
        Condition::Header {
            scope,
            name,
            operator,
            value,
        }
    }
}

fn parse_header(value: &[u8]) -> Option<(HeaderScope, Vec<u8>)> {
    let (prefix, scope) = [
        (b"req.http.".as_slice(), HeaderScope::Request),
        (b"bereq.http.".as_slice(), HeaderScope::BackendRequest),
        (b"beresp.http.".as_slice(), HeaderScope::BackendResponse),
        (b"resp.http.".as_slice(), HeaderScope::Response),
        (b"obj.http.".as_slice(), HeaderScope::Object),
    ]
    .into_iter()
    .find(|(prefix, _)| value.starts_with(prefix))?;
    let name = value[prefix.len()..].to_vec();
    (!name.is_empty()).then_some((scope, name))
}

fn is_cookie(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"cookie") || name.eq_ignore_ascii_case(b"set-cookie")
}

fn condition_is_non_static(condition: &Condition) -> bool {
    match condition {
        Condition::All(left, right) | Condition::Any(left, right) => {
            condition_is_non_static(left) || condition_is_non_static(right)
        }
        Condition::Not(condition) => condition_is_non_static(condition),
        Condition::Dynamic(_) | Condition::UnsupportedCall { .. } => true,
        Condition::Header { .. }
        | Condition::Cookie { .. }
        | Condition::Acl { .. }
        | Condition::Comparison { .. }
        | Condition::Value(_) => false,
    }
}

fn unique_index(map: &BTreeMap<Vec<u8>, Vec<usize>>, name: &[u8]) -> Option<usize> {
    map.get(name)
        .and_then(|indices| (indices.len() == 1).then_some(indices[0]))
}

fn expression_name(expression: &Expression) -> Option<&[u8]> {
    let ExpressionKind::Name(value) = &expression.kind else {
        return None;
    };
    Some(&value.bytes)
}

fn expression_call(expression: &Expression) -> Option<(&[u8], &[Expression])> {
    let ExpressionKind::Call {
        function,
        arguments,
    } = &expression.kind
    else {
        return None;
    };
    Some((expression_name(function)?, arguments))
}

fn object_method_call(expression: &Expression) -> Option<(&[u8], &[u8], &[Expression])> {
    let (function, arguments) = expression_call(expression)?;
    let (object, method) = split_method(function)?;
    Some((object, method, arguments))
}

fn split_method(function: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = function.iter().rposition(|byte| *byte == b'.')?;
    Some((&function[..separator], &function[separator + 1..]))
}

fn first_call(expression: &Expression) -> Option<&[u8]> {
    match &expression.kind {
        ExpressionKind::Call {
            function,
            arguments,
        } => expression_name(function).or_else(|| arguments.iter().find_map(first_call)),
        ExpressionKind::Unary { operand, .. } => first_call(operand),
        ExpressionKind::Binary { left, right, .. } => {
            first_call(left).or_else(|| first_call(right))
        }
        ExpressionKind::Object(properties) => properties
            .iter()
            .find_map(|property| first_call(&property.value)),
        ExpressionKind::Name(_) | ExpressionKind::Literal(_) | ExpressionKind::Invalid => None,
    }
}

fn literal_status(expression: &Expression) -> Option<u16> {
    let ExpressionKind::Literal(Literal::Number(value)) = &expression.kind else {
        return None;
    };
    std::str::from_utf8(&value.bytes).ok()?.parse().ok()
}

fn invalid_expression(span: Span) -> Expression {
    Expression {
        kind: ExpressionKind::Invalid,
        span,
    }
}

fn detect_cycles(
    node: &[u8],
    adjacency: &BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
    stack: &mut Vec<Vec<u8>>,
    cycles: &mut BTreeSet<Vec<Vec<u8>>>,
    depth_limited: &mut bool,
    walk: &mut usize,
) {
    if stack.len() == MAX_CALL_DEPTH || *walk == MAX_CALL_WALK {
        *depth_limited = true;
        return;
    }
    *walk += 1;
    if let Some(index) = stack.iter().position(|entry| entry == node) {
        if cycles.len() == MAX_CALL_CYCLES {
            *depth_limited = true;
        } else {
            let mut cycle = stack[index..].to_vec();
            cycle.push(node.to_vec());
            cycles.insert(cycle);
        }
        return;
    }
    stack.push(node.to_vec());
    if let Some(targets) = adjacency.get(node) {
        for target in targets {
            detect_cycles(target, adjacency, stack, cycles, depth_limited, walk);
        }
    }
    stack.pop();
}

/// Stable human-readable ledger used by differential fixtures and later lowering work.
#[must_use]
pub fn decision_signatures(report: &ImportReport) -> Vec<String> {
    let mut signatures = Vec::with_capacity(report.declarations.len() + report.statements.len());
    signatures.extend(report.declarations.iter().map(declaration_signature));
    signatures.extend(report.statements.iter().map(|statement| {
        let subroutine = display_bytes(&report.subroutines[statement.subroutine].name);
        format!(
            "statement:{subroutine}:{}",
            statement_signature(&statement.classification)
        )
    }));
    signatures
}

fn declaration_signature(decision: &DeclarationDecision) -> String {
    match &decision.classification {
        DeclarationClassification::Version(version) => {
            format!("declaration:version:{}", display_bytes(version.as_bytes()))
        }
        DeclarationClassification::Include {
            path,
            glob,
            resolved,
        } => format!(
            "declaration:include{}:{}:{}",
            if *glob { "+glob" } else { "" },
            display_bytes(path),
            if *resolved { "resolved" } else { "unresolved" },
        ),
        DeclarationClassification::Import { module, .. } => {
            format!("declaration:import:{}:unsupported", display_bytes(module))
        }
        DeclarationClassification::Acl { name, .. } => {
            format!("declaration:acl:{}", display_bytes(name))
        }
        DeclarationClassification::Probe { name, .. } => {
            format!("declaration:probe:{}", display_bytes(name))
        }
        DeclarationClassification::Backend { name, .. } => {
            format!("declaration:backend:{}", display_bytes(name))
        }
        DeclarationClassification::Director { name, .. } => {
            format!("declaration:director:{}", display_bytes(name))
        }
        DeclarationClassification::Subroutine { name, .. } => {
            format!("declaration:subroutine:{}", display_bytes(name))
        }
        DeclarationClassification::Unsupported => "declaration:unsupported".to_owned(),
    }
}

fn statement_signature(classification: &StatementClassification) -> &'static str {
    match classification {
        StatementClassification::Conditional(_) => "condition",
        StatementClassification::CacheDecision(FlowAction::Lookup) => "lookup",
        StatementClassification::CacheDecision(FlowAction::Hash) => "hash",
        StatementClassification::CacheDecision(FlowAction::Pass) => "pass",
        StatementClassification::CacheDecision(FlowAction::Pipe) => "pipe",
        StatementClassification::CacheDecision(FlowAction::Miss) => "miss",
        StatementClassification::CacheDecision(FlowAction::Deliver) => "deliver",
        StatementClassification::CacheDecision(_) => "flow",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Ttl,
            ..
        }) => "ttl",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Grace,
            ..
        }) => "grace",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Keep,
            ..
        }) => "keep",
        StatementClassification::CacheFlag(CacheFlag::HitForPass { .. }) => "hit-for-pass",
        StatementClassification::CacheFlag(_) => "cache-flag",
        StatementClassification::BackendSelection(_) => "backend-selection",
        StatementClassification::HeaderMutation(mutation) if mutation.cookie => "cookie-mutation",
        StatementClassification::HeaderMutation(_) => "header-mutation",
        StatementClassification::Hash(_) => "hash-input",
        StatementClassification::Response(ResponseAction::Redirect { .. }) => "redirect",
        StatementClassification::Response(ResponseAction::Synth { .. }) => "synth",
        StatementClassification::Response(ResponseAction::SyntheticBody(_)) => "synthetic-body",
        StatementClassification::Invalidation(Invalidation::Ban(_)) => "ban",
        StatementClassification::Invalidation(Invalidation::Purge) => "purge",
        StatementClassification::Feature(FeatureBehavior::Esi { .. }) => "esi",
        StatementClassification::Feature(FeatureBehavior::Compression { .. }) => "compression",
        StatementClassification::NewDirector { .. } => "new-director",
        StatementClassification::DirectorMethod { .. } => "director-method",
        StatementClassification::SubroutineCall { .. } => "subroutine-call",
        StatementClassification::Dynamic(_) => "dynamic",
        StatementClassification::Unsupported(_) => "unsupported",
        StatementClassification::Invalid => "invalid",
    }
}

fn display_bytes(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}
