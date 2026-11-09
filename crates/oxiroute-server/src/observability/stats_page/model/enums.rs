//! Closed vocabularies of the snapshot. Each enum mirrors a runtime enum variant for variant, so the
//! compiler rejects a runtime change the page contract has not followed.

use schemars::JsonSchema;
use serde::Serialize;

macro_rules! mirror_enum {
    ($(#[$meta:meta])* $name:ident <= $source:path { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name {
            $($variant),+
        }

        impl From<$source> for $name {
            fn from(source: $source) -> Self {
                match source {
                    $(<$source>::$variant => Self::$variant),+
                }
            }
        }
    };
}

mirror_enum!(
    AdministrativeState
        <= crate::AdministrativeState {
            Ready,
            Drain,
            Maintenance
        }
);
mirror_enum!(
    /// Operator override of the observed health: `auto` follows the checks.
    HealthOverride <= crate::HealthOverride { Auto, Up, Down }
);
mirror_enum!(
    /// What the active checks observed: `unchecked` when the server has no checks, `unknown` before
    /// the first result.
    HealthState <= crate::EndpointHealthState { Unchecked, Unknown, Healthy, Unhealthy }
);
mirror_enum!(
    /// Why a check, a passive observation or the canary failed.
    HealthFailure <= crate::HealthFailure { Timeout, ConnectFailed, UnexpectedStatus, ProtocolError, Slow }
);
mirror_enum!(ServerOrigin <= crate::ServerOrigin { Static, Dynamic });
mirror_enum!(
    ListenerState
        <= crate::ListenerRuntimeState {
            Configured,
            Listening,
            Stopped,
            Failed
        }
);
mirror_enum!(
    ComponentState
        <= crate::ComponentState {
            Healthy,
            Degraded,
            Unsupported
        }
);
mirror_enum!(
    CanaryResult
        <= crate::routing::CanaryResult {
            Ok,
            Slow,
            Failed,
            Busy,
            Unavailable
        }
);
mirror_enum!(
    HttpOutcome
        <= crate::HttpOperationResult {
            Success,
            ClientError,
            ServerError,
            UpstreamError,
            Timeout,
            Cancelled,
            InternalError
        }
);
mirror_enum!(
    TcpRelayOutcome
        <= crate::TcpRelayResult {
            Success,
            ConnectError,
            ConnectTimeout,
            IdleTimeout,
            LifetimeTimeout,
            Cancelled,
            IoError,
            AccountingError,
            ProxyProtocolError
        }
);
mirror_enum!(
    ProxyProtocolOutcome
        <= crate::ProxyProtocolResult {
            Accepted,
            Sent,
            Timeout,
            Cancelled,
            Malformed,
            Unsupported,
            Mismatch,
            IoError
        }
);
mirror_enum!(
    CertbotWatcherHealth
        <= crate::CertbotWatcherHealth {
            Healthy,
            Degraded,
            Stopped
        }
);

/// The state a person reads first: the server's own judgement of whether it takes traffic.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ServerStatus {
    Up,
    Down,
    Drain,
    Maint,
    Ejected,
    Unchecked,
    Unknown,
}

/// Worst-of-children state of a pool.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PoolStatus {
    /// Every server is available.
    Up,
    /// Some servers are available.
    Degraded,
    /// No server is available.
    Down,
    /// The pool has no servers.
    Empty,
}

mirror_enum!(RetiredState <= crate::RetiredState { Draining, Closing });

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CertificateSource {
    Certbot,
    AcmeManaged,
    DirectFile,
}

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RouteAction {
    Proxy,
    FixedResponse,
    Redirect,
    StaticFiles,
}

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SelectorKind {
    NormalizedHost,
    ExactAuthority,
    AsciiCaseInsensitiveExactAuthority,
    NginxLeadingWildcard,
    NginxLeadingDot,
    SegmentPrefix,
    RawPrefix,
    Exact,
    AsciiCaseInsensitiveExact,
}

/// The `snake_case` name a closed enum serialises to: CSV and HTML print the words the JSON carries.
pub(crate) fn wire_name(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("closed enums serialise to a string"),
    }
}
