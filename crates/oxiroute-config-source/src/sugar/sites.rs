use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::ConfigSourceError;

use super::actions::{
    RouteActionSpec, build_route_action, merge_json_objects, parse_path_selector,
    parse_string_or_list, parse_u64, typed_err,
};
use super::cert::{SiteCert, parse_site_cert};

const DEFAULT_GZIP_LEVEL: u64 = 1;
const DEFAULT_GZIP_CONTENT_TYPES: &[&str] = &[
    "text/html",
    "text/css",
    "text/javascript",
    "application/javascript",
    "application/json",
    "text/plain",
    "image/svg+xml",
    "application/xml",
];

#[derive(Clone, Debug, Default)]
pub(super) struct ParsedSites {
    pub(super) certificates: Vec<Value>,
    pub(super) tls_certificates: Vec<String>,
    pub(super) upstream_pools: Vec<Value>,
    pub(super) https_routes: Vec<Value>,
    pub(super) http_only_routes: Vec<Value>,
    pub(super) https_gzip: Option<GzipChoice>,
    pub(super) http_only_gzip: Option<GzipChoice>,
}

#[derive(Clone, Debug)]
pub(super) enum GzipChoice {
    Default,
    Custom(Map<String, Value>),
}

impl GzipChoice {
    pub(super) fn to_value(&self) -> Value {
        match self {
            Self::Default => json!({
                "level": DEFAULT_GZIP_LEVEL,
                "content_types": DEFAULT_GZIP_CONTENT_TYPES,
            }),
            Self::Custom(map) => Value::Object(map.clone()),
        }
    }
}

pub(super) struct HostEntry {
    pub(super) raw: Option<String>,
    pub(super) selector: Value,
}

struct SiteFields {
    hosts: Vec<HostEntry>,
    cert: Option<Value>,
    root_action: Option<RouteActionSpec>,
    routes: Vec<Value>,
    gzip: Option<GzipChoice>,
    http_only: bool,
    access_policy: Option<Value>,
    max_request_body_bytes: Option<Value>,
    proxy_policy: Option<Map<String, Value>>,
}

struct SubRouteFields {
    path: Value,
    action: RouteActionSpec,
    methods: Option<Value>,
    headers: Option<Value>,
    access_policy: Option<Value>,
    route_policy: Option<Map<String, Value>>,
    max_body_override: Option<Value>,
    proxy_policy_override: Option<Map<String, Value>>,
}

pub(super) fn parse(value: Value) -> Result<ParsedSites, ConfigSourceError> {
    let entries = collect_site_entries(value)?;
    let mut out = ParsedSites::default();
    let mut seen_names = HashSet::new();
    let mut seen_tls_certs = HashSet::new();
    let mut catch_all_https = false;
    let mut catch_all_http_only = false;

    for (name, site_map) in entries {
        if name.trim().is_empty() {
            return Err(typed_err("site name must not be empty"));
        }
        if !seen_names.insert(name.clone()) {
            return Err(typed_err(format!("duplicate site `{name}`")));
        }

        let fields = parse_site_fields(&name, site_map)?;
        if fields.hosts.is_empty() {
            let slot = if fields.http_only {
                &mut catch_all_http_only
            } else {
                &mut catch_all_https
            };
            if *slot {
                return Err(typed_err(format!(
                    "site `{name}`: at most one catch-all site without `host`/`hosts` is allowed"
                )));
            }
            *slot = true;
        }

        match parse_site_cert(&name, fields.cert.clone(), &fields.hosts, fields.http_only)? {
            SiteCert::None => {}
            SiteCert::Reference(cert_name) => {
                if seen_tls_certs.insert(cert_name.clone()) {
                    out.tls_certificates.push(cert_name);
                }
            }
            SiteCert::Generate(cert_val, cert_name) => {
                out.certificates.push(cert_val);
                if seen_tls_certs.insert(cert_name.clone()) {
                    out.tls_certificates.push(cert_name);
                }
            }
        }

        let site_routes = build_site_routes(&name, &fields, &mut out.upstream_pools)?;
        if fields.http_only {
            out.http_only_routes.extend(site_routes);
            merge_gzip_slot(&mut out.http_only_gzip, fields.gzip);
        } else {
            out.https_routes.extend(site_routes);
            merge_gzip_slot(&mut out.https_gzip, fields.gzip);
        }
    }

    Ok(out)
}

