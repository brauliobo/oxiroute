#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn collect_duplicate_spans<T, F>(
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

pub(super) fn classify_probe_property(assignment: &Assignment) -> ProbeProperty {
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

pub(super) fn classify_backend_property(assignment: &Assignment) -> BackendProperty {
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

pub(super) fn backend_kind(
    declaration: &BackendDeclaration,
    properties: &[BackendProperty],
) -> BackendKind {
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

pub(super) fn director_kind(value: &[u8]) -> DirectorKind {
    match value {
        b"round-robin" | b"round_robin" => DirectorKind::RoundRobin,
        b"random" => DirectorKind::Random,
        b"fallback" => DirectorKind::Fallback,
        b"hash" => DirectorKind::Hash,
        value => DirectorKind::Unknown(value.to_vec()),
    }
}

pub(super) fn cache_lifetime_field(target: &[u8]) -> Option<CacheLifetimeField> {
    match target {
        b"beresp.ttl" => Some(CacheLifetimeField::Ttl),
        b"beresp.grace" => Some(CacheLifetimeField::Grace),
        b"beresp.keep" => Some(CacheLifetimeField::Keep),
        _ => None,
    }
}

pub(super) fn condition_operator(operator: BinaryOperator) -> Option<ConditionOperator> {
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

pub(super) fn header_condition(
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

pub(super) fn parse_header(value: &[u8]) -> Option<(HeaderScope, Vec<u8>)> {
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

pub(super) fn is_cookie(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"cookie") || name.eq_ignore_ascii_case(b"set-cookie")
}

pub(super) fn condition_is_non_static(condition: &Condition) -> bool {
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

pub(super) fn unique_index(map: &BTreeMap<Vec<u8>, Vec<usize>>, name: &[u8]) -> Option<usize> {
    map.get(name)
        .and_then(|indices| (indices.len() == 1).then_some(indices[0]))
}

pub(super) fn expression_name(expression: &Expression) -> Option<&[u8]> {
    let ExpressionKind::Name(value) = &expression.kind else {
        return None;
    };
    Some(&value.bytes)
}

pub(super) fn expression_call(expression: &Expression) -> Option<(&[u8], &[Expression])> {
    let ExpressionKind::Call {
        function,
        arguments,
    } = &expression.kind
    else {
        return None;
    };
    Some((expression_name(function)?, arguments))
}

pub(super) fn object_method_call(expression: &Expression) -> Option<(&[u8], &[u8], &[Expression])> {
    let (function, arguments) = expression_call(expression)?;
    let (object, method) = split_method(function)?;
    Some((object, method, arguments))
}

pub(super) fn split_method(function: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = function.iter().rposition(|byte| *byte == b'.')?;
    Some((&function[..separator], &function[separator + 1..]))
}

pub(super) fn first_call(expression: &Expression) -> Option<&[u8]> {
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

pub(super) fn literal_status(expression: &Expression) -> Option<u16> {
    let ExpressionKind::Literal(Literal::Number(value)) = &expression.kind else {
        return None;
    };
    std::str::from_utf8(&value.bytes).ok()?.parse().ok()
}

pub(super) fn invalid_expression(span: Span) -> Expression {
    Expression {
        kind: ExpressionKind::Invalid,
        span,
    }
}

pub(super) fn detect_cycles(
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
