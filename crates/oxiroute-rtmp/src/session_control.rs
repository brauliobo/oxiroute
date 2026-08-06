use std::{
    net::IpAddr,
    sync::{Arc, Mutex},
};

use crate::catalog::SessionId;

pub const MAX_RTMP_SESSION_CONTROLS: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpSessionRole {
    Client,
    Publisher,
    Subscriber,
}

impl RtmpSessionRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Publisher => "publisher",
            Self::Subscriber => "subscriber",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpSessionControlAction {
    Client,
    Publisher,
    Subscriber,
}

impl RtmpSessionControlAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Publisher => "publisher",
            Self::Subscriber => "subscriber",
        }
    }

    #[must_use]
    fn matches_streams(self, streams: &[RtmpMessageStreamSnapshot]) -> bool {
        match self {
            Self::Client => true,
            Self::Publisher => streams
                .iter()
                .any(|stream| stream.role == RtmpSessionRole::Publisher),
            Self::Subscriber => streams
                .iter()
                .any(|stream| stream.role == RtmpSessionRole::Subscriber),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpMessageStreamSnapshot {
    pub message_stream_id: u32,
    pub application: String,
    pub stream_name: String,
    pub role: RtmpSessionRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpClientSnapshot {
    pub session_id: SessionId,
    pub service_id: String,
    pub peer_addr: Option<IpAddr>,
    pub connected: bool,
    pub connected_at_unix_ms: u64,
    pub application: Option<String>,
    pub stream_name: Option<String>,
    pub role: RtmpSessionRole,
    pub message_streams: Vec<RtmpMessageStreamSnapshot>,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpSessionControlOutcome {
    pub already_requested: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpSessionControlError {
    #[error("RTMP session does not exist")]
    NotFound,
    #[error("RTMP session revision does not match")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("RTMP session role does not match the requested control target")]
    RoleMismatch {
        target: RtmpSessionControlAction,
        actual: RtmpSessionRole,
    },
    #[error("RTMP session already has a pending control request")]
    AlreadyPending,
}

#[derive(Clone, Copy)]
struct PendingControl {
    action: RtmpSessionControlAction,
    revision: u64,
}

struct ControlState {
    service_id: String,
    peer_addr: Option<IpAddr>,
    connected: bool,
    connected_at_unix_ms: u64,
    application: Option<String>,
    stream_name: Option<String>,
    role: RtmpSessionRole,
    message_streams: Vec<RtmpMessageStreamSnapshot>,
    revision: u64,
    pending: Option<PendingControl>,
}

pub(crate) struct RtmpSessionControl {
    session_id: SessionId,
    state: Mutex<ControlState>,
}

impl RtmpSessionControl {
    pub(crate) fn new(
        session_id: SessionId,
        service_id: &str,
        peer_addr: Option<IpAddr>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            state: Mutex::new(ControlState {
                service_id: service_id.to_owned(),
                peer_addr,
                connected: false,
                connected_at_unix_ms: 0,
                application: None,
                stream_name: None,
                role: RtmpSessionRole::Client,
                message_streams: Vec::new(),
                revision: 0,
                pending: None,
            }),
        })
    }

    pub(crate) fn connected(&self, application: &str, at_unix_ms: u64) {
        let mut state = self.lock();
        state.connected = true;
        state.connected_at_unix_ms = at_unix_ms;
        state.application = Some(application.to_owned());
        state.stream_name = None;
        state.role = RtmpSessionRole::Client;
        state.message_streams.clear();
        state.revision = state.revision.saturating_add(1);
        state.pending = None;
    }

    pub(crate) fn set_message_streams(
        &self,
        application: Option<&str>,
        message_streams: &[RtmpMessageStreamSnapshot],
    ) {
        let mut state = self.lock();
        let first_stream = message_streams.first();
        let next_application = application.map(str::to_owned);
        let next_stream_name = first_stream.map(|stream| stream.stream_name.clone());
        let next_role = first_stream.map_or(RtmpSessionRole::Client, |stream| stream.role);
        if state.application == next_application
            && state.stream_name == next_stream_name
            && state.role == next_role
            && state.message_streams == message_streams
        {
            return;
        }
        state.application = next_application;
        state.stream_name = next_stream_name;
        state.role = next_role;
        state.message_streams = message_streams.to_vec();
        state.revision = state.revision.saturating_add(1);
        state.pending = None;
    }

    pub(crate) fn request(
        &self,
        action: RtmpSessionControlAction,
        expected_revision: u64,
    ) -> Result<RtmpSessionControlOutcome, RtmpSessionControlError> {
        let mut state = self.lock();
        if state.revision != expected_revision {
            return Err(RtmpSessionControlError::RevisionMismatch {
                expected: expected_revision,
                actual: state.revision,
            });
        }
        if !action.matches_streams(&state.message_streams) {
            return Err(RtmpSessionControlError::RoleMismatch {
                target: action,
                actual: state.role,
            });
        }
        if let Some(pending) = state.pending {
            if pending.action == action && pending.revision == expected_revision {
                return Ok(RtmpSessionControlOutcome {
                    already_requested: true,
                });
            }
            return Err(RtmpSessionControlError::AlreadyPending);
        }
        state.pending = Some(PendingControl {
            action,
            revision: expected_revision,
        });
        Ok(RtmpSessionControlOutcome {
            already_requested: false,
        })
    }

    pub(crate) fn take_action(
        &self,
        message_streams: &[RtmpMessageStreamSnapshot],
    ) -> Option<RtmpSessionControlAction> {
        let mut state = self.lock();
        let pending = state.pending.take()?;
        (pending.revision == state.revision && pending.action.matches_streams(message_streams))
            .then_some(pending.action)
    }

    pub(crate) fn snapshot(&self) -> RtmpClientSnapshot {
        let state = self.lock();
        RtmpClientSnapshot {
            session_id: self.session_id,
            service_id: state.service_id.clone(),
            peer_addr: state.peer_addr,
            connected: state.connected,
            connected_at_unix_ms: state.connected_at_unix_ms,
            application: state.application.clone(),
            stream_name: state.stream_name.clone(),
            role: state.role,
            message_streams: state.message_streams.clone(),
            revision: state.revision,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_requires_the_current_role_revision() {
        let session_id = SessionId::new();
        let control = RtmpSessionControl::new(session_id, "edge", None);

        control.connected("live", 10);
        control.set_message_streams(
            Some("live"),
            &[RtmpMessageStreamSnapshot {
                message_stream_id: 1,
                application: "live".into(),
                stream_name: "camera".into(),
                role: RtmpSessionRole::Publisher,
            }],
        );
        let snapshot = control.snapshot();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.role, RtmpSessionRole::Publisher);

        assert!(matches!(
            control.request(RtmpSessionControlAction::Subscriber, snapshot.revision),
            Err(RtmpSessionControlError::RoleMismatch { .. })
        ));
        assert!(
            control
                .request(RtmpSessionControlAction::Publisher, snapshot.revision)
                .is_ok()
        );
        assert_eq!(
            control.take_action(&snapshot.message_streams),
            Some(RtmpSessionControlAction::Publisher)
        );
        assert_eq!(control.take_action(&snapshot.message_streams), None);
        assert!(matches!(
            control.request(RtmpSessionControlAction::Publisher, 1),
            Err(RtmpSessionControlError::RevisionMismatch { .. })
        ));
    }
}
