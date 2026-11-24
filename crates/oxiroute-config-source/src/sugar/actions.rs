use std::net::SocketAddr;

use serde_json::{Map, Value, json};

use crate::ConfigSourceError;

use super::headers::{apply_proxy_route_headers, parse_literal_headers};

const POOL_OPTION_KEYS: &[&str] = &[
    "algorithm",
    "health_check",
    "passive_health",
    "tls",
    "http_versions",
    "queue_timeout_ms",
    "connect_timeout_ms",
    "server_timeout_ms",
    "connection_reuse",
    "resilience",
    "queue",
    "registration",
    "canary",
];

const STATIC_PASSTHROUGH_KEYS: &[&str] = &[
    "path_mapping",
    "index_files",
    "internal_index_redirects",
    "directory_redirects",
    "try_files",
    "autoindex",
    "autoindex_exact_size",
    "autoindex_local_time",
    "etag",
    "mime",
    "error_responses",
];

#[derive(Clone, Debug)]
pub(super) enum RouteActionSpec {
    Proxy(Value),
    Static(Value),
    Redirect(Value),
    Respond(Value),
}

impl RouteActionSpec {
    pub(super) fn from_entry(key: &str, value: Value) -> Result<Self, ConfigSourceError> {
        match key {
            "proxy" => Ok(Self::Proxy(value)),
            "static" => Ok(Self::Static(value)),
            "redirect" => Ok(Self::Redirect(value)),
            "respond" => Ok(Self::Respond(value)),
            other => Err(typed_err(format!("unknown action `{other}`"))),
        }
    }
}

pub(super) fn typed_err(message: impl Into<String>) -> ConfigSourceError {
    ConfigSourceError::TypedConfig(message.into())
}

pub(super) fn parse_u64(value: &Value, field: &str) -> Result<u64, ConfigSourceError> {
    value
        .as_u64()
        .ok_or_else(|| typed_err(format!("{field} must be a non-negative integer")))
}

pub(super) fn parse_string_or_list(
    value: Value,
    field: &str,
) -> Result<Vec<String>, ConfigSourceError> {
    match value {
        Value::String(text) => Ok(vec![text]),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(text) = item else {
                    return Err(typed_err(format!("{field} entries must be strings")));
                };
                out.push(text);
            }
            Ok(out)
        }
        Value::Object(empty) if empty.is_empty() => Ok(Vec::new()),
        _ => Err(typed_err(format!(
            "{field} must be a string or an array of strings"
        ))),
    }
}

pub(super) fn merge_json_objects(target: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        if let (Some(Value::Object(target_obj)), Value::Object(overlay_obj)) =
            (target.get_mut(&key), &value)
        {
            merge_json_objects(target_obj, overlay_obj.clone());
        } else {
            target.insert(key, value);
        }
    }
}

pub(super) fn append_array(
    root: &mut Map<String, Value>,
    field: &str,
    items: Vec<Value>,
) -> Result<(), ConfigSourceError> {
    match root.get_mut(field) {
        None | Some(Value::Null) => {
            root.insert(field.to_owned(), Value::Array(items));
            Ok(())
        }
        Some(Value::Array(existing)) => {
            existing.extend(items);
            Ok(())
        }
        Some(Value::Object(empty)) if empty.is_empty() => {
            root.insert(field.to_owned(), Value::Array(items));
            Ok(())
        }
        Some(_) => Err(typed_err(format!("top-level `{field}` must be an array"))),
    }
}

pub(super) fn has_named_entry(
    root: &Map<String, Value>,
    field: &str,
    name: &str,
) -> Result<bool, ConfigSourceError> {
    match root.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Object(empty)) if empty.is_empty() => Ok(false),
        Some(Value::Array(items)) => Ok(items
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some(name))),
        Some(_) => Err(typed_err(format!("top-level `{field}` must be an array"))),
    }
}

