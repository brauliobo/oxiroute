use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::{ConfigSourceError, limits};

mod actions;
mod cert;
mod edge;
mod headers;
mod sites;

use actions::{append_array, has_named_entry, typed_err};
use edge::{EdgeSpec, apply_edge_management, apply_edge_stats, build_listeners, build_tls_profile};
use sites::{GzipChoice, ParsedSites};

const DEFAULT_TLS_PROFILE_NAME: &str = "edge-tls";
const HTTP_SERVICE_NAME: &str = "edge-http";
const HTTP_REDIRECT_SERVICE_NAME: &str = "edge-http-redirect";
const HTTPS_SERVICE_NAME: &str = "edge-https";

/// Expands high-level `edge` and `sites` declarations into canonical `ConfigDraft` fields.
///
/// Returns the updated value tree and whether any high-level sugar was expanded.
pub(crate) fn desugar(mut value: Value) -> Result<(Value, bool), ConfigSourceError> {
    let has_sugar = value
        .as_object()
        .is_some_and(|root| root.contains_key("edge") || root.contains_key("sites"));
    if !has_sugar {
        return Ok((value, false));
    }

    let Value::Object(ref mut root) = value else {
        return Ok((value, false));
    };

    let raw_edge = root.remove("edge");
    let raw_sites = root.remove("sites");

    root.entry("version".to_owned()).or_insert_with(|| json!(1));

    let edge_spec = match raw_edge {
        Some(Value::Null) | None => EdgeSpec::default(),
        Some(edge_val) => edge::parse(edge_val)?,
    };
    let parsed_sites = match raw_sites {
        Some(Value::Null) | None => ParsedSites::default(),
        Some(sites_val) => sites::parse(sites_val)?,
    };

    apply_edge_stats(root, edge_spec.stats.as_ref())?;
    apply_edge_management(root, edge_spec.management.as_ref())?;

    if !parsed_sites.certificates.is_empty() {
        append_array(root, "certificates", parsed_sites.certificates)?;
    }
    if !parsed_sites.upstream_pools.is_empty() {
        append_array(root, "upstream_pools", parsed_sites.upstream_pools)?;
    }

    let tls_profile_name = edge_spec
        .tls_profile
        .as_deref()
        .unwrap_or(DEFAULT_TLS_PROFILE_NAME);
    let has_tls_profile = resolve_tls_profile(
        root,
        tls_profile_name,
        parsed_sites.tls_certificates,
        &edge_spec,
    )?;

    let emitted = emit_http_services(
        root,
        &edge_spec,
        has_tls_profile,
        ServiceInputs {
            https_routes: parsed_sites.https_routes,
            http_only_routes: parsed_sites.http_only_routes,
            https_gzip: parsed_sites.https_gzip,
            http_only_gzip: parsed_sites.http_only_gzip,
        },
    )?;

    emit_edge_listeners(root, &edge_spec, tls_profile_name, emitted)?;

    limits::validate_value(&value)?;
    Ok((value, true))
}

struct ServiceInputs {
    https_routes: Vec<Value>,
    http_only_routes: Vec<Value>,
    https_gzip: Option<GzipChoice>,
    http_only_gzip: Option<GzipChoice>,
}

#[derive(Clone, Copy)]
struct EmittedServices {
    http_listener_service: Option<&'static str>,
    has_https_service: bool,
    has_tls_profile: bool,
}

fn resolve_tls_profile(
    root: &mut Map<String, Value>,
    tls_profile_name: &str,
    mut tls_certs: Vec<String>,
    edge_spec: &EdgeSpec,
) -> Result<bool, ConfigSourceError> {
    if has_named_entry(root, "tls_profiles", tls_profile_name)? {
        return Ok(true);
    }
    if let Some(default_cert) = &edge_spec.default_cert
        && !tls_certs.iter().any(|existing| existing == default_cert)
    {
        tls_certs.push(default_cert.clone());
    }
    if tls_certs.is_empty() {
        return Ok(false);
    }
    let profile = build_tls_profile(tls_profile_name, &tls_certs, edge_spec)?;
    append_array(root, "tls_profiles", vec![profile])?;
    Ok(true)
}

