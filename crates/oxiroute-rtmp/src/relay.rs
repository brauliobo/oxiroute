use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    io::{self, Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};

use crate::{
    LiveHub, MediaEvent, MediaEventKind, RtmpClientOptions, RtmpOutboundPolicy, RtmpRegistry,
    RtmpTransport, SessionId, StreamKey, VideoCodec,
    client::{self, RtmpStream},
    clock::unix_time_ms,
    media_snapshot::MediaSnapshotAccumulator,
};

const HANDSHAKE_RESPONSE_BYTES: usize = 3_073;
const RELAY_READ_BUFFER_BYTES: usize = 16 * 1_024;
const RELAY_QUEUE_POLL: Duration = Duration::from_millis(20);
pub const RTMP_RELAY_WORKER_THREADS: usize = 8;
pub const MAX_QUEUED_RTMP_RELAYS: usize = 64;
const MAX_RTMP_DESTINATION_ADDRESSES: usize = 32;

/// Resolves one RTMP destination name without retaining resolver-specific state.
pub trait RtmpDnsResolver: Send + Sync {
    /// Resolves the canonical host and port into a bounded candidate answer.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the resolver cannot produce an answer.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

#[derive(Debug, Default)]
struct SystemRtmpDnsResolver;

impl RtmpDnsResolver for SystemRtmpDnsResolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .take(MAX_RTMP_DESTINATION_ADDRESSES + 1)
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpDestinationResolverError {
    #[error("destination DNS answer is empty")]
    EmptyAnswer,
    #[error("destination DNS answer exceeds the address limit")]
    TooManyAddresses,
    #[error("destination DNS answer contains an invalid address")]
    InvalidAddress,
    #[error("destination address is denied by RTMP outbound policy")]
    Policy,
    #[error("destination resolves to an active RTMP listener")]
    DirectLoop,
    #[error("destination DNS answer has no address in the selected family")]
    FamilyMismatch,
    #[error("destination DNS refresh interval must be nonzero")]
    InvalidRefreshInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpDnsRefreshFailure {
    Resolution,
    AddressSet,
    Policy,
    DirectLoop,
    FamilyMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RtmpDnsRefresh {
    NotDue,
    Refreshed(SocketAddr),
    Failed(RtmpDnsRefreshFailure),
}

struct DestinationResolverState {
    current_address: SocketAddr,
    next_refresh_at: Instant,
    refreshing: bool,
}

/// Shared, bounded DNS state for one compiled RTMP destination.
pub struct RtmpDestinationResolver {
    host: Arc<str>,
    port: u16,
    transport: RtmpTransport,
    address_is_ipv4: bool,
    policy: RtmpOutboundPolicy,
    listener_addresses: Arc<[SocketAddr]>,
    refresh_interval: Duration,
    resolver: Arc<dyn RtmpDnsResolver>,
    state: Arc<Mutex<DestinationResolverState>>,
}

impl fmt::Debug for RtmpDestinationResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpDestinationResolver")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("transport", &self.transport)
            .field("address_is_ipv4", &self.address_is_ipv4)
            .field("refresh_interval", &self.refresh_interval)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RtmpDestinationResolver {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.transport == other.transport
            && self.address_is_ipv4 == other.address_is_ipv4
            && self.policy == other.policy
            && self.listener_addresses == other.listener_addresses
            && self.refresh_interval == other.refresh_interval
            && Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for RtmpDestinationResolver {}

impl RtmpDestinationResolver {
    /// Compiles a destination from its already resolved startup answer.
    ///
    /// The complete startup answer is checked before the selected address is retained. Refreshes
    /// use the same policy and listener-loop checks, and only an address in this initial family can
    /// replace it.
    ///
    /// # Errors
    ///
    /// Returns an error when the startup answer or refresh interval is invalid.
    pub fn from_startup(
        host: impl Into<Arc<str>>,
        port: u16,
        transport: RtmpTransport,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: RtmpOutboundPolicy,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        refresh_interval: Duration,
    ) -> Result<Self, RtmpDestinationResolverError> {
        Self::from_startup_with_resolver(
            host,
            port,
            transport,
            addresses,
            policy,
            listener_addresses,
            refresh_interval,
            Arc::new(SystemRtmpDnsResolver),
        )
    }

    /// Compiles a destination with an injected resolver, primarily for deterministic loopback tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the startup answer or refresh interval is invalid.
    #[expect(
        clippy::too_many_arguments,
        reason = "startup resolver construction keeps destination policy inputs explicit"
    )]
    pub fn from_startup_with_resolver(
        host: impl Into<Arc<str>>,
        port: u16,
        transport: RtmpTransport,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: RtmpOutboundPolicy,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        refresh_interval: Duration,
        resolver: Arc<dyn RtmpDnsResolver>,
    ) -> Result<Self, RtmpDestinationResolverError> {
        if refresh_interval.is_zero() {
            return Err(RtmpDestinationResolverError::InvalidRefreshInterval);
        }
        let host = host.into();
        let listener_addresses: Arc<[SocketAddr]> = listener_addresses.into_iter().collect();
        let address = validate_rtmp_destination_answer(
            &host,
            port,
            transport,
            &policy,
            &listener_addresses,
            addresses,
            None,
        )?;
        Ok(Self {
            host,
            port,
            transport,
            address_is_ipv4: address.is_ipv4(),
            policy,
            listener_addresses,
            refresh_interval,
            resolver,
            state: Arc::new(Mutex::new(DestinationResolverState {
                current_address: address,
                next_refresh_at: Instant::now() + refresh_interval,
                refreshing: false,
            })),
        })
    }

    /// # Panics
    ///
    /// Panics if the resolver state mutex is poisoned.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.state
            .lock()
            .expect("RTMP destination resolver mutex poisoned")
            .current_address
    }

    fn refresh_if_due(&self) -> RtmpDnsRefresh {
        let now = Instant::now();
        {
            let mut state = self
                .state
                .lock()
                .expect("RTMP destination resolver mutex poisoned");
            if state.refreshing || now < state.next_refresh_at {
                return RtmpDnsRefresh::NotDue;
            }
            state.refreshing = true;
        }

        let result = self
            .resolver
            .resolve(&self.host, self.port)
            .map_err(|_| RtmpDnsRefreshFailure::Resolution)
            .and_then(|addresses| {
                validate_rtmp_destination_answer(
                    &self.host,
                    self.port,
                    self.transport,
                    &self.policy,
                    &self.listener_addresses,
                    addresses,
                    Some(self.address_is_ipv4),
                )
                .map_err(|error| match error {
                    RtmpDestinationResolverError::EmptyAnswer
                    | RtmpDestinationResolverError::TooManyAddresses
                    | RtmpDestinationResolverError::InvalidAddress => {
                        RtmpDnsRefreshFailure::AddressSet
                    }
                    RtmpDestinationResolverError::Policy
                    | RtmpDestinationResolverError::InvalidRefreshInterval => {
                        RtmpDnsRefreshFailure::Policy
                    }
                    RtmpDestinationResolverError::DirectLoop => RtmpDnsRefreshFailure::DirectLoop,
                    RtmpDestinationResolverError::FamilyMismatch => {
                        RtmpDnsRefreshFailure::FamilyMismatch
                    }
                })
            });

        let mut state = self
            .state
            .lock()
            .expect("RTMP destination resolver mutex poisoned");
        state.refreshing = false;
        state.next_refresh_at = now
            .checked_add(self.refresh_interval)
            .unwrap_or_else(Instant::now);
        match result {
            Ok(address) => {
                state.current_address = address;
                RtmpDnsRefresh::Refreshed(address)
            }
            Err(error) => RtmpDnsRefresh::Failed(error),
        }
    }
}

fn validate_rtmp_destination_answer(
    host: &str,
    port: u16,
    transport: RtmpTransport,
    policy: &RtmpOutboundPolicy,
    listener_addresses: &[SocketAddr],
    addresses: impl IntoIterator<Item = SocketAddr>,
    family: Option<bool>,
) -> Result<SocketAddr, RtmpDestinationResolverError> {
    let mut addresses: Vec<_> = addresses
        .into_iter()
        .take(MAX_RTMP_DESTINATION_ADDRESSES + 1)
        .collect();
    if addresses.is_empty() {
        return Err(RtmpDestinationResolverError::EmptyAnswer);
    }
    if addresses.len() > MAX_RTMP_DESTINATION_ADDRESSES {
        return Err(RtmpDestinationResolverError::TooManyAddresses);
    }
    if addresses.iter().any(|address| address.port() != port) {
        return Err(RtmpDestinationResolverError::InvalidAddress);
    }
    addresses.sort_unstable();
    addresses.dedup();
    policy
        .validate_resolved(host, &addresses)
        .and_then(|()| policy.validate_transport(transport))
        .map_err(|_| RtmpDestinationResolverError::Policy)?;
    if addresses.iter().any(|destination| {
        listener_addresses.iter().any(|listener| {
            listener.port() == destination.port()
                && (listener.ip() == destination.ip()
                    || listener.ip().is_unspecified()
                        && listener.is_ipv4() == destination.is_ipv4())
        })
    }) {
        return Err(RtmpDestinationResolverError::DirectLoop);
    }
    addresses
        .into_iter()
        .find(|address| family.is_none_or(|is_ipv4| address.is_ipv4() == is_ipv4))
        .ok_or(RtmpDestinationResolverError::FamilyMismatch)
}

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

pub(crate) struct RtmpRelayController {
    shared: Arc<RelayShared>,
}

struct RelayShared {
    destination: RtmpDestination,
    config: RtmpRelayConfig,
    resolver: Option<Arc<RtmpDestinationResolver>>,
    state: Mutex<RelayState>,
    available: Condvar,
}

struct RelayState {
    accepting: bool,
    waiting_for_keyframe: bool,
    queue: VecDeque<MediaEvent>,
    queue_bytes: usize,
    queue_started_at: Option<Instant>,
    cache: RelayCache,
    phase: RtmpRelayPhase,
    last_failure: Option<RtmpRelayFailure>,
    connection_attempts: u64,
    connections: u64,
    reconnects: u64,
    events_enqueued: u64,
    events_sent: u64,
    events_dropped: u64,
    payload_bytes_sent: u64,
    dns_refresh_attempts: u64,
    dns_refresh_successes: u64,
    dns_refresh_failures: u64,
    last_dns_refresh_failure: Option<RtmpDnsRefreshFailure>,
}

#[derive(Default)]
struct RelayCache {
    metadata: Option<MediaEvent>,
    aac_header: Option<MediaEvent>,
    video_headers: BTreeMap<VideoCodec, MediaEvent>,
    keyframe: Option<MediaEvent>,
    latest_audio: Option<MediaEvent>,
}

impl RtmpRelayController {
    pub(crate) fn start(destination: RtmpDestination, config: RtmpRelayConfig) -> Arc<Self> {
        let resolver = config.dns_resolver.clone();
        let shared = Arc::new(RelayShared {
            destination,
            config,
            resolver,
            state: Mutex::new(RelayState {
                accepting: true,
                waiting_for_keyframe: false,
                queue: VecDeque::new(),
                queue_bytes: 0,
                queue_started_at: None,
                cache: RelayCache::default(),
                phase: RtmpRelayPhase::Connecting,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_enqueued: 0,
                events_sent: 0,
                events_dropped: 0,
                payload_bytes_sent: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        });
        let controller = Arc::new(Self { shared });
        if relay_executor()
            .admit(Arc::clone(&controller.shared))
            .is_err()
        {
            let mut state = controller.shared.lock();
            state.accepting = false;
            state.phase = RtmpRelayPhase::Stopped;
            state.last_failure = Some(RtmpRelayFailure::Thread);
        }
        controller
    }

    pub(crate) fn try_enqueue(&self, event: MediaEvent) {
        let mut state = self.shared.lock();
        if !state.accepting {
            return;
        }
        let codec_header_changed = state.cache.update(&event);
        if codec_header_changed {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = true;
        }
        if state.waiting_for_keyframe {
            if event.kind() != MediaEventKind::VideoKeyframe {
                state.events_dropped = state.events_dropped.saturating_add(1);
                return;
            }
            let bootstrap = state.cache.bootstrap();
            if fits_queue(&bootstrap, &self.shared.config) {
                for cached in bootstrap {
                    state.queue_bytes += cached.payload_len();
                    state.queue.push_back(cached);
                }
                state.queue_started_at = (!state.queue.is_empty()).then(Instant::now);
                state.waiting_for_keyframe = false;
                drop(state);
                self.shared.available.notify_one();
                return;
            }
            state.events_dropped = state.events_dropped.saturating_add(1);
            return;
        }
        let exceeds_messages = state.queue.len() >= self.shared.config.max_queue_messages;
        let queue_bytes = state.queue_bytes.checked_add(event.payload_len());
        if exceeds_messages
            || queue_bytes.is_none_or(|bytes| bytes > self.shared.config.max_queue_bytes)
        {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state
                .events_dropped
                .saturating_add(discarded)
                .saturating_add(1);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = !state.cache.video_headers.is_empty();
            if event.kind() == MediaEventKind::VideoKeyframe {
                let bootstrap = state.cache.bootstrap();
                if fits_queue(&bootstrap, &self.shared.config) {
                    for cached in bootstrap {
                        state.queue_bytes += cached.payload_len();
                        state.queue.push_back(cached);
                    }
                    state.queue_started_at = (!state.queue.is_empty()).then(Instant::now);
                    state.waiting_for_keyframe = false;
                }
            }
            self.shared.available.notify_one();
            return;
        }

        state.queue_bytes = queue_bytes.expect("bounded relay queue byte sum was checked");
        if state.queue.is_empty() {
            state.queue_started_at = Some(Instant::now());
        }
        state.queue.push_back(event);
        state.events_enqueued = state.events_enqueued.saturating_add(1);
        drop(state);
        self.shared.available.notify_one();
    }

    pub(crate) fn status(&self) -> RtmpRelayStatus {
        let state = self.shared.lock();
        let mut destination = self.shared.destination.clone();
        if let Some(resolver) = &self.shared.resolver {
            destination.address = resolver.address();
        }
        RtmpRelayStatus {
            destination,
            phase: state.phase,
            last_failure: state.last_failure,
            queue_messages: state.queue.len(),
            queue_bytes: state.queue_bytes,
            connection_attempts: state.connection_attempts,
            connections: state.connections,
            reconnects: state.reconnects,
            events_enqueued: state.events_enqueued,
            events_sent: state.events_sent,
            events_dropped: state.events_dropped,
            payload_bytes_sent: state.payload_bytes_sent,
            dns_refresh_attempts: state.dns_refresh_attempts,
            dns_refresh_successes: state.dns_refresh_successes,
            dns_refresh_failures: state.dns_refresh_failures,
            last_dns_refresh_failure: state.last_dns_refresh_failure,
        }
    }

    pub(crate) fn deactivate(&self) {
        {
            let mut state = self.shared.lock();
            state.accepting = false;
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
        }
        self.shared.available.notify_all();
    }
}

impl Drop for RtmpRelayController {
    fn drop(&mut self) {
        self.deactivate();
    }
}

pub(crate) struct RtmpPullController {
    shared: Arc<PullShared>,
}

struct PullShared {
    service_id: Arc<str>,
    target: RtmpPullTarget,
    resolver: Option<Arc<RtmpDestinationResolver>>,
    registry: Arc<RtmpRegistry>,
    hub: LiveHub,
    state: Mutex<PullState>,
    available: Condvar,
}

struct PullState {
    accepting: bool,
    phase: RtmpRelayPhase,
    last_failure: Option<RtmpRelayFailure>,
    connection_attempts: u64,
    connections: u64,
    reconnects: u64,
    events_received: u64,
    payload_bytes_received: u64,
    dns_refresh_attempts: u64,
    dns_refresh_successes: u64,
    dns_refresh_failures: u64,
    last_dns_refresh_failure: Option<RtmpDnsRefreshFailure>,
}

impl RtmpPullController {
    pub(crate) fn start(
        service_id: Arc<str>,
        target: RtmpPullTarget,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
    ) -> Arc<Self> {
        let resolver = target.config.dns_resolver.clone();
        let shared = Arc::new(PullShared {
            service_id,
            target,
            resolver,
            registry,
            hub,
            state: Mutex::new(PullState {
                accepting: true,
                phase: RtmpRelayPhase::Connecting,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_received: 0,
                payload_bytes_received: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        });
        let controller = Arc::new(Self { shared });
        if pull_executor()
            .admit(Arc::clone(&controller.shared))
            .is_err()
        {
            let mut state = controller.shared.lock();
            state.accepting = false;
            state.phase = RtmpRelayPhase::Stopped;
            state.last_failure = Some(RtmpRelayFailure::Thread);
        }
        controller
    }

    pub(crate) fn deactivate(&self) {
        let mut state = self.shared.lock();
        state.accepting = false;
        drop(state);
        self.shared.available.notify_all();
    }
}

impl Drop for RtmpPullController {
    fn drop(&mut self) {
        self.deactivate();
    }
}

impl PullShared {
    fn lock(&self) -> MutexGuard<'_, PullState> {
        self.state.lock().expect("RTMP pull state mutex poisoned")
    }

    fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    fn wait_backoff(&self) -> bool {
        let state = self.lock();
        let (state, _) = self
            .available
            .wait_timeout_while(state, self.target.config.reconnect_interval, |state| {
                state.accepting
            })
            .expect("RTMP pull state mutex poisoned during backoff");
        state.accepting
    }

    fn record_failure(&self, failure: RtmpRelayFailure) {
        let mut state = self.lock();
        state.last_failure = Some(failure);
        state.phase = RtmpRelayPhase::Backoff;
    }

    fn set_phase(&self, phase: RtmpRelayPhase) {
        self.lock().phase = phase;
    }

    fn refresh_address(&self) -> SocketAddr {
        let Some(resolver) = &self.resolver else {
            return self.target.address;
        };
        match resolver.refresh_if_due() {
            RtmpDnsRefresh::NotDue => {}
            RtmpDnsRefresh::Refreshed(_) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_successes = state.dns_refresh_successes.saturating_add(1);
                state.last_dns_refresh_failure = None;
            }
            RtmpDnsRefresh::Failed(failure) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_failures = state.dns_refresh_failures.saturating_add(1);
                state.last_dns_refresh_failure = Some(failure);
            }
        }
        resolver.address()
    }
}

struct PullExecutor {
    sender: SyncSender<Arc<PullShared>>,
}

impl PullExecutor {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Arc<PullShared>>(MAX_QUEUED_RTMP_RELAYS);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..RTMP_RELAY_WORKER_THREADS {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("rtmp-pull-worker-{index}"))
                .spawn(move || {
                    loop {
                        let task = receiver
                            .lock()
                            .expect("RTMP pull executor mutex poisoned")
                            .recv();
                        let Ok(shared) = task else {
                            return;
                        };
                        run_pull(&shared);
                    }
                })
                .expect("shared RTMP pull worker must start");
        }
        Self { sender }
    }

