#[allow(clippy::wildcard_imports)]
use super::*;

impl Builder {
    pub(super) fn classify_condition(&self, expression: &Expression) -> Condition {
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

    pub(super) fn backend_reference(
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

    pub(super) fn backend_only_reference(
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

    pub(super) fn modern_director_reference(
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

    pub(super) fn classify_backend_property(
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
}
