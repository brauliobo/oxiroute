use crate::haproxy::{Directive, Word};

use super::{
    AclCriterion, AclDefinition, ConditionPolarity, ForwardFor, HttpCheck, HttpHeaderValue,
    HttpRequestRule, HttpResponseRule, PendingAclCondition, StatusRange, parse_u16,
};

pub(super) fn parse_acl(directive: &Directive) -> Option<AclDefinition> {
    let [name, criterion, rest @ ..] = directive.arguments.as_slice() else {
        return None;
    };
    let criterion = match criterion.value.as_slice() {
        b"hdr(host)" => AclCriterion::HostExact,
        b"path" => AclCriterion::PathExact,
        b"path_beg" => AclCriterion::PathPrefix,
        _ => return None,
    };
    let (case_insensitive, values) = if rest.first().is_some_and(|word| word.value == b"-i") {
        (true, &rest[1..])
    } else {
        (false, rest)
    };
    if values.is_empty() || values.iter().any(|value| value.value.starts_with(b"-")) {
        return None;
    }
    Some(AclDefinition {
        name: name.value.clone(),
        criterion,
        case_insensitive,
        values: values.iter().map(|value| value.value.clone()).collect(),
    })
}

pub(super) fn parse_http_request_rule(
    arguments: &[Word],
) -> Option<(HttpRequestRule, Option<PendingAclCondition>)> {
    match arguments {
        [action, name, value] if action.value == b"set-header" => Some((
            HttpRequestRule::SetHeader {
                name: name.value.clone(),
                value: parse_http_header_value(&value.value)?,
            },
            None,
        )),
        [action, name] if action.value == b"del-header" => Some((
            HttpRequestRule::RemoveHeader {
                name: name.value.clone(),
            },
            None,
        )),
        [action, kind, location, rest @ ..]
            if action.value == b"redirect" && kind.value == b"location" =>
        {
            if location.value.contains(&b'%') {
                return None;
            }
            let status = match rest {
                [] => 302,
                [code, value] if code.value == b"code" => parse_u16(&value.value)?,
                _ => return None,
            };
            matches!(status, 301 | 302 | 307 | 308).then(|| {
                (
                    HttpRequestRule::Redirect {
                        status,
                        location: location.value.clone(),
                    },
                    None,
                )
            })
        }
        [action, rest @ ..] if action.value == b"return" => {
            let (arguments, condition) = split_http_condition(rest);
            Some((parse_http_return_rule(arguments)?, condition))
        }
        _ => None,
    }
}

fn split_http_condition(arguments: &[Word]) -> (&[Word], Option<PendingAclCondition>) {
    match arguments {
        [action @ .., polarity, negation, acl]
            if matches!(polarity.value.as_slice(), b"if" | b"unless") && negation.value == b"!" =>
        {
            (
                action,
                Some(PendingAclCondition {
                    name: acl.value.clone(),
                    span: acl.span,
                    polarity: parse_condition_polarity(&polarity.value),
                    negated: true,
                }),
            )
        }
        [action @ .., polarity, acl] if matches!(polarity.value.as_slice(), b"if" | b"unless") => (
            action,
            Some(PendingAclCondition {
                name: acl.value.clone(),
                span: acl.span,
                polarity: parse_condition_polarity(&polarity.value),
                negated: false,
            }),
        ),
        _ => (arguments, None),
    }
}

fn parse_condition_polarity(value: &[u8]) -> ConditionPolarity {
    match value {
        b"if" => ConditionPolarity::If,
        b"unless" => ConditionPolarity::Unless,
        _ => unreachable!("condition syntax accepts only if or unless"),
    }
}

fn parse_http_return_rule(arguments: &[Word]) -> Option<HttpRequestRule> {
    let [status_keyword, status, rest @ ..] = arguments else {
        return None;
    };
    if status_keyword.value != b"status" {
        return None;
    }
    let status = parse_u16(&status.value)?;
    let mut content_type = None;
    let mut body = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        match rest[index].value.as_slice() {
            b"content-type" if content_type.is_none() => {
                content_type = Some(rest.get(index + 1)?.value.clone());
                index += 2;
            }
            b"string" if body.is_empty() => {
                body.clone_from(&rest.get(index + 1)?.value);
                index += 2;
            }
            _ => return None,
        }
    }
    Some(HttpRequestRule::FixedResponse {
        status,
        body,
        content_type,
        condition: None,
    })
}

pub(super) fn parse_http_response_rule(arguments: &[Word]) -> Option<HttpResponseRule> {
    match arguments {
        [action, name, value] if action.value == b"set-header" && !value.value.contains(&b'%') => {
            Some(HttpResponseRule::SetHeader {
                name: name.value.clone(),
                value: value.value.clone(),
            })
        }
        [action, name] if action.value == b"del-header" => Some(HttpResponseRule::RemoveHeader {
            name: name.value.clone(),
        }),
        _ => None,
    }
}