type SiteEntry = (String, Map<String, Value>);

fn collect_site_entries(value: Value) -> Result<Vec<SiteEntry>, ConfigSourceError> {
    match value {
        Value::Object(map) => {
            let mut entries = Vec::with_capacity(map.len());
            for (name, val) in map {
                let Value::Object(mut site_map) = val else {
                    return Err(typed_err(format!("site `{name}` must be an object")));
                };
                if let Some(explicit_name) = site_map.remove("name") {
                    let Value::String(explicit_name) = explicit_name else {
                        return Err(typed_err(format!("site `{name}`: `name` must be a string")));
                    };
                    if explicit_name != name {
                        return Err(typed_err(format!(
                            "site `{name}`: `name` field `{explicit_name}` does not match map key"
                        )));
                    }
                }
                entries.push((name, site_map));
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Ok(entries)
        }
        Value::Array(items) => {
            let mut entries = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                let Value::Object(mut site_map) = item else {
                    return Err(typed_err(format!("sites[{index}] must be an object")));
                };
                let Some(name_val) = site_map.remove("name") else {
                    return Err(typed_err(format!(
                        "sites[{index}] requires a string `name` field"
                    )));
                };
                let Value::String(name) = name_val else {
                    return Err(typed_err(format!("sites[{index}].name must be a string")));
                };
                entries.push((name, site_map));
            }
            Ok(entries)
        }
        _ => Err(typed_err("`sites` must be an object map or an array")),
    }
}

fn parse_site_fields(name: &str, map: Map<String, Value>) -> Result<SiteFields, ConfigSourceError> {
    let mut single_host: Option<Value> = None;
    let mut multi_hosts: Option<Value> = None;
    let mut fields = SiteFields {
        hosts: Vec::new(),
        cert: None,
        root_action: None,
        routes: Vec::new(),
        gzip: None,
        http_only: false,
        access_policy: None,
        max_request_body_bytes: None,
        proxy_policy: None,
    };

    for (key, val) in map {
        apply_site_field(
            name,
            &key,
            val,
            &mut single_host,
            &mut multi_hosts,
            &mut fields,
        )?;
    }

    if single_host.is_some() && multi_hosts.is_some() {
        return Err(typed_err(format!(
            "site `{name}`: specify either `host` or `hosts`, not both"
        )));
    }
    fields.hosts = match single_host.or(multi_hosts) {
        None => Vec::new(),
        Some(raw) => parse_host_entries(name, raw)?,
    };

    if fields.root_action.is_none() && fields.routes.is_empty() {
        return Err(typed_err(format!(
            "site `{name}`: must specify at least one action (`proxy`, `static`, `redirect`, `respond`) or non-empty `routes`"
        )));
    }
    Ok(fields)
}