    fn admit(&self, shared: Arc<PullShared>) -> Result<(), ()> {
        self.sender.try_send(shared).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
        })
    }
}

fn pull_executor() -> &'static PullExecutor {
    static EXECUTOR: OnceLock<PullExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(PullExecutor::new)
}

#[allow(
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::too_many_lines,
    reason = "the pull worker keeps connection, RTMP session, and publisher-incarnation cleanup in one bounded loop"
)]
fn run_pull(shared: &PullShared) {
    while shared.is_accepting() {
        {
            let mut state = shared.lock();
            state.phase = RtmpRelayPhase::Connecting;
            state.connection_attempts = state.connection_attempts.saturating_add(1);
        }
        let address = shared.refresh_address();
        let Ok(mut stream) = client::connect_stream(
            &shared.target.host,
            address,
            shared.target.transport,
            shared.target.config.connect_timeout,
            shared.target.config.handshake_timeout,
        ) else {
            shared.record_failure(RtmpRelayFailure::Connect);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        };
        if establish_transport(&mut stream, shared.target.config.handshake_timeout).is_err() {
            shared.record_failure(RtmpRelayFailure::Handshake);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        }
        let mut session = match establish_pull_session(
            &mut stream,
            &shared.target,
            shared.target.config.handshake_timeout,
        ) {
            Ok(session) => session,
            Err(failure) => {
                shared.record_failure(failure);
                if !shared.wait_backoff() {
                    break;
                }
                continue;
            }
        };
        let now = unix_time_ms();
        let key = StreamKey::new(
            shared.service_id.as_ref(),
            &shared.target.local_application,
            &shared.target.local_stream_name,
        );
        let publisher_session_id = SessionId::new();
        let (lease, mut registration) = {
            let _transaction = shared.hub.lock_roles();
            let lease = match shared.hub.attach_publisher(key.clone()) {
                Ok(lease) => lease,
                Err(_) => {
                    shared.record_failure(RtmpRelayFailure::Source);
                    if !shared.wait_backoff() {
                        break;
                    }
                    continue;
                }
            };
            let registration =
                match shared
                    .registry
                    .register_publisher(key, publisher_session_id, Vec::new(), now)
                {
                    Ok(registration) => registration,
                    Err(_) => {
                        drop(lease);
                        shared.record_failure(RtmpRelayFailure::Source);
                        if !shared.wait_backoff() {
                            break;
                        }
                        continue;
                    }
                };
            (lease, registration)
        };
        {
            let mut state = shared.lock();
            state.connections = state.connections.saturating_add(1);
            state.reconnects = state.connections.saturating_sub(1);
            state.phase = RtmpRelayPhase::Pulling;
            state.last_failure = None;
        }

        let stream_id = registration.stream_id();
        let mut media = MediaSnapshotAccumulator::default();
        let mut sequence = 0_u64;
        let mut failed = None;
        let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
        while failed.is_none() && shared.is_accepting() {
            let count = match stream.read(&mut buffer) {
                Ok(0) => {
                    failed = Some(RtmpRelayFailure::Transport);
                    continue;
                }
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => {
                    failed = Some(RtmpRelayFailure::Transport);
                    continue;
                }
            };
            let results = match session.handle_input(&buffer[..count]) {
                Ok(results) => results,
                Err(_) => {
                    failed = Some(RtmpRelayFailure::Session);
                    continue;
                }
            };
            for result in results {
                match result {
                    ClientSessionResult::OutboundResponse(packet) => {
                        if stream.write_all(&packet.bytes).is_err() {
                            failed = Some(RtmpRelayFailure::Transport);
                            break;
                        }
                    }
                    ClientSessionResult::RaisedEvent(event) => {
                        let Some(event) = pull_media_event(event) else {
                            continue;
                        };
                        sequence = sequence.saturating_add(1);
                        let at_unix_ms = unix_time_ms();
                        media.observe(&event, at_unix_ms);
                        if lease.publish(event.clone()).is_err()
                            || shared
                                .registry
                                .update_media_sample(
                                    stream_id,
                                    publisher_session_id,
                                    sequence,
                                    media.snapshot(0),
                                    at_unix_ms,
                                )
                                .is_err()
                        {
                            failed = Some(RtmpRelayFailure::Source);
                            break;
                        }
                        let mut state = shared.lock();
                        state.events_received = state.events_received.saturating_add(1);
                        state.payload_bytes_received = state
                            .payload_bytes_received
                            .saturating_add(event.payload_len() as u64);
                    }
                    ClientSessionResult::UnhandleableMessageReceived(_) => {}
                }
            }
            if stream.flush().is_err() {
                failed = Some(RtmpRelayFailure::Transport);
            }
        }
        let at_unix_ms = unix_time_ms();
        let _ = registration.release(at_unix_ms);
        drop(lease);
        if shared.is_accepting()
            && let Some(failure) = failed
        {
            shared.record_failure(failure);
            if !shared.wait_backoff() {
                break;
            }
        }
    }
    shared.set_phase(RtmpRelayPhase::Stopped);
}

