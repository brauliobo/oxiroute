#[allow(clippy::wildcard_imports)]
use super::*;

impl Builder {
    pub(super) fn classify_statements(
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

    fn classify_unset(
        target: &super::super::super::Value,
        provenance: &Provenance,
    ) -> StatementClassification {
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
        target: &super::super::super::Value,
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

    pub(super) fn unsupported_call(
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

    pub(super) fn unsupported_expression_call(
        &self,
        expression: &Expression,
    ) -> StatementClassification {
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
}
