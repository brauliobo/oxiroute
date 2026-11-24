use std::{collections::HashSet, net::SocketAddr};

use serde_json::{Map, Value};

use crate::ConfigSourceError;

use super::actions::{parse_string_or_list, parse_u64, typed_err};

const DEFAULT_STATS_URI_PREFIX: &str = "/stats";
const DEFAULT_STATS_REFRESH_MS: u64 = 2_000;
const DEFAULT_STATS_ADMIN: &str = "disabled";

#[derive(Clone, Debug, Default)]
pub(super) struct EdgeSpec {
    pub(super) http: Vec<ParsedBind>,
    pub(super) https: Vec<ParsedBind>,
    pub(super) redirect_https: bool,
    pub(super) tls_profile: Option<String>,
    pub(super) min_version: Option<String>,
    pub(super) alpn: Option<Vec<String>>,
    pub(super) default_cert: Option<String>,
    pub(super) stats: Option<EdgeStats>,
    pub(super) management: Option<Value>,
    pub(super) max_connections: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) enum ParsedBind {
    Socket(SocketAddr),
    Unix(String),
}

#[derive(Clone, Debug)]
pub(super) enum EdgeStats {
    Page(Value),
    Full(Map<String, Value>),
}

pub(super) fn parse(value: Value) -> Result<EdgeSpec, ConfigSourceError> {
    let Value::Object(map) = value else {
        return Err(typed_err("`edge` must be an object"));
    };

    let mut spec = EdgeSpec::default();
    for (key, val) in map {
        match key.as_str() {
            "http" => {
                if !val.is_null() {
                    let raw = parse_string_or_list(val, "edge.http")?;
                    spec.http = parse_binds(&raw, "edge.http")?;
                }
            }
            "https" => {
                if !val.is_null() {
                    let raw = parse_string_or_list(val, "edge.https")?;
                    spec.https = parse_binds(&raw, "edge.https")?;
                }
            }
            "redirect_https" => {
                let Value::Bool(flag) = val else {
                    return Err(typed_err("edge.redirect_https must be a boolean"));
                };
                spec.redirect_https = flag;
            }
            "tls_profile" => {
                if !val.is_null() {
                    spec.tls_profile = Some(parse_non_empty_str(val, "edge.tls_profile")?);
                }
            }
            "min_version" => {
                if !val.is_null() {
                    let version = parse_non_empty_str(val, "edge.min_version")?;
                    if !matches!(version.as_str(), "1.2" | "1.3") {
                        return Err(typed_err(format!(
                            "edge.min_version must be `1.2` or `1.3`, got `{version}`"
                        )));
                    }
                    spec.min_version = Some(version);
                }
            }
            "alpn" => {
                if !val.is_null() {
                    let list = parse_string_or_list(val, "edge.alpn")?;
                    if list.is_empty() {
                        return Err(typed_err("edge.alpn must not be empty"));
                    }
                    spec.alpn = Some(list);
                }
            }
            "default_cert" => {
                if !val.is_null() {
                    spec.default_cert = Some(parse_non_empty_str(val, "edge.default_cert")?);
                }
            }
            "stats" => {
                if !val.is_null() {
                    spec.stats = Some(parse_edge_stats(val)?);
                }
            }
            "management" => {
                if !val.is_null() {
                    spec.management = Some(parse_edge_management(val)?);
                }
            }
            "max_connections" => {
                if !val.is_null() {
                    spec.max_connections = Some(parse_u64(&val, "edge.max_connections")?);
                }
            }
            unknown => {
                return Err(typed_err(format!("edge: unknown key `{unknown}`")));
            }
        }
    }

    Ok(spec)
}

fn parse_non_empty_str(value: Value, field: &str) -> Result<String, ConfigSourceError> {
    let Value::String(text) = value else {
        return Err(typed_err(format!("{field} must be a string")));
    };
    if text.trim().is_empty() {
        return Err(typed_err(format!("{field} must not be empty")));
    }
    Ok(text)
}

