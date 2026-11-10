use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use oxiroute_config::Management;

use http::header::AUTHORIZATION;
use pingora::protocols::http::ServerSession;

use crate::secure_bearer::{
    HeaderCardinality, SecureBearerToken, SecureBearerTokenError, single_header,
};

pub(super) struct ManagementAuth {
    token: SecureBearerToken,
}

impl ManagementAuth {
    pub(super) fn new(token: &str) -> io::Result<Self> {
        Ok(Self {
            token: SecureBearerToken::new(token.as_bytes()).map_err(management_token_error)?,
        })
    }

    pub(super) fn from_token_file(path: &Path) -> io::Result<Self> {
        Ok(Self {
            token: SecureBearerToken::load(path).map_err(management_token_error)?,
        })
    }

    pub(super) fn authorized(&self, session: &ServerSession) -> bool {
        matches!(
            single_header(&session.req_header().headers, &AUTHORIZATION),
            HeaderCardinality::Single(value) if self.token.authorizes(value.as_bytes())
        )
    }
}

const TOKEN_FILE_ENV: &str = "OXIROUTE_MANAGEMENT_TOKEN_FILE";

/// The management token file: `management.token_file`, then the `OXIROUTE_MANAGEMENT_TOKEN_FILE`
/// environment variable.
#[must_use]
pub fn management_token_file(management: &Management) -> Option<PathBuf> {
    token_file_or(management, std::env::var_os(TOKEN_FILE_ENV))
}

fn token_file_or(management: &Management, environment: Option<OsString>) -> Option<PathBuf> {
    management
        .token_file
        .clone()
        .or_else(|| environment.map(PathBuf::from))
}

pub(crate) fn preflight_management_token(path: &Path) -> io::Result<()> {
    SecureBearerToken::load(path)
        .map(drop)
        .map_err(management_token_error)
}

fn management_token_error(error: SecureBearerTokenError) -> io::Error {
    let message = match error {
        SecureBearerTokenError::Open => "management token file could not be securely opened",
        SecureBearerTokenError::NotRegular => {
            "management token file must be a regular no-follow file"
        }
        SecureBearerTokenError::InsecureMode => "management token file mode must be 0400 or 0600",
        SecureBearerTokenError::TooLarge => "management token file exceeds the supported size",
        SecureBearerTokenError::Read => "management token file could not be read",
        SecureBearerTokenError::Unstable => "management token file changed while it was read",
        SecureBearerTokenError::InvalidToken => {
            "management token must be 32 to 512 visible ASCII bytes"
        }
    };
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn management(token_file: Option<&str>) -> Management {
        Management {
            bind: "127.0.0.1:9900".parse().unwrap(),
            ui_dir: None,
            token_file: token_file.map(PathBuf::from),
        }
    }

    #[test]
    fn configured_token_file_wins_over_the_environment() {
        let environment = Some(OsString::from("/env/token"));
        assert_eq!(
            token_file_or(&management(Some("/etc/token")), environment),
            Some(PathBuf::from("/etc/token"))
        );
    }

    #[test]
    fn environment_names_the_token_file_when_the_configuration_does_not() {
        assert_eq!(
            token_file_or(&management(None), Some(OsString::from("/env/token"))),
            Some(PathBuf::from("/env/token"))
        );
        assert_eq!(token_file_or(&management(None), None), None);
    }
}
