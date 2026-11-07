use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Runtime bounds for same-daemon RTMP worker auto-push.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpAutoPushConfig {
    pub enabled: bool,
    pub socket_dir: PathBuf,
    pub secret_file: Option<PathBuf>,
    pub reconnect_interval: Duration,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_peers: usize,
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub max_streams: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpAutoPushConfigError {
    #[error("auto-push field `{0}` is invalid")]
    InvalidField(&'static str),
}

impl RtmpAutoPushConfig {
    /// Validates value-only bounds and paths without reading credentials or creating sockets.
    ///
    /// # Errors
    ///
    /// Returns an error when an intrinsic path, duration, or queue/admission bound is invalid.
    pub fn validate_intrinsic(&self) -> Result<(), RtmpAutoPushConfigError> {
        if !valid_absolute_path(&self.socket_dir) {
            return Err(RtmpAutoPushConfigError::InvalidField("socket_dir"));
        }
        if self
            .secret_file
            .as_deref()
            .is_some_and(|path| !valid_absolute_path(path))
        {
            return Err(RtmpAutoPushConfigError::InvalidField("secret_file"));
        }
        if self.reconnect_interval.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("reconnect_interval"));
        }
        if self.connect_timeout.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("connect_timeout"));
        }
        if self.handshake_timeout.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("handshake_timeout"));
        }
        for (field, value) in [
            ("max_peers", self.max_peers),
            ("max_queue_messages", self.max_queue_messages),
            ("max_queue_bytes", self.max_queue_bytes),
            ("max_streams", self.max_streams),
        ] {
            if value == 0 {
                return Err(RtmpAutoPushConfigError::InvalidField(field));
            }
        }
        Ok(())
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4_096
        && path.components().all(|component| {
            matches!(component, Component::RootDir | Component::Normal(_))
                && component.as_os_str().to_str().is_some_and(|value| {
                    !value
                        .bytes()
                        .any(|byte| byte == 0 || byte.is_ascii_control())
                })
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpAutoPushError {
    #[error("RTMP auto-push is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("RTMP auto-push local transport is unavailable")]
    TransportUnavailable,
    #[error("RTMP auto-push local credentials are unavailable")]
    CredentialsUnavailable,
    #[error("RTMP auto-push admission is closed")]
    AdmissionClosed,
    #[error("RTMP auto-push stream limit reached")]
    StreamLimit,
    #[error("RTMP auto-push peer limit reached")]
    PeerLimit,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RtmpAutoPushStatus {
    pub enabled: bool,
    pub started: bool,
    pub peers: usize,
    pub source_streams: usize,
    pub remote_streams: usize,
    pub frames_sent: u64,
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub authentication_failures: u64,
    pub reconnects: u64,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub last_failure: Option<RtmpAutoPushError>,
}

#[cfg(unix)]
mod wire;

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::{AutoPushCoordinator, AutoPushPublisher};

#[cfg(not(unix))]
mod unsupported;

#[cfg(not(unix))]
pub(crate) use unsupported::{AutoPushCoordinator, AutoPushPublisher};

#[cfg(all(test, unix))]
mod tests;
