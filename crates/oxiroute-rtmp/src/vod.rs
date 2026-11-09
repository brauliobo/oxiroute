use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Component, Path},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use http::Uri;
use openssl::ssl::SslStream;
use oxiroute_tls_client::system_trust_connector;
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, FileType, Mode, OFlags},
};

use crate::{MediaEvent, RtmpOutboundPolicy};

mod application;
mod origin;
mod range;

pub use application::*;
#[allow(clippy::wildcard_imports)]
use origin::*;
pub use range::*;

pub const MAX_VOD_PATH_BYTES: usize = 4_096;
pub const MAX_VOD_SOURCE_NAME_BYTES: usize = 128;
pub const MAX_VOD_ORIGIN_BYTES: usize = 2_048;
pub const MAX_VOD_REDIRECTS: usize = 3;
pub const MAX_VOD_HTTP_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_VOD_EVENTS: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VodLimits {
    pub max_sessions: usize,
    pub max_file_bytes: u64,
    pub max_duration: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VodSourceDefinition {
    Local {
        name: String,
        root_directory: std::path::PathBuf,
    },
    Http {
        name: String,
        origin: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VodValueError {
    #[error("VOD field `{0}` is invalid")]
    InvalidField(&'static str),
}

impl VodLimits {
    /// # Errors
    ///
    /// Returns an error when a VOD session, file, or duration bound is zero.
    pub fn validate_intrinsic(self) -> Result<(), VodValueError> {
        if self.max_sessions == 0 || self.max_file_bytes == 0 || self.max_duration.is_zero() {
            return Err(VodValueError::InvalidField("limits"));
        }
        Ok(())
    }
}

impl VodSourceDefinition {
    /// # Errors
    ///
    /// Returns an error for an invalid source name, local root value, or HTTP origin syntax.
    pub fn validate_intrinsic(&self) -> Result<(), VodValueError> {
        let (name, valid_value) = match self {
            Self::Local {
                name,
                root_directory,
            } => (name, valid_absolute_path(root_directory)),
            Self::Http { name, origin } => (name, valid_http_origin_syntax(origin)),
        };
        if !valid_source_name(name) {
            return Err(VodValueError::InvalidField("source.name"));
        }
        if !valid_value {
            return Err(VodValueError::InvalidField("source.value"));
        }
        Ok(())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Local { name, .. } | Self::Http { name, .. } => name,
        }
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= MAX_VOD_PATH_BYTES
        && path.components().all(|component| {
            matches!(component, Component::RootDir | Component::Normal(_))
                && component.as_os_str().to_str().is_some_and(|value| {
                    !value
                        .bytes()
                        .any(|byte| byte == 0 || byte.is_ascii_control())
                })
        })
}

fn valid_http_origin_syntax(value: &str) -> bool {
    if value.len() > MAX_VOD_ORIGIN_BYTES {
        return false;
    }
    let Ok(uri) = value.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    let has_query = uri
        .path_and_query()
        .is_some_and(|path_and_query| path_and_query.query().is_some());
    !authority.as_str().contains('@')
        && !has_query
        && !value.contains('#')
        && !authority.host().is_empty()
        && authority.port_u16() != Some(0)
        && !uri.path().contains("..")
        && !uri.path().contains('%')
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum VodError {
    #[error("VOD source is not configured")]
    SourceNotFound,
    #[error("VOD source session limit reached")]
    SessionLimit,
    #[error("VOD path is invalid")]
    InvalidPath,
    #[error("VOD object was not found")]
    NotFound,
    #[error("VOD root cannot be opened safely")]
    RootOpen,
    #[error("VOD object exceeds its configured size bound")]
    TooLarge,
    #[error("VOD origin is not allowed")]
    OriginDenied,
    #[error("VOD origin request failed")]
    Fetch,
    #[error("VOD object is not a valid FLV stream")]
    InvalidFlv,
    #[error("VOD media payload is invalid")]
    InvalidMedia,
    #[error("VOD byte range is invalid")]
    InvalidRange,
}

#[cfg(test)]
mod tests;
