use std::{io, path::Path};

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