fn parse_http_header_value(value: &[u8]) -> Option<HttpHeaderValue> {
    match value {
        b"%[src]" => Some(HttpHeaderValue::ClientIp),
        b"%[req.hdr(host)]" | b"%[hdr(host)]" => Some(HttpHeaderValue::IncomingAuthority),
        value if !value.contains(&b'%') => Some(HttpHeaderValue::Literal(value.to_vec())),
        _ => None,
    }
}

pub(super) fn parse_forward_for(arguments: &[Word]) -> Option<ForwardFor> {
    let mut forward_for = ForwardFor {
        except: None,
        header: None,
        if_none: false,
    };
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].value.as_slice() {
            b"except" if forward_for.except.is_none() => {
                forward_for.except = Some(arguments.get(index + 1)?.value.clone());
                index += 2;
            }
            b"header" if forward_for.header.is_none() => {
                forward_for.header = Some(arguments.get(index + 1)?.value.clone());
                index += 2;
            }
            b"if-none" if !forward_for.if_none => {
                forward_for.if_none = true;
                index += 1;
            }
            _ => return None,
        }
    }
    Some(forward_for)
}

pub(super) fn parse_http_check(arguments: &[Word]) -> Option<HttpCheck> {
    match arguments {
        [] => Some(HttpCheck {
            method: b"OPTIONS".to_vec(),
            uri: b"/".to_vec(),
            version: b"HTTP/1.0".to_vec(),
            host: None,
        }),
        [uri] if uri.value.starts_with(b"/") || uri.value == b"*" => Some(HttpCheck {
            method: b"OPTIONS".to_vec(),
            uri: uri.value.clone(),
            version: b"HTTP/1.0".to_vec(),
            host: None,
        }),
        [method, uri] => Some(HttpCheck {
            method: method.value.clone(),
            uri: uri.value.clone(),
            version: b"HTTP/1.0".to_vec(),
            host: None,
        }),
        [method, uri, version] => Some(HttpCheck {
            method: method.value.clone(),
            uri: uri.value.clone(),
            version: version.value.clone(),
            host: None,
        }),
        [method, uri, version, host] => Some(HttpCheck {
            method: method.value.clone(),
            uri: uri.value.clone(),
            version: version.value.clone(),
            host: Some(host.value.clone()),
        }),
        _ => None,
    }
}

pub(super) fn parse_http_check_send(arguments: &[Word]) -> Option<HttpCheck> {
    let mut method = None;
    let mut uri = None;
    let mut version = None;
    let mut host = None;
    let mut index = 0;

    while index < arguments.len() {
        match arguments[index].value.as_slice() {
            b"meth" if method.is_none() => {
                let value = arguments.get(index + 1)?;
                if !is_literal_http_check_value(&value.value) {
                    return None;
                }
                method = Some(value.value.clone());
                index += 2;
            }
            b"uri" if uri.is_none() => {
                let value = arguments.get(index + 1)?;
                if !is_literal_http_check_value(&value.value) {
                    return None;
                }
                uri = Some(value.value.clone());
                index += 2;
            }
            b"ver" if version.is_none() => {
                let value = arguments.get(index + 1)?;
                if !is_literal_http_check_value(&value.value) {
                    return None;
                }
                version = Some(value.value.clone());
                index += 2;
            }
            b"hdr" => {
                let name = arguments.get(index + 1)?;
                let value = arguments.get(index + 2)?;
                if !name.value.eq_ignore_ascii_case(b"host")
                    || host.is_some()
                    || !is_literal_http_check_value(&value.value)
                {
                    return None;
                }
                host = Some(value.value.clone());
                index += 3;
            }
            _ => return None,
        }
    }

    Some(HttpCheck {
        method: method.unwrap_or_else(|| b"GET".to_vec()),
        uri: uri.unwrap_or_else(|| b"/".to_vec()),
        version: version.unwrap_or_else(|| b"HTTP/1.0".to_vec()),
        host,
    })
}

fn is_literal_http_check_value(value: &[u8]) -> bool {
    !value.is_empty() && !value.contains(&b'$') && !value.windows(2).any(|window| window == b"%[")
}

pub(super) fn parse_status_ranges(value: &[u8]) -> Option<Vec<StatusRange>> {
    let mut ranges = Vec::new();
    for item in value.split(|byte| *byte == b',') {
        let mut parts = item.split(|byte| *byte == b'-');
        let start = parse_u16(parts.next()?)?;
        let end = parts.next().map_or(Some(start), parse_u16)?;
        if parts.next().is_some() || !(100..=599).contains(&start) || !(start..=599).contains(&end)
        {
            return None;
        }
        ranges.push(StatusRange { start, end });
    }
    (!ranges.is_empty()).then_some(ranges)
}