pub(super) fn parse_path_selector(value: Value, context: &str) -> Result<Value, ConfigSourceError> {
    match value {
        Value::Object(obj) => Ok(Value::Object(obj)),
        Value::String(text) => {
            if let Some(exact) = text.strip_prefix('=') {
                Ok(json!({ "kind": "exact", "value": exact }))
            } else if let Some(raw) = text.strip_prefix('^') {
                Ok(json!({ "kind": "raw_prefix", "value": raw }))
            } else {
                Ok(json!({ "kind": "segment_prefix", "value": text }))
            }
        }
        _ => Err(typed_err(format!(
            "{context}: `path` must be a string or path selector object"
        ))),
    }
}

pub(super) fn build_route_action(
    context: &str,
    generated_pool_name: &str,
    spec: RouteActionSpec,
    base_proxy_policy: Option<&Map<String, Value>>,
    route_headers: Option<Value>,
    pools: &mut Vec<Value>,
) -> Result<Value, ConfigSourceError> {
    match spec {
        RouteActionSpec::Proxy(val) => build_proxy_action(
            context,
            generated_pool_name,
            val,
            base_proxy_policy,
            route_headers,
            pools,
        ),
        RouteActionSpec::Static(val) => build_static_action(context, val, route_headers),
        RouteActionSpec::Redirect(val) => build_redirect_action(context, val, route_headers),
        RouteActionSpec::Respond(val) => build_respond_action(context, val, route_headers),
    }
}

fn build_proxy_action(
    context: &str,
    generated_pool_name: &str,
    value: Value,
    base_proxy_policy: Option<&Map<String, Value>>,
    route_headers: Option<Value>,
    pools: &mut Vec<Value>,
) -> Result<Value, ConfigSourceError> {
    let mut policy = base_proxy_policy.cloned().unwrap_or_default();
    let pool_name = match value {
        Value::String(target) => {
            if let Some(existing) = target.strip_prefix('@') {
                if existing.trim().is_empty() {
                    return Err(typed_err(format!(
                        "{context}: proxy pool reference `{target}` must include a pool name"
                    )));
                }
                existing.to_owned()
            } else {
                let pool = build_upstream_pool(
                    context,
                    generated_pool_name,
                    vec![Value::String(target)],
                    Map::new(),
                )?;
                pools.push(pool);
                generated_pool_name.to_owned()
            }
        }
        Value::Array(targets) => {
            let pool = build_upstream_pool(context, generated_pool_name, targets, Map::new())?;
            pools.push(pool);
            generated_pool_name.to_owned()
        }
        Value::Object(obj) => {
            resolve_proxy_object(context, generated_pool_name, obj, &mut policy, pools)?
        }
        _ => {
            return Err(typed_err(format!(
                "{context}: `proxy` must be a target string, array of targets, or object"
            )));
        }
    };

    if let Some(headers) = route_headers {
        apply_proxy_route_headers(context, &mut policy, headers)?;
    }

    Ok(json!({
        "type": "proxy",
        "upstream_pool": pool_name,
        "policy": Value::Object(policy),
    }))
}

fn resolve_proxy_object(
    context: &str,
    generated_pool_name: &str,
    mut obj: Map<String, Value>,
    policy: &mut Map<String, Value>,
    pools: &mut Vec<Value>,
) -> Result<String, ConfigSourceError> {
    let Some(to_val) = obj.remove("to") else {
        return Err(typed_err(format!(
            "{context}: `proxy` object requires `to`"
        )));
    };
    if let Some(action_policy) = obj.remove("policy")
        && !action_policy.is_null()
    {
        let Value::Object(policy_obj) = action_policy else {
            return Err(typed_err(format!(
                "{context}: `proxy.policy` must be an object"
            )));
        };
        merge_json_objects(policy, policy_obj);
    }

    let mut pool_opts = Map::new();
    for (key, val) in obj {
        if POOL_OPTION_KEYS.contains(&key.as_str()) {
            pool_opts.insert(key, val);
        } else {
            return Err(typed_err(format!(
                "{context}: unknown key `{key}` in `proxy` object"
            )));
        }
    }

    if let Some(existing) = to_val.as_str().and_then(|s| s.strip_prefix('@')) {
        if existing.trim().is_empty() {
            return Err(typed_err(format!(
                "{context}: proxy pool reference `@{existing}` must include a pool name"
            )));
        }
        if !pool_opts.is_empty() {
            return Err(typed_err(format!(
                "{context}: cannot set pool options when referencing existing pool `@{existing}`"
            )));
        }
        return Ok(existing.to_owned());
    }

    let raw_servers = match to_val {
        Value::String(s) => vec![Value::String(s)],
        Value::Array(items) => items,
        Value::Object(server_obj) => vec![Value::Object(server_obj)],
        _ => {
            return Err(typed_err(format!(
                "{context}: `proxy.to` must be an endpoint string, server object, or array"
            )));
        }
    };
    let pool = build_upstream_pool(context, generated_pool_name, raw_servers, pool_opts)?;
    pools.push(pool);
    Ok(generated_pool_name.to_owned())
}

