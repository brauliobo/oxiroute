use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::ConfigSourceError;

use super::actions::{parse_string_or_list, typed_err};
use super::sites::HostEntry;

pub(super) enum SiteCert {
    None,
    Reference(String),
    Generate(Value, String),
}

pub(super) fn parse_site_cert(
    site_name: &str,
    cert: Option<Value>,
    hosts: &[HostEntry],
    http_only: bool,
) -> Result<SiteCert, ConfigSourceError> {
    let Some(cert_val) = cert else {
        return Ok(SiteCert::None);
    };
    if cert_val.is_null() || cert_val.as_str() == Some("none") {
        return Ok(SiteCert::None);
    }
    if http_only {
        return Err(typed_err(format!(
            "site `{site_name}`: `http_only: true` cannot be combined with `cert` (omit `cert` or set `cert: \"none\"`)"
        )));
    }

    match cert_val {
        Value::String(kind_or_ref) => parse_cert_string(site_name, &kind_or_ref, hosts),
        Value::Object(obj) => parse_cert_object(site_name, obj, hosts),
        _ => Err(typed_err(format!(
            "site `{site_name}`: `cert` must be a string, object, or null"
        ))),
    }
}

fn parse_cert_string(
    site_name: &str,
    kind_or_ref: &str,
    hosts: &[HostEntry],
) -> Result<SiteCert, ConfigSourceError> {
    match kind_or_ref {
        "" => Err(typed_err(format!(
            "site `{site_name}`: `cert` string must not be empty"
        ))),
        "certbot" => {
            let dns_names = derive_cert_dns_names(site_name, hosts)?;
            let source = build_certbot_source(site_name, &dns_names, None, None, None)?;
            Ok(SiteCert::Generate(
                build_certificate(site_name, dns_names, source),
                site_name.to_owned(),
            ))
        }
        "self_signed" | "self_signed_development" => {
            let dns_names = derive_cert_dns_names(site_name, hosts)?;
            let source = json!({ "type": "self_signed_development" });
            Ok(SiteCert::Generate(
                build_certificate(site_name, dns_names, source),
                site_name.to_owned(),
            ))
        }
        cert_ref => Ok(SiteCert::Reference(cert_ref.to_owned())),
    }
}

fn parse_cert_object(
    site_name: &str,
    mut obj: Map<String, Value>,
    hosts: &[HostEntry],
) -> Result<SiteCert, ConfigSourceError> {
    let explicit_dns = match obj.remove("dns_names") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(parse_string_or_list(
            raw,
            &format!("site `{site_name}`.cert.dns_names"),
        )?),
    };
    let dns_names = match explicit_dns {
        Some(names) if !names.is_empty() => names,
        Some(_) => {
            return Err(typed_err(format!(
                "site `{site_name}`: `cert.dns_names` must not be empty"
            )));
        }
        None => derive_cert_dns_names(site_name, hosts)?,
    };

    let cert_type = match obj.remove("type") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(_) => {
            return Err(typed_err(format!(
                "site `{site_name}`: `cert.type` must be a string"
            )));
        }
    };

    let source = match cert_type.as_deref() {
        None | Some("files") => parse_files_cert_source(site_name, obj)?,
        Some("certbot") => parse_certbot_cert_source(site_name, &dns_names, obj)?,
        Some("self_signed" | "self_signed_development") => {
            parse_self_signed_cert_source(site_name, obj)?
        }
        Some("acme_managed") => {
            obj.insert("type".to_owned(), Value::String("acme_managed".to_owned()));
            Value::Object(obj)
        }
        Some(other) => {
            return Err(typed_err(format!(
                "site `{site_name}`: unknown cert type `{other}`"
            )));
        }
    };

    Ok(SiteCert::Generate(
        build_certificate(site_name, dns_names, source),
        site_name.to_owned(),
    ))
}

fn derive_cert_dns_names(
    site_name: &str,
    hosts: &[HostEntry],
) -> Result<Vec<String>, ConfigSourceError> {
    let mut dns_names = Vec::new();
    let mut seen = HashSet::new();
    for entry in hosts {
        let Some(raw) = &entry.raw else {
            continue;
        };
        if let Some(suffix) = raw.strip_prefix('.') {
            for expanded in [suffix.to_owned(), format!("*.{suffix}")] {
                let lower = expanded.to_ascii_lowercase();
                if seen.insert(lower) {
                    dns_names.push(expanded);
                }
            }
        } else {
            let lower = raw.to_ascii_lowercase();
            if seen.insert(lower) {
                dns_names.push(raw.clone());
            }
        }
    }
    if dns_names.is_empty() {
        return Err(typed_err(format!(
            "site `{site_name}`: generating a certificate requires `host`/`hosts` (or explicit `cert.dns_names`)"
        )));
    }
    Ok(dns_names)
}