fn apply_site_field(
    name: &str,
    key: &str,
    val: Value,
    single_host: &mut Option<Value>,
    multi_hosts: &mut Option<Value>,
    fields: &mut SiteFields,
) -> Result<(), ConfigSourceError> {
    match key {
        "host" => {
            if !val.is_null() {
                *single_host = Some(val);
            }
        }
        "hosts" => {
            if !val.is_null() {
                *multi_hosts = Some(val);
            }
        }
        "cert" => fields.cert = Some(val),
        "proxy" | "static" | "redirect" | "respond" => {
            if fields.root_action.is_some() {
                return Err(typed_err(format!(
                    "site `{name}`: cannot specify more than one root action (`proxy`, `static`, `redirect`, `respond`)"
                )));
            }
            fields.root_action = Some(RouteActionSpec::from_entry(key, val)?);
        }
        "routes" => match val {
            Value::Null => {}
            Value::Array(items) => fields.routes = items,
            Value::Object(empty) if empty.is_empty() => {}
            _ => {
                return Err(typed_err(format!(
                    "site `{name}`: `routes` must be an array"
                )));
            }
        },
        "gzip" => fields.gzip = parse_gzip(&format!("site `{name}`.gzip"), val)?,
        "http_only" => {
            let Value::Bool(flag) = val else {
                return Err(typed_err(format!(
                    "site `{name}`: `http_only` must be a boolean"
                )));
            };
            fields.http_only = flag;
        }
        "access_policy" => {
            if !val.is_null() {
                if !val.is_object() {
                    return Err(typed_err(format!(
                        "site `{name}`: `access_policy` must be an object"
                    )));
                }
                fields.access_policy = Some(val);
            }
        }
        "max_request_body_bytes" => {
            if !val.is_null() {
                let _ = parse_u64(&val, &format!("site `{name}`.max_request_body_bytes"))?;
            }
            fields.max_request_body_bytes = Some(val);
        }
        "proxy_policy" => {
            if !val.is_null() {
                let Value::Object(obj) = val else {
                    return Err(typed_err(format!(
                        "site `{name}`: `proxy_policy` must be an object"
                    )));
                };
                fields.proxy_policy = Some(obj);
            }
        }
        unknown => return Err(typed_err(format!("site `{name}`: unknown key `{unknown}`"))),
    }
    Ok(())
}

fn parse_host_entries(site_name: &str, value: Value) -> Result<Vec<HostEntry>, ConfigSourceError> {
    let items = match value {
        Value::String(text) => vec![Value::String(text)],
        Value::Object(obj) => vec![Value::Object(obj)],
        Value::Array(items) => {
            if items.is_empty() {
                return Err(typed_err(format!(
                    "site `{site_name}`: `hosts` must not be empty"
                )));
            }
            items
        }
        _ => {
            return Err(typed_err(format!(
                "site `{site_name}`: `host`/`hosts` must be a string or array of strings"
            )));
        }
    };

    let mut result = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(text) => {
                let selector = host_string_to_selector(&text, site_name)?;
                result.push(HostEntry {
                    raw: Some(text),
                    selector,
                });
            }
            Value::Object(obj) => {
                let raw = obj.get("value").and_then(Value::as_str).map(str::to_owned);
                result.push(HostEntry {
                    raw,
                    selector: Value::Object(obj),
                });
            }
            _ => {
                return Err(typed_err(format!(
                    "site `{site_name}`: host entries must be strings"
                )));
            }
        }
    }
    Ok(result)
}

fn host_string_to_selector(text: &str, site_name: &str) -> Result<Value, ConfigSourceError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(typed_err(format!(
            "site `{site_name}`: host must not be empty"
        )));
    }
    if let Some(suffix) = trimmed.strip_prefix("*.") {
        if suffix.is_empty() {
            return Err(typed_err(format!(
                "site `{site_name}`: wildcard host `{trimmed}` must include a domain suffix"
            )));
        }
        Ok(json!({ "kind": "nginx_leading_wildcard", "value": suffix }))
    } else if let Some(suffix) = trimmed.strip_prefix('.') {
        if suffix.is_empty() {
            return Err(typed_err(format!(
                "site `{site_name}`: leading-dot host `{trimmed}` must include a domain suffix"
            )));
        }
        Ok(json!({ "kind": "nginx_leading_dot", "value": suffix }))
    } else {
        Ok(json!({ "kind": "normalized_host", "value": trimmed }))
    }
}

