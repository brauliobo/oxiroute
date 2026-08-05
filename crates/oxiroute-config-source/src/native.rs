use std::{net::IpAddr, path::PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::ConfigSourceError;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NginxSource {
    pub path: PathBuf,
    #[serde(default = "default_root_prefix")]
    pub root_prefix: PathBuf,
    #[serde(default)]
    pub host_timezone: Option<String>,
    #[serde(default)]
    pub default_access_log_file: Option<PathBuf>,
    #[serde(default)]
    pub recording_root: Option<PathBuf>,
    #[serde(default)]
    pub default_error_server: Option<String>,
    #[serde(default)]
    pub x_accel_controls_absent: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HaproxySource {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub node_ip: Option<IpAddr>,
    #[serde(default)]
    pub gpu1_defined: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidSource {
    pub path: PathBuf,
    #[serde(default)]
    pub externalize_cache: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApacheSource {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VarnishSource {
    pub path: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum NativeDirective {
    Nginx(NginxSource),
    Haproxy(HaproxySource),
    Squid(SquidSource),
    Apache(ApacheSource),
    Varnish(VarnishSource),
}

fn default_root_prefix() -> PathBuf {
    PathBuf::from("/")
}

pub(crate) fn extract_directives(
    value: &mut Value,
    format: &'static str,
) -> Result<Vec<NativeDirective>, ConfigSourceError> {
    let Value::Object(root) = value else {
        return Ok(Vec::new());
    };
    let mut directives = Vec::new();
    if let Some(value) = root.remove("nginx_server") {
        for value in one_or_many(value, "nginx_server", format)? {
            directives.push(NativeDirective::Nginx(decode_nginx(value, format)?));
        }
    }
    if let Some(value) = root.remove("haproxy_server") {
        for value in one_or_many(value, "haproxy_server", format)? {
            directives.push(NativeDirective::Haproxy(decode_haproxy(value, format)?));
        }
    }
    if let Some(value) = root.remove("squid_server") {
        for value in one_or_many(value, "squid_server", format)? {
            directives.push(NativeDirective::Squid(decode_squid(value, format)?));
        }
    }
    if let Some(value) = root.remove("apache_server") {
        for value in one_or_many(value, "apache_server", format)? {
            directives.push(NativeDirective::Apache(decode_apache(value, format)?));
        }
    }
    if let Some(value) = root.remove("varnish_server") {
        for value in one_or_many(value, "varnish_server", format)? {
            directives.push(NativeDirective::Varnish(decode_varnish(value, format)?));
        }
    }
    Ok(directives)
}

pub(crate) fn decode_squid(
    value: Value,
    format: &'static str,
) -> Result<SquidSource, ConfigSourceError> {
    let source: SquidSource = serde_json::from_value(value).map_err(|error| {
        ConfigSourceError::parse(format, format!("invalid squid_server: {error}"))
    })?;
    if source.path.as_os_str().is_empty() {
        return Err(ConfigSourceError::parse(
            format,
            "squid_server path must not be empty",
        ));
    }
    Ok(source)
}

pub(crate) fn decode_apache(
    value: Value,
    format: &'static str,
) -> Result<ApacheSource, ConfigSourceError> {
    let source: ApacheSource = serde_json::from_value(value).map_err(|error| {
        ConfigSourceError::parse(format, format!("invalid apache_server: {error}"))
    })?;
    if source.path.as_os_str().is_empty() {
        return Err(ConfigSourceError::parse(
            format,
            "apache_server path must not be empty",
        ));
    }
    Ok(source)
}

pub(crate) fn decode_varnish(
    value: Value,
    format: &'static str,
) -> Result<VarnishSource, ConfigSourceError> {
    let source: VarnishSource = serde_json::from_value(value).map_err(|error| {
        ConfigSourceError::parse(format, format!("invalid varnish_server: {error}"))
    })?;
    if source.path.as_os_str().is_empty() {
        return Err(ConfigSourceError::parse(
            format,
            "varnish_server path must not be empty",
        ));
    }
    if source.arguments.iter().any(String::is_empty) {
        return Err(ConfigSourceError::parse(
            format,
            "varnish_server arguments must not contain empty values",
        ));
    }
    Ok(source)
}

pub(crate) fn decode_nginx(
    value: Value,
    format: &'static str,
) -> Result<NginxSource, ConfigSourceError> {
    let source: NginxSource = serde_json::from_value(value).map_err(|error| {
        ConfigSourceError::parse(format, format!("invalid nginx_server: {error}"))
    })?;
    if source.path.as_os_str().is_empty() {
        return Err(ConfigSourceError::parse(
            format,
            "nginx_server path must not be empty",
        ));
    }
    if source.root_prefix.as_os_str().is_empty() {
        return Err(ConfigSourceError::parse(
            format,
            "nginx_server root_prefix must not be empty",
        ));
    }
    Ok(source)
}

pub(crate) fn decode_haproxy(
    value: Value,
    format: &'static str,
) -> Result<HaproxySource, ConfigSourceError> {
    let source: HaproxySource = serde_json::from_value(value).map_err(|error| {
        ConfigSourceError::parse(format, format!("invalid haproxy_server: {error}"))
    })?;
    if source.paths.is_empty() || source.paths.iter().any(|path| path.as_os_str().is_empty()) {
        return Err(ConfigSourceError::parse(
            format,
            "haproxy_server paths must contain nonempty paths",
        ));
    }
    if source.gpu1_defined && source.node_ip.is_none() {
        return Err(ConfigSourceError::parse(
            format,
            "haproxy_server node_ip is required when gpu1_defined is true",
        ));
    }
    Ok(source)
}

fn one_or_many(
    value: Value,
    name: &str,
    format: &'static str,
) -> Result<Vec<Value>, ConfigSourceError> {
    match value {
        Value::Object(_) => Ok(vec![value]),
        Value::Array(values) if values.iter().all(Value::is_object) => Ok(values),
        Value::Array(_) => Err(ConfigSourceError::parse(
            format,
            format!("{name} arrays may contain only objects"),
        )),
        _ => Err(ConfigSourceError::parse(
            format,
            format!("{name} must be an object or array of objects"),
        )),
    }
}
