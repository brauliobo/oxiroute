use schemars::JsonSchema;
use serde::Serialize;

use super::{
    AdministrativeState, Counter, Histogram, HttpOutcome, ListenerState, ProxyProtocolOutcome,
    TcpRelayOutcome,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListenerStats {
    pub(crate) name: String,
    /// `http`, `http3`, `tcp`, `udp`, `rtmp`, `forward_http1`, `forward_http2` or `forward_http3`.
    pub(crate) protocol: String,
    /// The configured bind with its transport: `socket:<address>`, `udp:<address>` or `unix:<path>`.
    pub(crate) bind: String,
    /// The HTTP, forward-proxy or TCP service the listener serves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tls_profile: Option<String>,
    /// The pool a TCP listener relays to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) upstream_pool: Option<String>,
    pub(crate) state: ListenerState,
    pub(crate) administrative_state: AdministrativeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_connections: Option<u64>,
    pub(crate) active_connections: u64,
    pub(crate) accepted_connections: Counter,
    pub(crate) rejected_connections: Counter,
    /// Bytes read from clients.
    pub(crate) bytes_received: Counter,
    /// Bytes written to clients.
    pub(crate) bytes_sent: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) http: Option<HttpStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tcp_relays: Option<TcpRelayStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proxy_protocol: Option<ProxyProtocolStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache: Option<CacheStats>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "{T}Count")]
pub(crate) struct OutcomeCount<T> {
    pub(crate) result: T,
    pub(crate) count: Counter,
}

/// Finished HTTP operations by outcome; absent until the listener has finished one. The proxy keeps no
/// per-listener status-class counters.
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HttpStats {
    pub(crate) outcomes: Vec<OutcomeCount<HttpOutcome>>,
    /// Sum of `outcomes`.
    pub(crate) total: Counter,
    pub(crate) latency: Histogram,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TcpRelayStats {
    pub(crate) outcomes: Vec<OutcomeCount<TcpRelayOutcome>>,
    pub(crate) total: Counter,
    pub(crate) latency: Histogram,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProxyProtocolStats {
    pub(crate) outcomes: Vec<OutcomeCount<ProxyProtocolOutcome>>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheStats {
    pub(crate) hits: Counter,
    pub(crate) misses: Counter,
    pub(crate) admissions: Counter,
    pub(crate) evictions: Counter,
}
