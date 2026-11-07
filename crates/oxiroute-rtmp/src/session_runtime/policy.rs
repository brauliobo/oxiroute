#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpAccessAction {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RtmpNetwork {
    All,
    Cidr { address: IpAddr, prefix: u8 },
}

impl RtmpNetwork {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value == "all" {
            return Some(Self::All);
        }
        let (address, prefix) = value.split_once('/').map_or_else(
            || {
                let address = value.parse::<IpAddr>().ok()?;
                Some((address, if address.is_ipv4() { 32 } else { 128 }))
            },
            |(address, prefix)| Some((address.parse().ok()?, prefix.parse().ok()?)),
        )?;
        (prefix <= if address.is_ipv4() { 32 } else { 128 })
            .then_some(Self::Cidr { address, prefix })
    }

    #[must_use]
    pub const fn is_representable(&self) -> bool {
        match self {
            Self::All => true,
            Self::Cidr { address, prefix } => *prefix <= if address.is_ipv4() { 32 } else { 128 },
        }
    }

    fn matches(&self, peer: Option<IpAddr>) -> bool {
        match self {
            Self::All => true,
            Self::Cidr { address, prefix } => peer.is_some_and(|peer| match (address, peer) {
                (IpAddr::V4(address), IpAddr::V4(peer)) => {
                    masked(u32::from(*address), 32, *prefix) == masked(u32::from(peer), 32, *prefix)
                }
                (IpAddr::V6(address), IpAddr::V6(peer)) => {
                    masked(u128::from(*address), 128, *prefix)
                        == masked(u128::from(peer), 128, *prefix)
                }
                _ => false,
            }),
        }
    }
}