fn establish_pull_session(
    stream: &mut RtmpStream,
    target: &RtmpPullTarget,
    timeout: Duration,
) -> Result<ClientSession, RtmpRelayFailure> {
    let mut config = ClientSessionConfig::new();
    config
        .flash_version
        .clone_from(&target.options.flash_version);
    config.playback_buffer_length_ms = target.options.playback_buffer_ms;
    config.tc_url = Some(target.options.tc_url.clone().unwrap_or_else(|| {
        format!(
            "{}://{}/{}",
            match target.transport {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
            target.host,
            target.source_application
        )
    }));
    apply_credentials(&mut config, &target.options)?;
    let (mut session, initial) =
        ClientSession::new(config).map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, initial)?;
    let connect = session
        .request_connection(target.source_application.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![connect], timeout, |event| {
        matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
    })?;
    let play = session
        .request_playback(target.source_stream_name.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![play], timeout, |event| {
        matches!(event, ClientSessionEvent::PlaybackRequestAccepted)
    })?;
    Ok(session)
}

fn pull_media_event(event: ClientSessionEvent) -> Option<MediaEvent> {
    match event {
        ClientSessionEvent::StreamMetadataReceived { metadata } => {
            MediaEvent::metadata(metadata).ok()
        }
        ClientSessionEvent::AudioDataReceived { timestamp, data } => {
            MediaEvent::audio(timestamp.value, Arc::<[u8]>::from(data.as_ref())).ok()
        }
        ClientSessionEvent::VideoDataReceived { timestamp, data } => {
            MediaEvent::video(timestamp.value, Arc::<[u8]>::from(data.as_ref())).ok()
        }
        ClientSessionEvent::ConnectionRequestAccepted
        | ClientSessionEvent::ConnectionRequestRejected { .. }
        | ClientSessionEvent::PlaybackRequestAccepted
        | ClientSessionEvent::PublishRequestAccepted
        | ClientSessionEvent::UnhandleableAmf0Command { .. }
        | ClientSessionEvent::UnknownTransactionResultReceived { .. }
        | ClientSessionEvent::UnhandleableOnStatusCode { .. }
        | ClientSessionEvent::AcknowledgementReceived { .. }
        | ClientSessionEvent::PingResponseReceived { .. } => None,
    }
}