fn parse_binds(raw: &[String], field: &str) -> Result<Vec<ParsedBind>, ConfigSourceError> {
    let mut binds = Vec::with_capacity(raw.len());
    for entry in raw {
        if let Some(path) = entry.strip_prefix("unix:") {
            if path.is_empty() {
                return Err(typed_err(format!(
                    "{field}: unix bind path must not be empty"
                )));
            }
            binds.push(ParsedBind::Unix(path.to_owned()));
        } else if entry.starts_with('/') {
            binds.push(ParsedBind::Unix(entry.clone()));
        } else {
            let addr = entry.parse::<SocketAddr>().map_err(|_| {
                typed_err(format!(
                    "{field}: invalid socket address `{entry}` (expected `ip:port` such as `0.0.0.0:80` or `[::]:443`)"
                ))
            })?;
            binds.push(ParsedBind::Socket(addr));
        }
    }
    Ok(binds)
}

fn parse_edge_stats(value: Value) -> Result<EdgeStats, ConfigSourceError> {
    match value {
        Value::String(spec) => {
            let (bind, uri_prefix) = split_stats_shorthand(&spec)?;
            let mut page = Map::new();
            page.insert("bind".to_owned(), Value::String(bind));
            page.insert("uri_prefix".to_owned(), Value::String(uri_prefix));
            page.insert(
                "refresh_ms".to_owned(),
                Value::Number(DEFAULT_STATS_REFRESH_MS.into()),
            );
            page.insert(
                "admin".to_owned(),
                Value::String(DEFAULT_STATS_ADMIN.to_owned()),
            );
            Ok(EdgeStats::Page(Value::Object(page)))
        }
        Value::Object(map) => {
            let is_full_stats = map.contains_key("binds")
                || map.contains_key("pages")
                || map.contains_key("admin_token_file");
            if is_full_stats {
                if map.contains_key("bind") {
                    return Err(typed_err(
                        "edge.stats cannot mix `bind` page shorthand with `binds`/`pages`/`admin_token_file`",
                    ));
                }
                for key in map.keys() {
                    if !matches!(key.as_str(), "binds" | "pages" | "admin_token_file") {
                        return Err(typed_err(format!("edge.stats: unknown key `{key}`")));
                    }
                }
                let mut full = map;
                if let Some(Value::Array(pages)) = full.get_mut("pages") {
                    for page in pages.iter_mut() {
                        let normalized = normalize_stats_page_object(page.take())?;
                        *page = normalized;
                    }
                }
                Ok(EdgeStats::Full(full))
            } else {
                Ok(EdgeStats::Page(normalize_stats_page_object(
                    Value::Object(map),
                )?))
            }
        }
        _ => Err(typed_err(
            "edge.stats must be a bind shorthand string or an object",
        )),
    }
}

fn normalize_stats_page_object(value: Value) -> Result<Value, ConfigSourceError> {
    let Value::Object(mut map) = value else {
        return Err(typed_err("edge.stats page must be an object"));
    };
    for key in map.keys() {
        if !matches!(
            key.as_str(),
            "bind"
                | "uri_prefix"
                | "refresh_ms"
                | "admin"
                | "max_connections"
                | "downstream_timeouts"
        ) {
            return Err(typed_err(format!("edge.stats: unknown key `{key}`")));
        }
    }

    let Some(bind_val) = map.remove("bind") else {
        return Err(typed_err("edge.stats object requires `bind`"));
    };
    let bind_str = parse_non_empty_str(bind_val, "edge.stats.bind")?;
    let (addr, inline_prefix) = if bind_str.contains('/') {
        let (addr, prefix) = split_stats_shorthand(&bind_str)?;
        (addr, Some(prefix))
    } else {
        bind_str.parse::<SocketAddr>().map_err(|_| {
            typed_err(format!(
                "edge.stats.bind: invalid socket address `{bind_str}`"
            ))
        })?;
        (bind_str, None)
    };
    map.insert("bind".to_owned(), Value::String(addr));

    if !map.contains_key("uri_prefix") {
        let prefix = inline_prefix.unwrap_or_else(|| DEFAULT_STATS_URI_PREFIX.to_owned());
        map.insert("uri_prefix".to_owned(), Value::String(prefix));
    }
    if !map.contains_key("refresh_ms") {
        map.insert(
            "refresh_ms".to_owned(),
            Value::Number(DEFAULT_STATS_REFRESH_MS.into()),
        );
    }
    if !map.contains_key("admin") {
        map.insert(
            "admin".to_owned(),
            Value::String(DEFAULT_STATS_ADMIN.to_owned()),
        );
    }

    Ok(Value::Object(map))
}

