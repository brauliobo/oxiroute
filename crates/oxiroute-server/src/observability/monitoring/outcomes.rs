#[allow(clippy::wildcard_imports)]
use super::*;

/// Fixed upper bounds for request and relay latency buckets, in milliseconds.
pub const OPERATION_LATENCY_BUCKETS_MS: &[u64] = &[
    1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    Healthy,
    Degraded,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl ComponentStatus {
    pub(super) const fn healthy() -> Self {
        Self {
            state: ComponentState::Healthy,
            reason: None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    const fn unsupported(reason: &'static str) -> Self {
        Self {
            state: ComponentState::Unsupported,
            reason: Some(reason),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpOperationResult {
    Success,
    ClientError,
    ServerError,
    UpstreamError,
    Timeout,
    Cancelled,
    InternalError,
}

impl HttpOperationResult {
    pub(super) const ALL: [Self; 7] = [
        Self::Success,
        Self::ClientError,
        Self::ServerError,
        Self::UpstreamError,
        Self::Timeout,
        Self::Cancelled,
        Self::InternalError,
    ];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ClientError => 1,
            Self::ServerError => 2,
            Self::UpstreamError => 3,
            Self::Timeout => 4,
            Self::Cancelled => 5,
            Self::InternalError => 6,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::UpstreamError => "upstream_error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::InternalError => "internal_error",
        }
    }

    #[must_use]
    pub const fn from_status(status: Option<u16>) -> Self {
        match status {
            Some(200..=399) => Self::Success,
            Some(400..=499) => Self::ClientError,
            Some(500..=599) => Self::ServerError,
            Some(_) => Self::InternalError,
            None => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpRelayResult {
    Success,
    ConnectError,
    ConnectTimeout,
    IdleTimeout,
    LifetimeTimeout,
    Cancelled,
    IoError,
    AccountingError,
    ProxyProtocolError,
}

impl TcpRelayResult {
    pub(super) const ALL: [Self; 9] = [
        Self::Success,
        Self::ConnectError,
        Self::ConnectTimeout,
        Self::IdleTimeout,
        Self::LifetimeTimeout,
        Self::Cancelled,
        Self::IoError,
        Self::AccountingError,
        Self::ProxyProtocolError,
    ];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ConnectError => 1,
            Self::ConnectTimeout => 2,
            Self::IdleTimeout => 3,
            Self::LifetimeTimeout => 4,
            Self::Cancelled => 5,
            Self::IoError => 6,
            Self::AccountingError => 7,
            Self::ProxyProtocolError => 8,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ConnectError => "connect_error",
            Self::ConnectTimeout => "connect_timeout",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeTimeout => "lifetime_timeout",
            Self::Cancelled => "cancelled",
            Self::IoError => "io_error",
            Self::AccountingError => "accounting_error",
            Self::ProxyProtocolError => "proxy_protocol_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedTransport {
    Http,
    Rtmp,
    Forward,
    Cache,
    Tcp,
    Udp,
    H3,
    Acme,
}

impl ObservedTransport {
    pub(super) const ALL: [Self; 8] = [
        Self::Http,
        Self::Rtmp,
        Self::Forward,
        Self::Cache,
        Self::Tcp,
        Self::Udp,
        Self::H3,
        Self::Acme,
    ];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Http => 0,
            Self::Rtmp => 1,
            Self::Forward => 2,
            Self::Cache => 3,
            Self::Tcp => 4,
            Self::Udp => 5,
            Self::H3 => 6,
            Self::Acme => 7,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Rtmp => "rtmp",
            Self::Forward => "forward",
            Self::Cache => "cache",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::H3 => "h3",
            Self::Acme => "acme",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportOutcome {
    Success,
    ClientError,
    ServerError,
    UpstreamError,
    Timeout,
    Rejected,
    Cancelled,
    InternalError,
    Degraded,
}

impl TransportOutcome {
    pub(super) const ALL: [Self; 9] = [
        Self::Success,
        Self::ClientError,
        Self::ServerError,
        Self::UpstreamError,
        Self::Timeout,
        Self::Rejected,
        Self::Cancelled,
        Self::InternalError,
        Self::Degraded,
    ];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::ClientError => 1,
            Self::ServerError => 2,
            Self::UpstreamError => 3,
            Self::Timeout => 4,
            Self::Rejected => 5,
            Self::Cancelled => 6,
            Self::InternalError => 7,
            Self::Degraded => 8,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::UpstreamError => "upstream_error",
            Self::Timeout => "timeout",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::InternalError => "internal_error",
            Self::Degraded => "degraded",
        }
    }

    pub(super) const fn from_http(result: HttpOperationResult) -> Self {
        match result {
            HttpOperationResult::Success => Self::Success,
            HttpOperationResult::ClientError => Self::ClientError,
            HttpOperationResult::ServerError => Self::ServerError,
            HttpOperationResult::UpstreamError => Self::UpstreamError,
            HttpOperationResult::Timeout => Self::Timeout,
            HttpOperationResult::Cancelled => Self::Cancelled,
            HttpOperationResult::InternalError => Self::InternalError,
        }
    }

    pub(super) const fn from_tcp(result: TcpRelayResult) -> Self {
        match result {
            TcpRelayResult::Success => Self::Success,
            TcpRelayResult::ConnectError
            | TcpRelayResult::ConnectTimeout
            | TcpRelayResult::IoError => Self::UpstreamError,
            TcpRelayResult::IdleTimeout | TcpRelayResult::LifetimeTimeout => Self::Timeout,
            TcpRelayResult::Cancelled => Self::Cancelled,
            TcpRelayResult::AccountingError => Self::InternalError,
            TcpRelayResult::ProxyProtocolError => Self::Rejected,
        }
    }

    pub(super) const fn from_admission(error: &MetricsError) -> Self {
        match error {
            MetricsError::ConnectionLimitReached { .. }
            | MetricsError::ProcessConnectionLimitReached { .. }
            | MetricsError::AdministrativeDrain { .. } => Self::Rejected,
            _ => Self::InternalError,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
pub(super) const fn transport_outcome_index(outcome: TransportOutcome) -> u8 {
    outcome.index() as u8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheEvent {
    Hit,
    Miss,
    Admission,
    Eviction,
}

impl CacheEvent {
    pub(super) const ALL: [Self; 4] = [Self::Hit, Self::Miss, Self::Admission, Self::Eviction];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Hit => 0,
            Self::Miss => 1,
            Self::Admission => 2,
            Self::Eviction => 3,
        }
    }
}

pub(super) const fn cache_event_outcome(event: CacheEvent) -> TransportOutcome {
    match event {
        CacheEvent::Hit | CacheEvent::Admission | CacheEvent::Eviction => TransportOutcome::Success,
        CacheEvent::Miss => TransportOutcome::UpstreamError,
    }
}