impl RelayShared {
    fn lock(&self) -> MutexGuard<'_, RelayState> {
        self.state.lock().expect("RTMP relay state mutex poisoned")
    }

    fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    fn set_phase(&self, phase: RtmpRelayPhase) {
        self.lock().phase = phase;
    }

    fn record_failure(&self, failure: RtmpRelayFailure) {
        let mut state = self.lock();
        state.last_failure = Some(failure);
        state.phase = RtmpRelayPhase::Backoff;
    }

    fn refresh_address(&self) -> SocketAddr {
        let Some(resolver) = &self.resolver else {
            return self.destination.address;
        };
        match resolver.refresh_if_due() {
            RtmpDnsRefresh::NotDue => {}
            RtmpDnsRefresh::Refreshed(_) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_successes = state.dns_refresh_successes.saturating_add(1);
                state.last_dns_refresh_failure = None;
            }
            RtmpDnsRefresh::Failed(failure) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_failures = state.dns_refresh_failures.saturating_add(1);
                state.last_dns_refresh_failure = Some(failure);
            }
        }
        resolver.address()
    }

    fn wait_backoff(&self) -> bool {
        let state = self.lock();
        let (state, _) = self
            .available
            .wait_timeout_while(state, self.config.reconnect_interval, |state| {
                state.accepting
            })
            .expect("RTMP relay state mutex poisoned during backoff");
        state.accepting
    }

    fn next_event(&self) -> Option<MediaEvent> {
        let state = self.lock();
        let mut state = self
            .available
            .wait_timeout_while(state, RELAY_QUEUE_POLL, |state| {
                state.accepting && state.queue.is_empty()
            })
            .expect("RTMP relay state mutex poisoned while waiting")
            .0;
        if state
            .queue_started_at
            .is_some_and(|started| started.elapsed() >= self.config.buffer_duration)
        {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = !state.cache.video_headers.is_empty();
            return None;
        }
        let event = state.queue.pop_front()?;
        state.queue_bytes -= event.payload_len();
        if state.queue.is_empty() {
            state.queue_started_at = None;
        }
        Some(event)
    }

    fn take_bootstrap(&self) -> Vec<MediaEvent> {
        let mut state = self.lock();
        let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
        state.events_dropped = state.events_dropped.saturating_add(discarded);
        state.queue.clear();
        state.queue_bytes = 0;
        state.queue_started_at = None;
        state.cache.bootstrap()
    }

    fn record_sent(&self, event: &MediaEvent) {
        let mut state = self.lock();
        state.events_sent = state.events_sent.saturating_add(1);
        state.payload_bytes_sent = state
            .payload_bytes_sent
            .saturating_add(event.payload_len() as u64);
    }
}

