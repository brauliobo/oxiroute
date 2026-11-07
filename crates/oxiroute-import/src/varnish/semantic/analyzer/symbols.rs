#[allow(clippy::wildcard_imports)]
use super::*;

impl Builder {
    pub(super) fn collect_symbols(&mut self) {
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

    pub(super) fn classify_declaration(&mut self, item: LoadedDeclaration) {
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
}
