#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpDestination {
    pub address: SocketAddr,
    pub host: String,
    pub transport: RtmpTransport,
    pub application: String,
    pub stream_name: String,
    pub options: RtmpClientOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPushTarget {
    pub address: SocketAddr,
    pub host: String,
    pub transport: RtmpTransport,
    pub application: RtmpPushApplication,
    pub stream_name: Option<String>,
    pub options: RtmpClientOptions,
    pub config: RtmpRelayConfig,
}

impl RtmpPushTarget {
    #[must_use]
    pub fn expand(&self, stream_name: &str) -> RtmpDestination {
        RtmpDestination {
            address: self.address,
            host: self.host.clone(),
            transport: self.transport,
            application: match &self.application {
                RtmpPushApplication::Exact(application) => application.clone(),
                RtmpPushApplication::StreamName => stream_name.to_owned(),
            },
            stream_name: self
                .stream_name
                .as_deref()
                .map_or_else(|| stream_name.to_owned(), ToOwned::to_owned),
            options: self.options.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPullTarget {
    pub address: SocketAddr,
    pub host: String,
    pub transport: RtmpTransport,
    pub source_application: String,
    pub source_stream_name: String,
    pub local_application: String,
    pub local_stream_name: String,
    pub options: RtmpClientOptions,
    pub config: RtmpRelayConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RtmpPushApplication {
    Exact(String),
    StreamName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpRelayConfig {
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub buffer_duration: Duration,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub reconnect_interval: Duration,
    pub max_chain_depth: u8,
    pub dns_resolver: Option<Arc<RtmpDestinationResolver>>,
}

impl Default for RtmpRelayConfig {
    fn default() -> Self {
        Self {
            max_queue_messages: 256,
            max_queue_bytes: 8 * 1_024 * 1_024,
            buffer_duration: Duration::from_secs(5),
            connect_timeout: Duration::from_millis(500),
            handshake_timeout: Duration::from_secs(2),
            reconnect_interval: Duration::from_secs(3),
            max_chain_depth: 4,
            dns_resolver: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRelayPhase {
    Connecting,
    Publishing,
    Pulling,
    Backoff,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRelayFailure {
    Policy,
    Connect,
    Handshake,
    Session,
    Transport,
    Source,
    Thread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpRelayStatus {
    pub destination: RtmpDestination,
    pub phase: RtmpRelayPhase,
    pub last_failure: Option<RtmpRelayFailure>,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub connection_attempts: u64,
    pub connections: u64,
    pub reconnects: u64,
    pub events_enqueued: u64,
    pub events_sent: u64,
    pub events_dropped: u64,
    pub payload_bytes_sent: u64,
    pub dns_refresh_attempts: u64,
    pub dns_refresh_successes: u64,
    pub dns_refresh_failures: u64,
    pub last_dns_refresh_failure: Option<RtmpDnsRefreshFailure>,
}