impl RelayCache {
    fn update(&mut self, event: &MediaEvent) -> bool {
        let mut codec_header_changed = false;
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event.clone()),
            MediaEventKind::AacSequenceHeader => self.aac_header = Some(event.clone()),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                if let Some(codec) = event.video_codec() {
                    self.keyframe = None;
                    self.video_headers.insert(codec, event.clone());
                    codec_header_changed = true;
                }
            }
            MediaEventKind::VideoKeyframe => self.keyframe = Some(event.clone()),
            MediaEventKind::Audio => self.latest_audio = Some(event.clone()),
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {}
        }
        codec_header_changed
    }

    fn bootstrap(&self) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(4);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_header.iter().cloned());
        if let Some(keyframe) = &self.keyframe {
            events.extend(
                keyframe
                    .video_codec()
                    .and_then(|codec| self.video_headers.get(&codec))
                    .cloned(),
            );
            events.push(keyframe.clone());
        } else {
            events.extend(self.latest_audio.iter().cloned());
        }
        events
    }
}

struct RelayExecutor {
    sender: SyncSender<Arc<RelayShared>>,
}

impl RelayExecutor {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Arc<RelayShared>>(MAX_QUEUED_RTMP_RELAYS);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..RTMP_RELAY_WORKER_THREADS {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("rtmp-relay-worker-{index}"))
                .spawn(move || {
                    loop {
                        let task = receiver
                            .lock()
                            .expect("RTMP relay executor mutex poisoned")
                            .recv();
                        let Ok(shared) = task else {
                            return;
                        };
                        run_relay(&shared);
                    }
                })
                .expect("shared RTMP relay worker must start");
        }
        Self { sender }
    }

    fn admit(&self, shared: Arc<RelayShared>) -> Result<(), ()> {
        self.sender.try_send(shared).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
        })
    }
}