fn masked(value: impl Into<u128>, bits: u8, prefix: u8) -> u128 {
    let value = value.into();
    if prefix == 0 {
        0
    } else {
        value & (u128::MAX << u32::from(bits - prefix))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpAccessRule {
    action: RtmpAccessAction,
    network: RtmpNetwork,
}

impl RtmpAccessRule {
    #[must_use]
    pub fn new(action: RtmpAccessAction, network: RtmpNetwork) -> Self {
        Self { action, network }
    }

    #[must_use]
    pub const fn action(&self) -> RtmpAccessAction {
        self.action
    }

    #[must_use]
    pub const fn network(&self) -> &RtmpNetwork {
        &self.network
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpTokenPolicy {
    parameter: Arc<str>,
    secret: Arc<[u8]>,
}

impl std::fmt::Debug for RtmpTokenPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpTokenPolicy")
            .field("source", &"stream_query")
            .field("parameter", &self.parameter)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl RtmpTokenPolicy {
    #[must_use]
    pub fn stream_query(parameter: impl Into<Arc<str>>, secret: impl AsRef<[u8]>) -> Option<Self> {
        let parameter = parameter.into();
        let secret = secret.as_ref();
        let valid = (1..=32).contains(&parameter.len())
            && parameter
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            && !secret.is_empty()
            && secret.len() <= MAX_RTMP_TOKEN_BYTES
            && secret
                .iter()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'=' | b'#' | b'?'));
        valid.then(|| Self {
            parameter,
            secret: Arc::from(secret),
        })
    }

    #[must_use]
    pub fn parameter(&self) -> &str {
        &self.parameter
    }

    fn authorize(&self, query: Option<&str>) -> Result<(), RtmpAuthorizationError> {
        let Some(query) = query else {
            return Err(RtmpAuthorizationError::TokenMissing);
        };
        let mut value = None;
        for pair in query.split('&') {
            let Some((parameter, candidate)) = pair.split_once('=') else {
                return Err(RtmpAuthorizationError::QueryMalformed);
            };
            if parameter == self.parameter.as_ref() {
                if value.is_some() {
                    return Err(RtmpAuthorizationError::TokenRejected);
                }
                value = Some(candidate);
            }
        }
        let Some(value) = value else {
            return Err(RtmpAuthorizationError::TokenMissing);
        };
        if constant_time_eq(value.as_bytes(), &self.secret) {
            Ok(())
        } else {
            Err(RtmpAuthorizationError::TokenRejected)
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..MAX_RTMP_TOKEN_BYTES {
        let left = left.get(index).copied().unwrap_or_default();
        let right = right.get(index).copied().unwrap_or_default();
        difference |= left ^ right;
    }
    difference == 0
}

#[derive(Clone, Debug, Default)]
pub struct RtmpAccessPolicy {
    rules: Arc<Vec<RtmpAccessRule>>,
    token: Option<RtmpTokenPolicy>,
}

impl RtmpAccessPolicy {
    #[must_use]
    pub fn new(
        rules: impl IntoIterator<Item = RtmpAccessRule>,
        token: Option<RtmpTokenPolicy>,
    ) -> Self {
        Self {
            rules: Arc::new(rules.into_iter().collect()),
            token,
        }
    }

    pub(super) fn authorize(
        &self,
        peer: Option<IpAddr>,
        query: Option<&str>,
    ) -> Result<(), RtmpAuthorizationError> {
        if !self.rules.is_empty() {
            let Some(rule) = self.rules.iter().find(|rule| rule.network.matches(peer)) else {
                return Err(RtmpAuthorizationError::NetworkDenied);
            };
            if rule.action == RtmpAccessAction::Deny {
                return Err(RtmpAuthorizationError::NetworkDenied);
            }
        }
        if let Some(token) = &self.token {
            token.authorize(query)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpSessionCeilings {
    pub max_connections: usize,
    pub max_publishers: usize,
    pub max_viewers: usize,
}

impl RtmpSessionCeilings {
    #[must_use]
    pub const fn new(max_connections: usize, max_publishers: usize, max_viewers: usize) -> Self {
        Self {
            max_connections,
            max_publishers,
            max_viewers,
        }
    }

    /// # Errors
    ///
    /// Returns an error when any application session ceiling is zero.
    pub const fn validate_intrinsic(self) -> Result<(), RtmpSessionLimitError> {
        if self.max_connections == 0 || self.max_publishers == 0 || self.max_viewers == 0 {
            return Err(RtmpSessionLimitError);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("RTMP session limit must be nonzero")]
pub struct RtmpSessionLimitError;

impl Default for RtmpSessionCeilings {
    fn default() -> Self {
        Self::new(
            DEFAULT_RTMP_APPLICATION_CONNECTIONS,
            DEFAULT_RTMP_APPLICATION_PUBLISHERS,
            DEFAULT_RTMP_APPLICATION_VIEWERS,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum RtmpAuthorizationError {
    NetworkDenied,
    TokenMissing,
    TokenRejected,
    QueryMalformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum SessionCounter {
    Connections,
    Publishers,
    Viewers,
}

#[derive(Clone, Copy, Debug, Default)]
struct SessionCounts {
    connections: usize,
    publishers: usize,
    viewers: usize,
}

pub(super) struct ApplicationAdmission {
    limits: RtmpSessionCeilings,
    counts: Mutex<SessionCounts>,
}

impl ApplicationAdmission {
    pub(super) fn new(limits: RtmpSessionCeilings) -> Self {
        Self {
            limits,
            counts: Mutex::new(SessionCounts::default()),
        }
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        counter: SessionCounter,
    ) -> Result<ApplicationSessionLease, SessionLimitError> {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (current, maximum) = match counter {
            SessionCounter::Connections => (&mut counts.connections, self.limits.max_connections),
            SessionCounter::Publishers => (&mut counts.publishers, self.limits.max_publishers),
            SessionCounter::Viewers => (&mut counts.viewers, self.limits.max_viewers),
        };
        if *current >= maximum {
            return Err(SessionLimitError { counter, maximum });
        }
        *current += 1;
        Ok(ApplicationSessionLease {
            admission: Arc::clone(self),
            counter,
        })
    }
}

pub(in super::super) struct ApplicationSessionLease {
    admission: Arc<ApplicationAdmission>,
    counter: SessionCounter,
}

impl Drop for ApplicationSessionLease {
    fn drop(&mut self) {
        let mut counts = self
            .admission
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = match self.counter {
            SessionCounter::Connections => &mut counts.connections,
            SessionCounter::Publishers => &mut counts.publishers,
            SessionCounter::Viewers => &mut counts.viewers,
        };
        *current = current.saturating_sub(1);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) struct SessionLimitError {
    pub(in super::super) counter: SessionCounter,
    pub(in super::super) maximum: usize,
}

/// Runtime-only admission bounds for one RTMP session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpSessionLimits {
    pub max_inbound_chunk_size: u32,
    pub max_inbound_message_size: usize,
    pub window_ack_size: u32,
    pub max_amf0_depth: usize,
    pub max_amf0_container_entries: usize,
    pub max_amf0_values: usize,
    pub max_amf0_string_bytes: usize,
}

impl RtmpSessionLimits {
    /// Creates explicit inbound chunk, message, and AMF0 admission bounds.
    #[must_use]
    pub const fn new(
        max_inbound_chunk_size: u32,
        max_inbound_message_size: usize,
        max_amf0_depth: usize,
        max_amf0_container_entries: usize,
        max_amf0_values: usize,
        max_amf0_string_bytes: usize,
    ) -> Self {
        Self {
            max_inbound_chunk_size,
            max_inbound_message_size,
            window_ack_size: DEFAULT_RTMP_ACK_WINDOW_SIZE,
            max_amf0_depth,
            max_amf0_container_entries,
            max_amf0_values,
            max_amf0_string_bytes,
        }
    }

    #[must_use]
    pub const fn with_window_ack_size(mut self, window_ack_size: u32) -> Self {
        self.window_ack_size = window_ack_size;
        self
    }

    #[must_use]
    pub const fn with_max_inbound_message_size(mut self, max_inbound_message_size: usize) -> Self {
        self.max_inbound_message_size = max_inbound_message_size;
        self
    }

    pub(in super::super) fn amf0_limits(self) -> Amf0Limits {
        Amf0Limits::new(
            self.max_amf0_depth,
            self.max_amf0_container_entries,
            self.max_amf0_values,
            self.max_amf0_string_bytes,
        )
    }

    /// # Errors
    ///
    /// Returns an error when any inbound protocol or AMF admission bound is zero.
    pub const fn validate_intrinsic(self) -> Result<(), RtmpSessionLimitError> {
        if self.max_inbound_chunk_size == 0
            || self.max_inbound_chunk_size > MAX_INBOUND_CHUNK_SIZE
            || self.max_inbound_message_size == 0
            || self.max_inbound_message_size > MAX_INBOUND_MESSAGE_SIZE
            || self.window_ack_size == 0
            || self.max_amf0_depth == 0
            || self.max_amf0_depth > MAX_INBOUND_AMF0_DEPTH
            || self.max_amf0_container_entries == 0
            || self.max_amf0_container_entries > MAX_INBOUND_AMF0_CONTAINER_ENTRIES
            || self.max_amf0_values == 0
            || self.max_amf0_values > MAX_INBOUND_AMF0_VALUES
            || self.max_amf0_string_bytes == 0
            || self.max_amf0_string_bytes > MAX_INBOUND_AMF0_STRING_BYTES
        {
            return Err(RtmpSessionLimitError);
        }
        Ok(())
    }
}

impl Default for RtmpSessionLimits {
    fn default() -> Self {
        Self::new(
            MAX_INBOUND_CHUNK_SIZE,
            MAX_INBOUND_MESSAGE_SIZE,
            MAX_INBOUND_AMF0_DEPTH,
            MAX_INBOUND_AMF0_CONTAINER_ENTRIES,
            MAX_INBOUND_AMF0_VALUES,
            MAX_INBOUND_AMF0_STRING_BYTES,
        )
    }
}