fn split_stats_shorthand(spec: &str) -> Result<(String, String), ConfigSourceError> {
    let (addr_part, uri_prefix) = match spec.split_once('/') {
        Some((addr, rest)) => (addr, format!("/{rest}")),
        None => (spec, DEFAULT_STATS_URI_PREFIX.to_owned()),
    };
    addr_part.parse::<SocketAddr>().map_err(|_| {
        typed_err(format!(
            "edge.stats: invalid socket address `{addr_part}` in `{spec}`"
        ))
    })?;
    Ok((addr_part.to_owned(), uri_prefix))
}

fn parse_edge_management(value: Value) -> Result<Value, ConfigSourceError> {
    match value {
        Value::String(bind) => {
            bind.parse::<SocketAddr>().map_err(|_| {
                typed_err(format!(
                    "edge.management: invalid socket address `{bind}` (expected a loopback `ip:port` such as `127.0.0.1:9090`)"
                ))
            })?;
            let mut obj = Map::new();
            obj.insert("bind".to_owned(), Value::String(bind));
            Ok(Value::Object(obj))
        }
        Value::Object(map) => {
            for key in map.keys() {
                if !matches!(key.as_str(), "bind" | "ui_dir" | "token_file") {
                    return Err(typed_err(format!("edge.management: unknown key `{key}`")));
                }
            }
            let Some(Value::String(bind)) = map.get("bind") else {
                return Err(typed_err(
                    "edge.management object requires a string `bind` socket address",
                ));
            };
            bind.parse::<SocketAddr>().map_err(|_| {
                typed_err(format!(
                    "edge.management.bind: invalid socket address `{bind}`"
                ))
            })?;
            Ok(Value::Object(map))
        }
        _ => Err(typed_err(
            "edge.management must be a socket address string or an object",
        )),
    }
}

pub(super) fn apply_edge_stats(
    root: &mut Map<String, Value>,
    edge_stats: Option<&EdgeStats>,
) -> Result<(), ConfigSourceError> {
    let Some(edge_stats) = edge_stats else {
        return Ok(());
    };

    let target = match root.get_mut("stats") {
        None | Some(Value::Null) => {
            root.insert("stats".to_owned(), Value::Object(Map::new()));
            root.get_mut("stats")
                .and_then(Value::as_object_mut)
                .expect("stats object just inserted")
        }
        Some(Value::Object(existing)) => existing,
        Some(_) => {
            return Err(typed_err("top-level `stats` must be an object"));
        }
    };

    match edge_stats {
        EdgeStats::Page(page) => {
            append_stats_array(target, "pages", vec![page.clone()])?;
        }
        EdgeStats::Full(full) => {
            if let Some(Value::Array(binds)) = full.get("binds") {
                append_stats_array(target, "binds", binds.clone())?;
            }
            if let Some(Value::Array(pages)) = full.get("pages") {
                append_stats_array(target, "pages", pages.clone())?;
            }
            if let Some(token_file) = full.get("admin_token_file") {
                if target.contains_key("admin_token_file") {
                    return Err(typed_err(
                        "both top-level `stats.admin_token_file` and `edge.stats.admin_token_file` are set",
                    ));
                }
                target.insert("admin_token_file".to_owned(), token_file.clone());
            }
        }
    }
    Ok(())
}

fn append_stats_array(
    stats: &mut Map<String, Value>,
    field: &str,
    items: Vec<Value>,
) -> Result<(), ConfigSourceError> {
    match stats.get_mut(field) {
        None | Some(Value::Null) => {
            stats.insert(field.to_owned(), Value::Array(items));
            Ok(())
        }
        Some(Value::Array(existing)) => {
            existing.extend(items);
            Ok(())
        }
        Some(Value::Object(empty)) if empty.is_empty() => {
            stats.insert(field.to_owned(), Value::Array(items));
            Ok(())
        }
        Some(_) => Err(typed_err(format!("stats.{field} must be an array"))),
    }
}