fn relay_executor() -> &'static RelayExecutor {
    static EXECUTOR: OnceLock<RelayExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(RelayExecutor::new)
}

fn fits_queue(events: &[MediaEvent], config: &RtmpRelayConfig) -> bool {
    events.len() <= config.max_queue_messages
        && events
            .iter()
            .try_fold(0_usize, |total, event| {
                total.checked_add(event.payload_len())
            })
            .is_some_and(|bytes| bytes <= config.max_queue_bytes)
}

fn run_relay(shared: &RelayShared) {
    while shared.is_accepting() {
        {
            let mut state = shared.lock();
            state.phase = RtmpRelayPhase::Connecting;
            state.connection_attempts = state.connection_attempts.saturating_add(1);
        }
        let address = shared.refresh_address();
        let Ok(mut stream) = client::connect_stream(
            &shared.destination.host,
            address,
            shared.destination.transport,
            shared.config.connect_timeout,
            shared.config.handshake_timeout,
        ) else {
            shared.record_failure(RtmpRelayFailure::Connect);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        };
        if establish_transport(&mut stream, shared.config.handshake_timeout).is_err() {
            shared.record_failure(RtmpRelayFailure::Handshake);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        }
        let mut session = match establish_publish_session(
            &mut stream,
            &shared.destination,
            shared.config.handshake_timeout,
        ) {
            Ok(session) => session,
            Err(failure) => {
                shared.record_failure(failure);
                if !shared.wait_backoff() {
                    break;
                }
                continue;
            }
        };
        {
            let mut state = shared.lock();
            state.connections = state.connections.saturating_add(1);
            state.reconnects = state.connections.saturating_sub(1);
            state.phase = RtmpRelayPhase::Publishing;
            state.last_failure = None;
        }

        let mut failed = None;
        for event in shared.take_bootstrap() {
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        while failed.is_none() && shared.is_accepting() {
            if let Err(failure) = process_peer_input(&mut stream, &mut session) {
                failed = Some(failure);
                break;
            }
            let Some(event) = shared.next_event() else {
                continue;
            };
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        if let Some(failure) = failed {
            shared.record_failure(failure);
            if !shared.wait_backoff() {
                break;
            }
        }
    }
    shared.set_phase(RtmpRelayPhase::Stopped);
}

fn establish_transport(stream: &mut RtmpStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;
    let mut handshake = Handshake::new(PeerType::Client);
    let hello = handshake
        .generate_outbound_p0_and_p1()
        .map_err(io::Error::other)?;
    stream.write_all(&hello)?;
    let mut response = [0; HANDSHAKE_RESPONSE_BYTES];
    stream.read_exact(&mut response)?;
    let finish = match handshake
        .process_bytes(&response)
        .map_err(io::Error::other)?
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RTMP server handshake remained incomplete",
            ));
        }
    };
    stream.write_all(&finish)?;
    stream.flush()
}

fn establish_publish_session(
    stream: &mut RtmpStream,
    destination: &RtmpDestination,
    timeout: Duration,
) -> Result<ClientSession, RtmpRelayFailure> {
    let mut config = ClientSessionConfig::new();
    config.chunk_size = 4_096;
    config.playback_buffer_length_ms = destination.options.playback_buffer_ms;
    config
        .flash_version
        .clone_from(&destination.options.flash_version);
    config.tc_url = Some(destination.options.tc_url.clone().unwrap_or_else(|| {
        format!(
            "{}://{}/{}",
            match destination.transport {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
            destination.host,
            destination.application
        )
    }));
    apply_credentials(&mut config, &destination.options)?;
    let (mut session, initial) =
        ClientSession::new(config).map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, initial)?;
    let connect = session
        .request_connection(destination.application.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![connect], timeout, |event| {
        matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
    })?;
    let publish = session
        .request_publishing(destination.stream_name.clone(), PublishRequestType::Live)
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![publish], timeout, |event| {
        matches!(event, ClientSessionEvent::PublishRequestAccepted)
    })?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .map_err(|_| RtmpRelayFailure::Transport)?;
    Ok(session)
}