fn parse_gzip(context: &str, value: Value) -> Result<Option<GzipChoice>, ConfigSourceError> {
    match value {
        Value::Null | Value::Bool(false) => Ok(None),
        Value::Bool(true) => Ok(Some(GzipChoice::Default)),
        Value::Object(mut map) => {
            for key in map.keys() {
                if !matches!(
                    key.as_str(),
                    "level"
                        | "content_types"
                        | "min_length_bytes"
                        | "min_http_version"
                        | "disable_on_via"
                        | "vary"
                ) {
                    return Err(typed_err(format!("{context}: unknown key `{key}`")));
                }
            }
            if !map.contains_key("level") {
                map.insert("level".to_owned(), Value::Number(DEFAULT_GZIP_LEVEL.into()));
            }
            if !map.contains_key("content_types") {
                map.insert(
                    "content_types".to_owned(),
                    json!(DEFAULT_GZIP_CONTENT_TYPES),
                );
            }
            Ok(Some(GzipChoice::Custom(map)))
        }
        _ => Err(typed_err(format!(
            "{context} must be a boolean or an object"
        ))),
    }
}

fn merge_gzip_slot(slot: &mut Option<GzipChoice>, next: Option<GzipChoice>) {
    let Some(next) = next else {
        return;
    };
    match (&*slot, &next) {
        (None | Some(GzipChoice::Default), _)
        | (Some(GzipChoice::Custom(_)), GzipChoice::Custom(_)) => *slot = Some(next),
        (Some(GzipChoice::Custom(_)), GzipChoice::Default) => {}
    }
}

fn build_site_routes(
    site_name: &str,
    fields: &SiteFields,
    pools: &mut Vec<Value>,
) -> Result<Vec<Value>, ConfigSourceError> {
    let mut templates = Vec::new();
    for (index, raw_route) in fields.routes.iter().enumerate() {
        let tpl = build_sub_route_template(site_name, index, raw_route.clone(), fields, pools)?;
        templates.push(tpl);
    }

    if let Some(root_action) = &fields.root_action {
        let pool_name = format!("site-{site_name}");
        let action_val = build_route_action(
            &format!("site `{site_name}`"),
            &pool_name,
            root_action.clone(),
            fields.proxy_policy.as_ref(),
            None,
            pools,
        )?;
        let mut route = Map::new();
        route.insert(
            "path".to_owned(),
            json!({ "kind": "segment_prefix", "value": "/" }),
        );
        if let Some(access_policy) = &fields.access_policy {
            route.insert("access_policy".to_owned(), access_policy.clone());
        }
        if let Some(max_body) = &fields.max_request_body_bytes {
            route.insert(
                "policy".to_owned(),
                json!({ "max_request_body_bytes": max_body }),
            );
        }
        route.insert("action".to_owned(), action_val);
        templates.push(route);
    }

    let mut routes = Vec::new();
    if fields.hosts.is_empty() {
        for tpl in templates {
            routes.push(Value::Object(tpl));
        }
    } else {
        for host in &fields.hosts {
            for tpl in &templates {
                let mut route = tpl.clone();
                route.insert("host".to_owned(), host.selector.clone());
                routes.push(Value::Object(route));
            }
        }
    }
    Ok(routes)
}