pub(super) fn apply_edge_management(
    root: &mut Map<String, Value>,
    edge_management: Option<&Value>,
) -> Result<(), ConfigSourceError> {
    let Some(management) = edge_management else {
        return Ok(());
    };
    if let Some(existing) = root.get("management")
        && !existing.is_null()
    {
        return Err(typed_err(
            "both top-level `management` and `edge.management` are set",
        ));
    }
    root.insert("management".to_owned(), management.clone());
    Ok(())
}

pub(super) fn build_tls_profile(
    profile_name: &str,
    certificates: &[String],
    edge_spec: &EdgeSpec,
) -> Result<Value, ConfigSourceError> {
    let default_cert = match &edge_spec.default_cert {
        Some(explicit) => explicit.clone(),
        None => certificates.first().cloned().ok_or_else(|| {
            typed_err(format!(
                "TLS profile `{profile_name}` requires at least one certificate"
            ))
        })?,
    };

    let mut profile = Map::new();
    profile.insert("name".to_owned(), Value::String(profile_name.to_owned()));
    profile.insert(
        "certificates".to_owned(),
        Value::Array(certificates.iter().cloned().map(Value::String).collect()),
    );
    profile.insert(
        "default_certificate".to_owned(),
        Value::String(default_cert),
    );
    if let Some(min_version) = &edge_spec.min_version {
        profile.insert("min_version".to_owned(), Value::String(min_version.clone()));
    }
    if let Some(alpn) = &edge_spec.alpn {
        profile.insert(
            "alpn".to_owned(),
            Value::Array(alpn.iter().cloned().map(Value::String).collect()),
        );
    }
    Ok(Value::Object(profile))
}

pub(super) fn build_listeners(
    base_name: &str,
    binds: &[ParsedBind],
    service_name: &str,
    tls_profile: Option<&str>,
    max_connections: Option<u64>,
    existing_ipv4_ports: &HashSet<u16>,
) -> Vec<Value> {
    let mut ipv4_ports = existing_ipv4_ports.clone();
    for bind in binds {
        if let ParsedBind::Socket(addr) = bind
            && addr.is_ipv4()
        {
            ipv4_ports.insert(addr.port());
        }
    }

    let multiple = binds.len() > 1;
    let mut listeners = Vec::with_capacity(binds.len());
    for (index, bind) in binds.iter().enumerate() {
        let name = if multiple {
            format!("{base_name}-{}", index + 1)
        } else {
            base_name.to_owned()
        };

        let bind_obj = match bind {
            ParsedBind::Socket(addr) => {
                let mut obj = Map::new();
                obj.insert("type".to_owned(), Value::String("socket".to_owned()));
                obj.insert("address".to_owned(), Value::String(addr.to_string()));
                if let SocketAddr::V6(v6) = addr
                    && v6.ip().is_unspecified()
                    && ipv4_ports.contains(&v6.port())
                {
                    obj.insert("ipv6_only".to_owned(), Value::Bool(true));
                }
                Value::Object(obj)
            }
            ParsedBind::Unix(path) => {
                let mut obj = Map::new();
                obj.insert("type".to_owned(), Value::String("unix".to_owned()));
                obj.insert("path".to_owned(), Value::String(path.clone()));
                Value::Object(obj)
            }
        };

        let mut listener = Map::new();
        listener.insert("name".to_owned(), Value::String(name));
        listener.insert("bind".to_owned(), bind_obj);
        listener.insert("protocol".to_owned(), Value::String("http".to_owned()));
        listener.insert("service".to_owned(), Value::String(service_name.to_owned()));
        if let Some(profile) = tls_profile {
            listener.insert("tls_profile".to_owned(), Value::String(profile.to_owned()));
        }
        if let Some(cap) = max_connections {
            listener.insert("max_connections".to_owned(), Value::Number(cap.into()));
        }
        listeners.push(Value::Object(listener));
    }

    listeners
}
