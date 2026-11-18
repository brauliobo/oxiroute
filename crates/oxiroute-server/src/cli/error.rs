use std::{error::Error, fmt};

pub(super) const EXIT_FINDING: u8 = 1;
pub(super) const EXIT_LOCAL: u8 = 3;
pub(super) const EXIT_CONNECT: u8 = 4;
pub(super) const EXIT_AUTH: u8 = 5;
pub(super) const EXIT_NOT_FOUND: u8 = 6;
pub(super) const EXIT_CONFLICT: u8 = 7;
pub(super) const EXIT_REMOTE: u8 = 8;
pub(super) const EXIT_UNSUPPORTED: u8 = 9;

#[derive(Debug)]
pub(crate) enum CliError {
    Startup(String),
    Local(String),
    Connect(String),
    Timeout(String),
    Io(std::io::Error),
    Status(u16, String),
    Remote(String),
    Unsupported(&'static str),
    Differs,
    Blocked(String),
    OutputClosed,
}

impl CliError {
    pub(super) fn startup(error: impl fmt::Display) -> Self {
        Self::Startup(error.to_string())
    }

    pub(super) fn local(error: impl fmt::Display) -> Self {
        Self::Local(error.to_string())
    }

    pub(super) const fn exit_code(&self) -> u8 {
        match self {
            Self::OutputClosed => 0,
            Self::Startup(_) | Self::Differs | Self::Blocked(_) => EXIT_FINDING,
            Self::Local(_) => EXIT_LOCAL,
            Self::Connect(_) | Self::Timeout(_) | Self::Io(_) => EXIT_CONNECT,
            Self::Status(401 | 403, _) => EXIT_AUTH,
            Self::Status(404, _) => EXIT_NOT_FOUND,
            Self::Status(409 | 412, _) => EXIT_CONFLICT,
            Self::Status(_, _) | Self::Remote(_) => EXIT_REMOTE,
            Self::Unsupported(_) => EXIT_UNSUPPORTED,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(message)
            | Self::Local(message)
            | Self::Connect(message)
            | Self::Timeout(message)
            | Self::Blocked(message)
            | Self::Remote(message) => formatter.write_str(message),
            Self::Status(status, code) => {
                write!(
                    formatter,
                    "management endpoint returned HTTP {status} ({code})"
                )?;
                match status {
                    401 => formatter.write_str(
                        "; the token was not accepted: check that --token-file or \
                         OXIROUTE_MANAGEMENT_TOKEN_FILE names the token this daemon runs with",
                    ),
                    403 => formatter.write_str(
                        "; the token is valid but does not allow this operation: check its scope",
                    ),
                    _ => Ok(()),
                }
            }
            Self::Io(error) => error.fmt(formatter),
            Self::Unsupported(message) => write!(formatter, "unsupported: {message}"),
            Self::Differs => formatter.write_str("differences found"),
            Self::OutputClosed => formatter.write_str("output closed"),
        }
    }
}

impl Error for CliError {}