fn emit_http_services(
    root: &mut Map<String, Value>,
    edge_spec: &EdgeSpec,
    has_tls_profile: bool,
    inputs: ServiceInputs,
) -> Result<EmittedServices, ConfigSourceError> {
    let tls_binds_present = !edge_spec.https.is_empty();
    let plain_binds_present = !edge_spec.http.is_empty();
    let explicit_https = has_named_entry(root, "http_services", HTTPS_SERVICE_NAME)?;
    let explicit_http = has_named_entry(root, "http_services", HTTP_SERVICE_NAME)?;
    let explicit_redirect = has_named_entry(root, "http_services", HTTP_REDIRECT_SERVICE_NAME)?;

    let mut generated_services = Vec::new();
    let mut has_https_service = explicit_https;

    let emit_https = tls_binds_present
        || (plain_binds_present && edge_spec.redirect_https && !inputs.https_routes.is_empty())
        || (!plain_binds_present && has_tls_profile && !inputs.https_routes.is_empty());

    if emit_https && !inputs.https_routes.is_empty() {
        if explicit_https {
            return Err(typed_err(format!(
                "cannot generate `{HTTPS_SERVICE_NAME}` service because an explicit `{HTTPS_SERVICE_NAME}` already exists"
            )));
        }
        generated_services.push(build_http_service(
            HTTPS_SERVICE_NAME,
            inputs.https_routes.clone(),
            inputs.https_gzip.as_ref(),
        ));
        has_https_service = true;
    }

    let http_listener_service = build_http_side_service(
        edge_spec,
        emit_https,
        explicit_http,
        explicit_redirect,
        inputs,
        &mut generated_services,
    )?;

    if !generated_services.is_empty() {
        append_array(root, "http_services", generated_services)?;
    }

    Ok(EmittedServices {
        http_listener_service,
        has_https_service,
        has_tls_profile,
    })
}

fn build_http_side_service(
    edge_spec: &EdgeSpec,
    emit_https: bool,
    explicit_http: bool,
    explicit_redirect: bool,
    inputs: ServiceInputs,
    generated_services: &mut Vec<Value>,
) -> Result<Option<&'static str>, ConfigSourceError> {
    let has_http_binds = !edge_spec.http.is_empty();
    if has_http_binds && edge_spec.redirect_https {
        if !explicit_redirect {
            let mut redirect_routes = inputs.http_only_routes;
            redirect_routes.push(https_redirect_route());
            generated_services.push(build_http_service(
                HTTP_REDIRECT_SERVICE_NAME,
                redirect_routes,
                inputs.http_only_gzip.as_ref(),
            ));
        }
        return Ok(Some(HTTP_REDIRECT_SERVICE_NAME));
    }

    if has_http_binds {
        let mut http_routes = inputs.http_only_routes;
        http_routes.extend(inputs.https_routes);
        let http_gzip =
            merge_gzip_choice(inputs.http_only_gzip.as_ref(), inputs.https_gzip.as_ref());
        let mut emitted = explicit_http;
        if !http_routes.is_empty() {
            if explicit_http {
                return Err(typed_err(format!(
                    "cannot generate `{HTTP_SERVICE_NAME}` service because an explicit `{HTTP_SERVICE_NAME}` already exists"
                )));
            }
            generated_services.push(build_http_service(
                HTTP_SERVICE_NAME,
                http_routes,
                http_gzip.as_ref(),
            ));
            emitted = true;
        }
        if !emitted {
            return Err(typed_err(
                "edge.http (without redirect_https) requires at least one site or an explicit `edge-http` HTTP service",
            ));
        }
        return Ok(Some(HTTP_SERVICE_NAME));
    }

    let mut standalone_routes = inputs.http_only_routes;
    let mut standalone_gzip = inputs.http_only_gzip;
    if !emit_https && !inputs.https_routes.is_empty() {
        standalone_routes.extend(inputs.https_routes);
        standalone_gzip = merge_gzip_choice(standalone_gzip.as_ref(), inputs.https_gzip.as_ref());
    }
    if !standalone_routes.is_empty() {
        if explicit_http {
            return Err(typed_err(format!(
                "cannot generate `{HTTP_SERVICE_NAME}` service because an explicit `{HTTP_SERVICE_NAME}` already exists"
            )));
        }
        generated_services.push(build_http_service(
            HTTP_SERVICE_NAME,
            standalone_routes,
            standalone_gzip.as_ref(),
        ));
    }
    Ok(None)
}

