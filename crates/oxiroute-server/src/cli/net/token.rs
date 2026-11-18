use std::{
    io,
    path::{Path, PathBuf},
};

use zeroize::Zeroizing;

use crate::cli::{error::CliError, fs::read_regular_file};

pub(in crate::cli) const DEFAULT_PACKAGE_ENV_FILE: &str = "/etc/oxiroute/oxiroute.env";
pub(in crate::cli) const DEFAULT_MANAGEMENT_TOKEN_FILE: &str = "/etc/oxiroute/management.token";
pub(in crate::cli) const MANAGEMENT_TOKEN_ENV: &str = "OXIROUTE_MANAGEMENT_TOKEN_FILE";
const MAX_PACKAGE_ENV_BYTES: usize = 8 * 1024;
const MAX_TOKEN_BYTES: usize = 512;

pub(in crate::cli) fn resolve_token_path(
    explicit_path: Option<&Path>,
    configured_path: Option<&Path>,
    package_env_file: &Path,
    default_path: &Path,
) -> Result<Option<PathBuf>, CliError> {
    if let Some(path) = explicit_path.or(configured_path) {
        return Ok(Some(path.to_owned()));
    }
    if let Some(path) = package_env_token_path(package_env_file)? {
        return Ok(Some(path));
    }
    match std::fs::symlink_metadata(default_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Ok(_) | Err(_) => Ok(Some(default_path.to_owned())),
    }
}

fn package_env_token_path(path: &Path) -> Result<Option<PathBuf>, CliError> {
    let bytes = match read_regular_file(path, MAX_PACKAGE_ENV_BYTES, false) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(CliError::Local(
                "management token path configuration could not be read securely".into(),
            ));
        }
    };
    let contents = std::str::from_utf8(&bytes)
        .map_err(|_| CliError::Local("management token path configuration is invalid".into()))?;
    let mut token_path = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name != MANAGEMENT_TOKEN_ENV {
            continue;
        }
        if token_path.is_some() {
            return Err(CliError::Local(
                "management token path configuration is invalid".into(),
            ));
        }
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(CliError::Local(
                "management token path configuration is invalid".into(),
            ));
        }
        token_path = Some(PathBuf::from(value));
    }
    Ok(token_path)
}

pub(in crate::cli) fn read_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let bytes = Zeroizing::new(
        read_regular_file(path, MAX_TOKEN_BYTES + 2, true).map_err(|_| {
            CliError::Local("management token file could not be read securely".into())
        })?,
    );
    let mut token = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CliError::Local("management token file is invalid".into()))?,
    );
    if token.ends_with("\r\n") {
        let length = token.len();
        token.truncate(length - 2);
    } else if token.ends_with('\n') {
        token.pop();
    }
    if token.len() < 32
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(CliError::Local("management token file is invalid".into()));
    }
    Ok(token)
}