fn build_upstream_pool(
    context: &str,
    pool_name: &str,
    raw_servers: Vec<Value>,
    pool_opts: Map<String, Value>,
) -> Result<Value, ConfigSourceError> {
    if raw_servers.is_empty() {
        return Err(typed_err(format!(
            "{context}: `proxy` target list must not be empty"
        )));
    }

    let mut servers = Vec::with_capacity(raw_servers.len());
    for (offset, item) in raw_servers.into_iter().enumerate() {
        servers.push(build_upstream_server(context, item, offset + 1)?);
    }

    let mut pool = Map::new();
    pool.insert("name".to_owned(), Value::String(pool_name.to_owned()));
    pool.insert("servers".to_owned(), Value::Array(servers));
    for (key, val) in pool_opts {
        pool.insert(key, val);
    }
    Ok(Value::Object(pool))
}

fn build_upstream_server(
    context: &str,
    value: Value,
    index: usize,
) -> Result<Value, ConfigSourceError> {
    let default_name = format!("s{index}");
    match value {
        Value::String(spec) => {
            let endpoint = parse_upstream_endpoint(context, &spec)?;
            Ok(json!({ "name": default_name, "endpoint": endpoint }))
        }
        Value::Object(mut obj) => {
            if !obj.contains_key("name") {
                obj.insert("name".to_owned(), Value::String(default_name));
            }
            if let Some(Value::String(spec)) = obj.get("endpoint") {
                let endpoint = parse_upstream_endpoint(context, spec)?;
                obj.insert("endpoint".to_owned(), endpoint);
            }
            Ok(Value::Object(obj))
        }
        _ => Err(typed_err(format!(
            "{context}: each proxy server must be an endpoint string or server object"
        ))),
    }
}

fn parse_upstream_endpoint(context: &str, spec: &str) -> Result<Value, ConfigSourceError> {
    if let Some(path) = spec.strip_prefix("unix:") {
        if path.is_empty() {
            return Err(typed_err(format!(
                "{context}: unix upstream endpoint path must not be empty"
            )));
        }
        return Ok(json!({ "type": "unix", "path": path }));
    }
    if spec.starts_with('/') {
        return Ok(json!({ "type": "unix", "path": spec }));
    }
    if spec.parse::<SocketAddr>().is_ok() {
        return Ok(json!({ "type": "socket", "address": spec }));
    }
    if let Some((host, port_str)) = spec.rsplit_once(':')
        && !host.is_empty()
        && let Ok(port) = port_str.parse::<u16>()
    {
        return Ok(json!({ "type": "dns", "host": host, "port": port }));
    }
    Err(typed_err(format!(
        "{context}: invalid upstream endpoint `{spec}` (expected socket `ip:port`, DNS `host:port`, or unix `/path`)"
    )))
}