fn build_certificate(name: &str, dns_names: Vec<String>, source: Value) -> Value {
    let mut cert = Map::new();
    cert.insert("name".to_owned(), Value::String(name.to_owned()));
    cert.insert(
        "dns_names".to_owned(),
        Value::Array(dns_names.into_iter().map(Value::String).collect()),
    );
    cert.insert("source".to_owned(), source);
    Value::Object(cert)
}

fn parse_files_cert_source(
    site_name: &str,
    mut obj: Map<String, Value>,
) -> Result<Value, ConfigSourceError> {
    let cert_short = obj.remove("cert");
    let cert_long = obj.remove("certificate_chain_path");
    let key_short = obj.remove("key");
    let key_long = obj.remove("private_key_path");

    if let Some((unknown, _)) = obj.into_iter().next() {
        return Err(typed_err(format!(
            "site `{site_name}`: unknown key `{unknown}` in `cert` files object"
        )));
    }
    if cert_short.is_some() && cert_long.is_some() {
        return Err(typed_err(format!(
            "site `{site_name}`: specify either `cert` or `certificate_chain_path`, not both"
        )));
    }
    if key_short.is_some() && key_long.is_some() {
        return Err(typed_err(format!(
            "site `{site_name}`: specify either `key` or `private_key_path`, not both"
        )));
    }

    let Some(Value::String(chain)) = cert_short.or(cert_long) else {
        return Err(typed_err(format!(
            "site `{site_name}`: files `cert` requires string `cert`/`certificate_chain_path`"
        )));
    };
    let Some(Value::String(key)) = key_short.or(key_long) else {
        return Err(typed_err(format!(
            "site `{site_name}`: files `cert` requires string `key`/`private_key_path`"
        )));
    };

    Ok(json!({
        "type": "files",
        "certificate_chain_path": chain,
        "private_key_path": key,
    }))
}

fn parse_certbot_cert_source(
    site_name: &str,
    dns_names: &[String],
    mut obj: Map<String, Value>,
) -> Result<Value, ConfigSourceError> {
    let root = extract_opt_string(&mut obj, "root", site_name, "cert")?;
    let live = extract_opt_string(&mut obj, "live_directory_path", site_name, "cert")?;
    let archive = extract_opt_string(&mut obj, "archive_directory_path", site_name, "cert")?;
    if let Some((unknown, _)) = obj.into_iter().next() {
        return Err(typed_err(format!(
            "site `{site_name}`: unknown key `{unknown}` in certbot `cert`"
        )));
    }
    build_certbot_source(site_name, dns_names, root, live, archive)
}

fn build_certbot_source(
    site_name: &str,
    dns_names: &[String],
    root: Option<String>,
    explicit_live: Option<String>,
    explicit_archive: Option<String>,
) -> Result<Value, ConfigSourceError> {
    let root = root.map_or_else(
        || "/etc/letsencrypt".to_owned(),
        |custom| custom.trim_end_matches('/').to_owned(),
    );
    let primary = dns_names
        .iter()
        .find(|domain| !domain.starts_with("*."))
        .map(String::as_str)
        .or_else(|| {
            dns_names
                .first()
                .map(|domain| domain.strip_prefix("*.").unwrap_or(domain.as_str()))
        })
        .ok_or_else(|| {
            typed_err(format!(
                "site `{site_name}`: certbot requires at least one domain"
            ))
        })?;

    let live_dir = explicit_live.unwrap_or_else(|| format!("{root}/live/{primary}"));
    let archive_dir = explicit_archive.unwrap_or_else(|| format!("{root}/archive/{primary}"));
    Ok(json!({
        "type": "certbot",
        "live_directory_path": live_dir,
        "archive_directory_path": archive_dir,
    }))
}

fn parse_self_signed_cert_source(
    site_name: &str,
    mut obj: Map<String, Value>,
) -> Result<Value, ConfigSourceError> {
    for key in obj.keys() {
        if !matches!(key.as_str(), "validity_days" | "key_type") {
            return Err(typed_err(format!(
                "site `{site_name}`: unknown key `{key}` in self_signed `cert`"
            )));
        }
    }
    obj.insert(
        "type".to_owned(),
        Value::String("self_signed_development".to_owned()),
    );
    Ok(Value::Object(obj))
}

fn extract_opt_string(
    map: &mut Map<String, Value>,
    key: &str,
    site_name: &str,
    section: &str,
) -> Result<Option<String>, ConfigSourceError> {
    match map.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(typed_err(format!(
            "site `{site_name}`: `{section}.{key}` must be a string"
        ))),
    }
}