fn emit_edge_listeners(
    root: &mut Map<String, Value>,
    edge_spec: &EdgeSpec,
    tls_profile_name: &str,
    emitted: EmittedServices,
) -> Result<(), ConfigSourceError> {
    let has_https_binds = !edge_spec.https.is_empty();
    if has_https_binds {
        if !emitted.has_https_service {
            return Err(typed_err(
                "edge.https requires at least one HTTPS site or an explicit `edge-https` HTTP service",
            ));
        }
        if !emitted.has_tls_profile {
            return Err(typed_err(
                "edge.https requires at least one certificate (via site `cert`) or an explicit `tls_profile`",
            ));
        }
    }

    let existing_ipv4_ports = collect_existing_ipv4_ports(root);
    let mut generated_listeners = Vec::new();
    if let Some(service_name) = emitted.http_listener_service {
        generated_listeners.extend(build_listeners(
            "edge-http",
            &edge_spec.http,
            service_name,
            None,
            edge_spec.max_connections,
            &existing_ipv4_ports,
        ));
    }
    if has_https_binds {
        generated_listeners.extend(build_listeners(
            "edge-https",
            &edge_spec.https,
            HTTPS_SERVICE_NAME,
            Some(tls_profile_name),
            edge_spec.max_connections,
            &existing_ipv4_ports,
        ));
    }
    if !generated_listeners.is_empty() {
        append_array(root, "listeners", generated_listeners)?;
    }
    Ok(())
}

fn build_http_service(name: &str, routes: Vec<Value>, gzip: Option<&GzipChoice>) -> Value {
    let mut service = Map::new();
    service.insert("name".to_owned(), Value::String(name.to_owned()));
    service.insert("routes".to_owned(), Value::Array(routes));
    if let Some(gzip) = gzip {
        service.insert("gzip".to_owned(), gzip.to_value());
    }
    Value::Object(service)
}

fn https_redirect_route() -> Value {
    json!({
        "path": {
            "kind": "segment_prefix",
            "value": "/"
        },
        "action": {
            "type": "redirect",
            "status": 308,
            "location": {
                "kind": "request_template",
                "value": "https://$host$request_uri"
            }
        }
    })
}

fn merge_gzip_choice(
    first: Option<&GzipChoice>,
    second: Option<&GzipChoice>,
) -> Option<GzipChoice> {
    match (first, second) {
        (None, None) => None,
        (Some(choice), None) | (None, Some(choice)) => Some(choice.clone()),
        (Some(GzipChoice::Default), Some(other)) => Some(other.clone()),
        (Some(first_custom @ GzipChoice::Custom(_)), Some(GzipChoice::Default)) => {
            Some(first_custom.clone())
        }
        (Some(GzipChoice::Custom(_)), Some(second_custom @ GzipChoice::Custom(_))) => {
            Some(second_custom.clone())
        }
    }
}

fn collect_existing_ipv4_ports(root: &Map<String, Value>) -> HashSet<u16> {
    let mut ports = HashSet::new();
    let Some(Value::Array(listeners)) = root.get("listeners") else {
        return ports;
    };
    for listener in listeners {
        let Some(bind) = listener.get("bind").and_then(Value::as_object) else {
            continue;
        };
        if bind.get("type").and_then(Value::as_str) != Some("socket") {
            continue;
        }
        if let Some(addr_str) = bind.get("address").and_then(Value::as_str)
            && let Ok(addr) = addr_str.parse::<std::net::SocketAddr>()
            && addr.is_ipv4()
        {
            ports.insert(addr.port());
        }
    }
    ports
}