fn build_static_action(
    context: &str,
    value: Value,
    route_headers: Option<Value>,
) -> Result<Value, ConfigSourceError> {
    let mut action = Map::new();
    action.insert("type".to_owned(), Value::String("static_files".to_owned()));
    let mut headers = Vec::new();

    match value {
        Value::String(root) => {
            action.insert("root_directory".to_owned(), Value::String(root));
        }
        Value::Object(obj) => {
            populate_static_object(context, obj, &mut action, &mut headers)?;
        }
        _ => {
            return Err(typed_err(format!(
                "{context}: `static` must be a root directory string or an object"
            )));
        }
    }

    if let Some(extra_headers) = route_headers {
        headers.extend(parse_literal_headers(
            &format!("{context}.headers"),
            extra_headers,
        )?);
    }
    if !headers.is_empty() {
        action.insert("headers".to_owned(), Value::Array(headers));
    }
    Ok(Value::Object(action))
}

fn populate_static_object(
    context: &str,
    mut obj: Map<String, Value>,
    action: &mut Map<String, Value>,
    headers: &mut Vec<Value>,
) -> Result<(), ConfigSourceError> {
    let root_short = obj.remove("root");
    let root_long = obj.remove("root_directory");
    if root_short.is_some() && root_long.is_some() {
        return Err(typed_err(format!(
            "{context}: `static` cannot specify both `root` and `root_directory`"
        )));
    }
    let Some(Value::String(root)) = root_short.or(root_long) else {
        return Err(typed_err(format!(
            "{context}: `static` requires a string `root` (or `root_directory`)"
        )));
    };
    action.insert("root_directory".to_owned(), Value::String(root));

    let spa_short = obj.remove("spa");
    let spa_long = obj.remove("spa_fallback");
    if spa_short.is_some() && spa_long.is_some() {
        return Err(typed_err(format!(
            "{context}: `static` cannot specify both `spa` and `spa_fallback`"
        )));
    }
    if let Some(spa) = spa_short {
        match spa {
            Value::Null | Value::Bool(false) => {}
            Value::Bool(true) => {
                action.insert(
                    "spa_fallback".to_owned(),
                    Value::String("index.html".to_owned()),
                );
            }
            Value::String(path) => {
                action.insert("spa_fallback".to_owned(), Value::String(path));
            }
            _ => {
                return Err(typed_err(format!(
                    "{context}: `static.spa` must be a boolean or relative file path"
                )));
            }
        }
    } else if let Some(spa_fallback) = spa_long
        && !spa_fallback.is_null()
    {
        action.insert("spa_fallback".to_owned(), spa_fallback);
    }

    if let Some(action_headers) = obj.remove("headers")
        && !action_headers.is_null()
    {
        headers.extend(parse_literal_headers(
            &format!("{context}.static.headers"),
            action_headers,
        )?);
    }

    for (key, val) in obj {
        if STATIC_PASSTHROUGH_KEYS.contains(&key.as_str()) {
            action.insert(key, val);
        } else {
            return Err(typed_err(format!(
                "{context}: unknown key `{key}` in `static` object"
            )));
        }
    }
    Ok(())
}

fn build_redirect_action(
    context: &str,
    value: Value,
    route_headers: Option<Value>,
) -> Result<Value, ConfigSourceError> {
    let mut status: u64 = 308;
    let mut headers = Vec::new();
    let location = match value {
        Value::String(target) => build_redirect_location(&target, None),
        Value::Object(obj) => parse_redirect_object(context, obj, &mut status, &mut headers)?,
        _ => {
            return Err(typed_err(format!(
                "{context}: `redirect` must be a location string or an object"
            )));
        }
    };

    if let Some(extra_headers) = route_headers {
        headers.extend(parse_literal_headers(
            &format!("{context}.headers"),
            extra_headers,
        )?);
    }

    let mut action = Map::new();
    action.insert("type".to_owned(), Value::String("redirect".to_owned()));
    action.insert("status".to_owned(), Value::Number(status.into()));
    action.insert("location".to_owned(), location);
    if !headers.is_empty() {
        action.insert("headers".to_owned(), Value::Array(headers));
    }
    Ok(Value::Object(action))
}