fn build_sub_route_template(
    site_name: &str,
    index: usize,
    raw_route: Value,
    fields: &SiteFields,
    pools: &mut Vec<Value>,
) -> Result<Map<String, Value>, ConfigSourceError> {
    let context = format!("site `{site_name}`.routes[{index}]");
    let parsed = parse_sub_route_fields(&context, raw_route)?;

    let merged_proxy_policy = match (fields.proxy_policy.as_ref(), parsed.proxy_policy_override) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over),
        (Some(base), Some(over)) => {
            let mut merged = base.clone();
            merge_json_objects(&mut merged, over);
            Some(merged)
        }
    };

    let pool_name = format!("site-{site_name}-route-{}", index + 1);
    let action = build_route_action(
        &context,
        &pool_name,
        parsed.action,
        merged_proxy_policy.as_ref(),
        parsed.headers,
        pools,
    )?;

    let mut route = Map::new();
    route.insert("path".to_owned(), parsed.path);
    if let Some(methods_raw) = parsed.methods {
        let methods = parse_string_or_list(methods_raw, &format!("{context}.methods"))?;
        if !methods.is_empty() {
            route.insert(
                "methods".to_owned(),
                Value::Array(methods.into_iter().map(Value::String).collect()),
            );
        }
    }
    if let Some(ap) = parsed
        .access_policy
        .or_else(|| fields.access_policy.clone())
    {
        route.insert("access_policy".to_owned(), ap);
    }

    let mut final_policy = Map::new();
    if let Some(site_max) = &fields.max_request_body_bytes {
        final_policy.insert("max_request_body_bytes".to_owned(), site_max.clone());
    }
    if let Some(rp) = parsed.route_policy {
        merge_json_objects(&mut final_policy, rp);
    }
    if let Some(route_max) = parsed.max_body_override {
        final_policy.insert("max_request_body_bytes".to_owned(), route_max);
    }
    if !final_policy.is_empty() {
        route.insert("policy".to_owned(), Value::Object(final_policy));
    }

    route.insert("action".to_owned(), action);
    Ok(route)
}

fn parse_sub_route_fields(
    context: &str,
    raw_route: Value,
) -> Result<SubRouteFields, ConfigSourceError> {
    let Value::Object(map) = raw_route else {
        return Err(typed_err(format!("{context} must be an object")));
    };

    let mut path_val: Option<Value> = None;
    let mut action_spec: Option<RouteActionSpec> = None;
    let mut methods: Option<Value> = None;
    let mut headers: Option<Value> = None;
    let mut access_policy: Option<Value> = None;
    let mut route_policy: Option<Map<String, Value>> = None;
    let mut max_body_override: Option<Value> = None;
    let mut proxy_policy_override: Option<Map<String, Value>> = None;

    for (key, val) in map {
        match key.as_str() {
            "path" => path_val = Some(val),
            "proxy" | "static" | "redirect" | "respond" => {
                if action_spec.is_some() {
                    return Err(typed_err(format!(
                        "{context}: cannot specify more than one action (`proxy`, `static`, `redirect`, `respond`)"
                    )));
                }
                action_spec = Some(RouteActionSpec::from_entry(&key, val)?);
            }
            "methods" if !val.is_null() => methods = Some(val),
            "headers" if !val.is_null() => headers = Some(val),
            "methods" | "headers" => {}
            "access_policy" => {
                if !val.is_null() {
                    if !val.is_object() {
                        return Err(typed_err(format!(
                            "{context}: `access_policy` must be an object"
                        )));
                    }
                    access_policy = Some(val);
                }
            }
            "policy" => {
                if !val.is_null() {
                    let Value::Object(obj) = val else {
                        return Err(typed_err(format!("{context}: `policy` must be an object")));
                    };
                    route_policy = Some(obj);
                }
            }
            "max_request_body_bytes" => {
                if !val.is_null() {
                    let _ = parse_u64(&val, &format!("{context}.max_request_body_bytes"))?;
                }
                max_body_override = Some(val);
            }
            "proxy_policy" => {
                if !val.is_null() {
                    let Value::Object(obj) = val else {
                        return Err(typed_err(format!(
                            "{context}: `proxy_policy` must be an object"
                        )));
                    };
                    proxy_policy_override = Some(obj);
                }
            }
            unknown => return Err(typed_err(format!("{context}: unknown key `{unknown}`"))),
        }
    }

    let Some(path_val) = path_val else {
        return Err(typed_err(format!("{context} requires `path`")));
    };
    let path = parse_path_selector(path_val, context)?;
    let Some(action) = action_spec else {
        return Err(typed_err(format!(
            "{context} requires an action (`proxy`, `static`, `redirect`, or `respond`)"
        )));
    };

    Ok(SubRouteFields {
        path,
        action,
        methods,
        headers,
        access_policy,
        route_policy,
        max_body_override,
        proxy_policy_override,
    })
}
