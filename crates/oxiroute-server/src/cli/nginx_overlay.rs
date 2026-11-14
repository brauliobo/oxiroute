use std::path::PathBuf;

use oxiroute_config::{UpstreamTls, UpstreamTlsVerify};
use oxiroute_import::nginx::{NginxBearerTokenOverlay, NginxUpstreamTlsOverlay};

fn nonempty<'a>(value: &'a str, what: &str) -> Result<&'a str, String> {
    if value.is_empty() {
        Err(format!("{what} must not be empty"))
    } else {
        Ok(value)
    }
}

fn absolute_path(value: &str, what: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(nonempty(value, what)?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{what} must be an absolute path: `{value}`"))
    }
}

/// Parses `<SERVER_NAME>=<ABS_PATH>`.
pub(super) fn parse_bearer_token_file(value: &str) -> Result<NginxBearerTokenOverlay, String> {
    let (server_name, path) = value
        .split_once('=')
        .ok_or("expected <SERVER_NAME>=<ABS_PATH>")?;
    Ok(NginxBearerTokenOverlay {
        server_name: nonempty(server_name, "server name")?.to_owned(),
        token_file_path: absolute_path(path, "token file")?,
    })
}

/// Parses `<AUTHORITY>=<SNI>[,ca=<PATH>][,activate]`.
pub(super) fn parse_upstream_tls(value: &str) -> Result<NginxUpstreamTlsOverlay, String> {
    let mut parts = value.split(',');
    let (authority, server_name) = parts
        .next()
        .and_then(|head| head.split_once('='))
        .ok_or("expected <AUTHORITY>=<SNI>[,ca=<PATH>][,activate]")?;
    let mut ca_certificate_path = None;
    let mut activate = false;
    for option in parts {
        match option.split_once('=') {
            Some(("ca", path)) if ca_certificate_path.is_none() => {
                ca_certificate_path = Some(absolute_path(path, "CA certificate")?);
            }
            None if option == "activate" && !activate => activate = true,
            _ => {
                return Err(format!(
                    "unknown or repeated option `{option}`; expected ca=<PATH> or activate"
                ));
            }
        }
    }
    Ok(NginxUpstreamTlsOverlay {
        authority: nonempty(authority, "authority")?.to_owned(),
        tls: UpstreamTls {
            server_name: Some(nonempty(server_name, "SNI")?.to_owned()),
            verify: UpstreamTlsVerify::Full,
            ca_certificate_path,
        },
        require_connectivity_activation: activate,
    })
}