fn apply_credentials(
    config: &mut ClientSessionConfig,
    options: &RtmpClientOptions,
) -> Result<(), RtmpRelayFailure> {
    let Some(credential) = &options.credential else {
        return Ok(());
    };
    config.username = Some(credential.username().to_owned());
    config.password = Some(
        String::from_utf8(credential.secret().to_vec()).map_err(|_| RtmpRelayFailure::Session)?,
    );
    Ok(())
}

fn await_event(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
    initial: Vec<ClientSessionResult>,
    timeout: Duration,
    predicate: impl Fn(&ClientSessionEvent) -> bool,
) -> Result<(), RtmpRelayFailure> {
    write_results(stream, initial)?;
    let deadline = Instant::now() + timeout;
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    while Instant::now() < deadline {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| RtmpRelayFailure::Transport)?;
        if count == 0 {
            return Err(RtmpRelayFailure::Transport);
        }
        let results = session
            .handle_input(&buffer[..count])
            .map_err(|_| RtmpRelayFailure::Session)?;
        let mut accepted = false;
        let mut outbound = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => outbound.push(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => accepted |= predicate(&event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        write_packets(stream, outbound)?;
        if accepted {
            return Ok(());
        }
    }
    Err(RtmpRelayFailure::Session)
}

fn process_peer_input(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
) -> Result<(), RtmpRelayFailure> {
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    match stream.read(&mut buffer) {
        Ok(0) => Err(RtmpRelayFailure::Transport),
        Ok(count) => {
            let results = session
                .handle_input(&buffer[..count])
                .map_err(|_| RtmpRelayFailure::Session)?;
            write_results(stream, results)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(RtmpRelayFailure::Transport),
    }
}

fn publish_event(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
    event: &MediaEvent,
) -> Result<(), RtmpRelayFailure> {
    let result = match event.kind() {
        MediaEventKind::Metadata => session.publish_metadata(
            event
                .stream_metadata()
                .expect("metadata events retain decoded metadata"),
        ),
        MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => session.publish_audio_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            false,
        ),
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => session.publish_video_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            event.kind() == MediaEventKind::VideoDisposable,
        ),
    }
    .map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, vec![result])
}

fn write_results(
    stream: &mut RtmpStream,
    results: Vec<ClientSessionResult>,
) -> Result<(), RtmpRelayFailure> {
    write_packets(
        stream,
        results.into_iter().filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        }),
    )
}

