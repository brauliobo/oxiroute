use serde_json::{Map, Value, json};

use crate::ConfigSourceError;

use super::actions::typed_err;

pub(super) fn parse_literal_headers(
    context: &str,
    value: Value,
) -> Result<Vec<Value>, ConfigSourceError> {
    match value {
        Value::Array(items) => {
            for item in &items {
                if !item.is_object() {
                    return Err(typed_err(format!(
                        "{context}: header array entries must be objects"
                    )));
                }
            }
            Ok(items)
        }
        Value::Object(map) => {
            let mut entries: Vec<(String, String)> = Vec::with_capacity(map.len());
            for (name, val) in map {
                let Value::String(header_val) = val else {
                    return Err(typed_err(format!(
                        "{context}: header `{name}` value must be a string"
                    )));
                };
                entries.push((name, header_val));
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Ok(entries
                .into_iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect())
        }
        _ => Err(typed_err(format!(
            "{context} must be a header map or an array of header objects"
        ))),
    }
}

pub(super) fn apply_proxy_route_headers(
    context: &str,
    policy: &mut Map<String, Value>,
    headers: Value,
) -> Result<(), ConfigSourceError> {
    match headers {
        Value::Object(map) if map.is_empty() => Ok(()),
        Value::Object(mut map)
            if map
                .keys()
                .all(|key| matches!(key.as_str(), "request" | "response"))
                && map.values().all(|val| val.is_object() || val.is_array()) =>
        {
            if let Some(req) = map.remove("request") {
                let mutations =
                    parse_proxy_request_headers(&format!("{context}.headers.request"), req)?;
                extend_policy_array(policy, "request_headers", mutations, context)?;
            }
            if let Some(resp) = map.remove("response") {
                let mutations =
                    parse_proxy_response_headers(&format!("{context}.headers.response"), resp)?;
                extend_policy_array(policy, "response_headers", mutations, context)?;
            }
            Ok(())
        }
        other => {
            let mutations = parse_proxy_response_headers(&format!("{context}.headers"), other)?;
            extend_policy_array(policy, "response_headers", mutations, context)
        }
    }
}

fn parse_proxy_request_headers(
    context: &str,
    value: Value,
) -> Result<Vec<Value>, ConfigSourceError> {
    match value {
        Value::Array(items) => Ok(items),
        Value::Object(map) => {
            let mut entries: Vec<(String, String)> = Vec::with_capacity(map.len());
            for (name, val) in map {
                let Value::String(header_val) = val else {
                    return Err(typed_err(format!(
                        "{context}: header `{name}` value must be a string"
                    )));
                };
                entries.push((name, header_val));
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Ok(entries
                .into_iter()
                .map(|(name, value)| {
                    json!({
                        "operation": "set",
                        "name": name,
                        "value": { "type": "literal", "value": value },
                    })
                })
                .collect())
        }
        _ => Err(typed_err(format!(
            "{context} must be a header map or an array of header mutations"
        ))),
    }
}

fn parse_proxy_response_headers(
    context: &str,
    value: Value,
) -> Result<Vec<Value>, ConfigSourceError> {
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Value::Object(mut obj) = item else {
                    return Err(typed_err(format!(
                        "{context}: header entries must be objects"
                    )));
                };
                if !obj.contains_key("operation") {
                    obj.insert("operation".to_owned(), Value::String("set".to_owned()));
                }
                out.push(Value::Object(obj));
            }
            Ok(out)
        }
        Value::Object(map) => {
            let mut entries: Vec<(String, String)> = Vec::with_capacity(map.len());
            for (name, val) in map {
                let Value::String(header_val) = val else {
                    return Err(typed_err(format!(
                        "{context}: header `{name}` value must be a string"
                    )));
                };
                entries.push((name, header_val));
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Ok(entries
                .into_iter()
                .map(|(name, value)| {
                    json!({
                        "operation": "set",
                        "name": name,
                        "value": value,
                    })
                })
                .collect())
        }
        _ => Err(typed_err(format!(
            "{context} must be a header map or an array of header objects"
        ))),
    }
}

fn extend_policy_array(
    policy: &mut Map<String, Value>,
    field: &str,
    items: Vec<Value>,
    context: &str,
) -> Result<(), ConfigSourceError> {
    if items.is_empty() {
        return Ok(());
    }
    match policy.get_mut(field) {
        None | Some(Value::Null) => {
            policy.insert(field.to_owned(), Value::Array(items));
            Ok(())
        }
        Some(Value::Array(existing)) => {
            existing.extend(items);
            Ok(())
        }
        Some(_) => Err(typed_err(format!(
            "{context}: `proxy_policy.{field}` must be an array"
        ))),
    }
}