fn parse_redirect_object(
    context: &str,
    mut obj: Map<String, Value>,
    status: &mut u64,
    headers: &mut Vec<Value>,
) -> Result<Value, ConfigSourceError> {
    let to_short = obj.remove("to");
    let to_long = obj.remove("location");
    if to_short.is_some() && to_long.is_some() {
        return Err(typed_err(format!(
            "{context}: `redirect` cannot specify both `to` and `location`"
        )));
    }
    let Some(loc_val) = to_short.or(to_long) else {
        return Err(typed_err(format!(
            "{context}: `redirect` object requires `to` (or `location`)"
        )));
    };

    if let Some(status_val) = obj.remove("status")
        && !status_val.is_null()
    {
        *status = parse_u64(&status_val, &format!("{context}.redirect.status"))?;
    }
    let fallback = match obj.remove("nginx_host_fallback") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text),
        Some(_) => {
            return Err(typed_err(format!(
                "{context}: `redirect.nginx_host_fallback` must be a string"
            )));
        }
    };
    if let Some(action_headers) = obj.remove("headers")
        && !action_headers.is_null()
    {
        headers.extend(parse_literal_headers(
            &format!("{context}.redirect.headers"),
            action_headers,
        )?);
    }
    if let Some((unknown, _)) = obj.into_iter().next() {
        return Err(typed_err(format!(
            "{context}: unknown key `{unknown}` in `redirect` object"
        )));
    }

    match loc_val {
        Value::String(target) => Ok(build_redirect_location(&target, fallback)),
        Value::Object(loc_obj) => {
            if fallback.is_some() {
                return Err(typed_err(format!(
                    "{context}: put `nginx_host_fallback` inside the `location` object"
                )));
            }
            Ok(Value::Object(loc_obj))
        }
        _ => Err(typed_err(format!(
            "{context}: `redirect.to` must be a string or location object"
        ))),
    }
}

fn build_redirect_location(target: &str, nginx_host_fallback: Option<String>) -> Value {
    let is_template =
        target.contains("$scheme") || target.contains("$host") || target.contains("$request_uri");
    let mut obj = Map::new();
    if is_template {
        obj.insert(
            "kind".to_owned(),
            Value::String("request_template".to_owned()),
        );
        obj.insert("value".to_owned(), Value::String(target.to_owned()));
        if let Some(fallback) = nginx_host_fallback {
            obj.insert("nginx_host_fallback".to_owned(), Value::String(fallback));
        }
    } else {
        obj.insert("kind".to_owned(), Value::String("literal".to_owned()));
        obj.insert("value".to_owned(), Value::String(target.to_owned()));
    }
    Value::Object(obj)
}

fn build_respond_action(
    context: &str,
    value: Value,
    route_headers: Option<Value>,
) -> Result<Value, ConfigSourceError> {
    let Value::Object(mut obj) = value else {
        return Err(typed_err(format!(
            "{context}: `respond` must be an object with `status`"
        )));
    };
    let Some(status_val) = obj.remove("status") else {
        return Err(typed_err(format!("{context}: `respond` requires `status`")));
    };
    let status = parse_u64(&status_val, &format!("{context}.respond.status"))?;

    let body = match obj.remove("body") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text,
        Some(_) => {
            return Err(typed_err(format!(
                "{context}: `respond.body` must be a string"
            )));
        }
    };

    let mut headers = Vec::new();
    if let Some(action_headers) = obj.remove("headers")
        && !action_headers.is_null()
    {
        headers.extend(parse_literal_headers(
            &format!("{context}.respond.headers"),
            action_headers,
        )?);
    }
    if let Some((unknown, _)) = obj.into_iter().next() {
        return Err(typed_err(format!(
            "{context}: unknown key `{unknown}` in `respond` object"
        )));
    }

    if let Some(extra_headers) = route_headers {
        headers.extend(parse_literal_headers(
            &format!("{context}.headers"),
            extra_headers,
        )?);
    }

    let mut action = Map::new();
    action.insert(
        "type".to_owned(),
        Value::String("fixed_response".to_owned()),
    );
    action.insert("status".to_owned(), Value::Number(status.into()));
    action.insert("body".to_owned(), Value::String(body));
    if !headers.is_empty() {
        action.insert("headers".to_owned(), Value::Array(headers));
    }
    Ok(Value::Object(action))
}