fn write_packets(
    stream: &mut RtmpStream,
    packets: impl IntoIterator<Item = Vec<u8>>,
) -> Result<(), RtmpRelayFailure> {
    for packet in packets {
        stream
            .write_all(&packet)
            .map_err(|_| RtmpRelayFailure::Transport)?;
    }
    stream.flush().map_err(|_| RtmpRelayFailure::Transport)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct SequenceResolver {
        answers: Mutex<VecDeque<Result<Vec<SocketAddr>, io::ErrorKind>>>,
        calls: AtomicUsize,
    }

    impl SequenceResolver {
        fn new(answers: impl IntoIterator<Item = Result<Vec<SocketAddr>, io::ErrorKind>>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl RtmpDnsResolver for SequenceResolver {
        fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            match self
                .answers
                .lock()
                .expect("test resolver mutex poisoned")
                .pop_front()
                .unwrap_or(Err(io::ErrorKind::NotFound))
            {
                Ok(addresses) => Ok(addresses),
                Err(kind) => Err(io::Error::from(kind)),
            }
        }
    }

    fn test_resolver(
        initial: SocketAddr,
        policy: RtmpOutboundPolicy,
        listeners: impl IntoIterator<Item = SocketAddr>,
        answers: impl IntoIterator<Item = Result<Vec<SocketAddr>, io::ErrorKind>>,
    ) -> (RtmpDestinationResolver, Arc<SequenceResolver>) {
        let resolver = Arc::new(SequenceResolver::new(answers));
        let destination = RtmpDestinationResolver::from_startup_with_resolver(
            "relay.example",
            initial.port(),
            RtmpTransport::Rtmp,
            [initial],
            policy,
            listeners,
            Duration::from_millis(5),
            Arc::clone(&resolver) as Arc<dyn RtmpDnsResolver>,
        )
        .expect("startup destination");
        (destination, resolver)
    }

    #[test]
    fn dns_refresh_rotates_an_address_once_per_bounded_interval() {
        let initial = "127.0.0.1:1935".parse().expect("initial address");
        let rotated = "127.0.0.2:1935".parse().expect("rotated address");
        let (resolver, queries) = test_resolver(
            initial,
            RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
            [],
            [Ok(vec![rotated])],
        );

        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Refreshed(rotated)
        );
        assert_eq!(resolver.address(), rotated);
        assert_eq!(resolver.refresh_if_due(), RtmpDnsRefresh::NotDue);
        assert_eq!(queries.calls(), 1);
    }

    #[test]
    fn failed_dns_refresh_retains_the_last_address_and_retries_later() {
        let initial = "127.0.0.1:1935".parse().expect("initial address");
        let rotated = "127.0.0.2:1935".parse().expect("rotated address");
        let (resolver, queries) = test_resolver(
            initial,
            RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
            [],
            [Err(io::ErrorKind::NotFound), Ok(vec![rotated])],
        );

        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::Resolution)
        );
        assert_eq!(resolver.address(), initial);
        assert_eq!(resolver.refresh_if_due(), RtmpDnsRefresh::NotDue);
        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Refreshed(rotated)
        );
        assert_eq!(queries.calls(), 2);
    }

    #[test]
    fn dns_refresh_rejects_a_family_mismatch_without_switching_addresses() {
        let initial = "127.0.0.1:1935".parse().expect("initial address");
        let (resolver, _) = test_resolver(
            initial,
            RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
            [],
            [Ok(vec!["[::1]:1935".parse().expect("IPv6 address")])],
        );

        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::FamilyMismatch)
        );
        assert_eq!(resolver.address(), initial);
    }

    #[test]
    fn dns_refresh_rejects_a_direct_listener_loop_before_family_selection() {
        let initial = "127.0.0.2:1935".parse().expect("initial address");
        let listener = "127.0.0.1:1935".parse().expect("listener address");
        let (resolver, _) = test_resolver(
            initial,
            RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
            [listener],
            [Ok(vec![listener])],
        );

        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::DirectLoop)
        );
        assert_eq!(resolver.address(), initial);
    }

    #[test]
    fn dns_refresh_rechecks_the_outbound_policy_for_new_answers() {
        let initial = "198.51.100.10:1935".parse().expect("initial address");
        let (resolver, _) = test_resolver(
            initial,
            RtmpOutboundPolicy::default(),
            [],
            [Ok(vec![
                "127.0.0.1:1935".parse().expect("loopback address"),
            ])],
        );

        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            resolver.refresh_if_due(),
            RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::Policy)
        );
        assert_eq!(resolver.address(), initial);
    }

    #[test]
    fn overflow_and_codec_headers_gate_relay_output_until_a_matching_keyframe() {
        let controller = RtmpRelayController {
            shared: Arc::new(RelayShared {
                destination: RtmpDestination {
                    address: "127.0.0.1:1935".parse().expect("destination"),
                    host: "127.0.0.1".into(),
                    transport: RtmpTransport::Rtmp,
                    application: "live".into(),
                    stream_name: "camera".into(),
                    options: RtmpClientOptions::default(),
                },
                config: RtmpRelayConfig {
                    max_queue_messages: 4,
                    max_queue_bytes: 128,
                    buffer_duration: Duration::from_secs(5),
                    max_chain_depth: 4,
                    ..RtmpRelayConfig::default()
                },
                resolver: None,
                state: Mutex::new(RelayState {
                    accepting: true,
                    waiting_for_keyframe: false,
                    queue: VecDeque::new(),
                    queue_bytes: 0,
                    queue_started_at: None,
                    cache: RelayCache::default(),
                    phase: RtmpRelayPhase::Publishing,
                    last_failure: None,
                    connection_attempts: 0,
                    connections: 0,
                    reconnects: 0,
                    events_enqueued: 0,
                    events_sent: 0,
                    events_dropped: 0,
                    payload_bytes_sent: 0,
                    dns_refresh_attempts: 0,
                    dns_refresh_successes: 0,
                    dns_refresh_failures: 0,
                    last_dns_refresh_failure: None,
                }),
                available: Condvar::new(),
            }),
        };

        controller.try_enqueue(video(0, &[0x17, 0x00, 0, 0, 0, 0x01]));
        controller.try_enqueue(video(1, &[0x17, 0x01, 0, 0, 0, 0x11]));
        for timestamp in 2..=5 {
            controller.try_enqueue(video(timestamp, &[0x27, 0x01, 0, 0, 0, 0x22]));
        }
        controller.try_enqueue(
            MediaEvent::audio(6, Arc::<[u8]>::from([0xaf, 0x01, 0x33])).expect("audio"),
        );
        controller.try_enqueue(video(7, &[0x27, 0x01, 0, 0, 0, 0x44]));
        {
            let state = controller.shared.lock();
            assert!(state.waiting_for_keyframe);
            assert!(state.queue.is_empty());
        }

        controller.try_enqueue(video(8, &[0x17, 0x00, 0, 0, 0, 0x02]));
        controller.try_enqueue(video(9, &[0x17, 0x01, 0, 0, 0, 0x55]));

        let state = controller.shared.lock();
        assert!(!state.waiting_for_keyframe);
        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.queue[0].payload(), [0x17, 0x00, 0, 0, 0, 0x02]);
        assert_eq!(state.queue[1].payload(), [0x17, 0x01, 0, 0, 0, 0x55]);
    }

    #[test]
    fn relay_queue_expires_after_the_configured_buffer_window() {
        let controller = RtmpRelayController {
            shared: Arc::new(RelayShared {
                destination: RtmpDestination {
                    address: "127.0.0.1:1935".parse().expect("destination"),
                    host: "127.0.0.1".into(),
                    transport: RtmpTransport::Rtmp,
                    application: "live".into(),
                    stream_name: "camera".into(),
                    options: RtmpClientOptions::default(),
                },
                config: RtmpRelayConfig {
                    buffer_duration: Duration::from_millis(1),
                    ..RtmpRelayConfig::default()
                },
                resolver: None,
                state: Mutex::new(RelayState {
                    accepting: true,
                    waiting_for_keyframe: false,
                    queue: VecDeque::new(),
                    queue_bytes: 0,
                    queue_started_at: None,
                    cache: RelayCache::default(),
                    phase: RtmpRelayPhase::Publishing,
                    last_failure: None,
                    connection_attempts: 0,
                    connections: 0,
                    reconnects: 0,
                    events_enqueued: 0,
                    events_sent: 0,
                    events_dropped: 0,
                    payload_bytes_sent: 0,
                    dns_refresh_attempts: 0,
                    dns_refresh_successes: 0,
                    dns_refresh_failures: 0,
                    last_dns_refresh_failure: None,
                }),
                available: Condvar::new(),
            }),
        };
        controller.try_enqueue(video(0, &[0x17, 0x01, 0, 0, 0, 0x01]));
        thread::sleep(Duration::from_millis(5));
        assert!(controller.shared.next_event().is_none());
        let state = controller.shared.lock();
        assert_eq!(state.queue_bytes, 0);
        assert!(state.events_dropped > 0);
    }

    fn video(timestamp: u32, payload: &[u8]) -> MediaEvent {
        MediaEvent::video(timestamp, Arc::<[u8]>::from(payload)).expect("video event")
    }
}
