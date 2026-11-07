use std::{
    collections::HashMap,
    future::pending,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use log::{debug, error, warn};
use oxiroute_config::{ProxyProtocolPolicy, UdpPolicy};
use tokio::{
    net::UdpSocket,
    runtime::Builder,
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, Sleep, sleep, timeout},
};

use crate::shutdown::wait_for_shutdown;
use crate::{
    EndpointLease, HealthFailure, L4ServicePlan, ListenerMetrics, ListenerReservation,
    ListenerRuntime, MAX_V1_HEADER_BYTES, ProxyProtocolError, ProxyProtocolErrorKind,
    ProxyProtocolResult, ProxyProtocolTransport, RuntimeGeneration, RuntimeReferenceKind,
    TrafficLease, encode_header, parse_header,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_UDP_WIRE_DATAGRAM_BYTES: usize = 65_507;
const MAX_V2_IPV4_ADDRESS_HEADER_BYTES: usize = 28;
const MAX_V2_IPV6_ADDRESS_HEADER_BYTES: usize = 52;

/// A generation-owned UDP listener and its bounded pseudo-session runtime.
pub struct UdpRuntime {
    accounting: Arc<UdpAccounting>,
    thread: Option<JoinHandle<()>>,
}

/// Bounded listener-local UDP admission and terminal counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UdpRelayStats {
    pub datagrams_received: u64,
    pub datagrams_dropped: u64,
    pub sessions_started: u64,
    pub sessions_completed: u64,
    pub sessions_failed: u64,
}

#[derive(Default)]
struct UdpAccounting {
    datagrams_received: AtomicU64,
    datagrams_dropped: AtomicU64,
    sessions_started: AtomicU64,
    sessions_completed: AtomicU64,
    sessions_failed: AtomicU64,
}

impl UdpAccounting {
    fn increment(counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        });
    }

    fn snapshot(&self) -> UdpRelayStats {
        UdpRelayStats {
            datagrams_received: self.datagrams_received.load(Ordering::Relaxed),
            datagrams_dropped: self.datagrams_dropped.load(Ordering::Relaxed),
            sessions_started: self.sessions_started.load(Ordering::Relaxed),
            sessions_completed: self.sessions_completed.load(Ordering::Relaxed),
            sessions_failed: self.sessions_failed.load(Ordering::Relaxed),
        }
    }
}

impl UdpRuntime {
    /// Starts one UDP listener on a previously reserved socket.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be duplicated, the runtime cannot be created, or
    /// the listener does not reach its startup boundary.
    #[cfg(unix)]
    pub fn start(
        listener_name: String,
        reservation: ListenerReservation,
        service: Arc<L4ServicePlan>,
        generation: Arc<RuntimeGeneration>,
        metrics: ListenerMetrics,
        proxy_protocol: Option<ProxyProtocolPolicy>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let accounting = Arc::new(UdpAccounting::default());
        let accounting_for_thread = Arc::clone(&accounting);
        let thread = thread::Builder::new()
            .name(format!("oxiroute-udp-{listener_name}"))
            .spawn(move || {
                let result = run(
                    &listener_name,
                    reservation,
                    service,
                    generation.clone(),
                    metrics,
                    proxy_protocol,
                    accounting_for_thread,
                    shutdown,
                    ready_tx,
                );
                if let Err(error) = result {
                    generation.mark_runtime_failed();
                    error!("UDP listener `{listener_name}` failed: {error}");
                }
            })?;
        match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                accounting,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(io::Error::other(error).into())
            }
            Err(error) => {
                let _ = thread.join();
                Err(io::Error::new(
                    if error == std::sync::mpsc::RecvTimeoutError::Timeout {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    },
                    format!("UDP listener did not become ready: {error}"),
                )
                .into())
            }
        }
    }

    /// Returns the unsupported-transport error on platforms without Unix descriptor ownership.
    #[cfg(not(unix))]
    pub fn start(
        _listener_name: String,
        _reservation: ListenerReservation,
        _service: Arc<L4ServicePlan>,
        _generation: Arc<RuntimeGeneration>,
        _metrics: ListenerMetrics,
        _proxy_protocol: Option<ProxyProtocolPolicy>,
        _shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "UDP runtime requires Unix socket descriptor ownership",
        )
        .into())
    }

    /// Waits for the listener and all active pseudo-sessions to stop.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener thread panicked.
    ///
    /// # Panics
    ///
    /// Panics if the listener thread handle was already consumed, which violates the runtime
    /// ownership invariant.
    pub fn join(mut self) -> io::Result<()> {
        self.thread
            .take()
            .expect("UDP listener thread exists")
            .join()
            .map_err(|_| io::Error::other("UDP listener thread panicked"))
    }

    #[must_use]
    pub fn stats(&self) -> UdpRelayStats {
        self.accounting.snapshot()
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run(
    listener_name: &str,
    reservation: ListenerReservation,
    service: Arc<L4ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    proxy_protocol: Option<ProxyProtocolPolicy>,
    accounting: Arc<UdpAccounting>,
    shutdown: watch::Receiver<bool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let result: io::Result<()> = runtime.block_on(async move {
        let socket = match reservation.duplicate_udp_socket() {
            Ok(socket) => socket,
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
                return Err(error);
            }
        };
        let socket = match UdpSocket::from_std(socket) {
            Ok(socket) => socket,
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
                return Err(error);
            }
        };
        metrics.mark_listening();
        if ready.send(Ok(())).is_err() {
            return Err(io::Error::other("UDP startup receiver was dropped"));
        }
        match serve(
            listener_name,
            socket,
            service,
            generation,
            metrics,
            proxy_protocol,
            accounting,
            shutdown,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
                Err(error)
            }
        }
    });
    result.map_err(Into::into)
}

#[derive(Clone)]
struct DatagramQueue {
    sender: mpsc::Sender<QueuedDatagram>,
    queued_bytes: Arc<AtomicU64>,
    max_bytes: u64,
}

struct QueuedDatagram {
    payload: Vec<u8>,
    queued_bytes: Arc<AtomicU64>,
}

impl Drop for QueuedDatagram {
    fn drop(&mut self) {
        let bytes = u64::try_from(self.payload.len()).unwrap_or(u64::MAX);
        self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}

enum QueueSendResult {
    Enqueued,
    Full,
    Closed,
}

impl DatagramQueue {
    fn new(policy: UdpPolicy) -> io::Result<(Self, mpsc::Receiver<QueuedDatagram>)> {
        let capacity = usize::try_from(policy.max_queue_datagrams).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP queue datagram limit is too large",
            )
        })?;
        let (sender, receiver) = mpsc::channel(capacity);
        Ok((
            Self {
                sender,
                queued_bytes: Arc::new(AtomicU64::new(0)),
                max_bytes: policy.max_queue_bytes,
            },
            receiver,
        ))
    }

    fn try_send(&self, payload: Vec<u8>) -> QueueSendResult {
        let bytes = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let reserved = self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .is_ok();
        if !reserved {
            return QueueSendResult::Full;
        }
        match self.sender.try_send(QueuedDatagram {
            payload,
            queued_bytes: Arc::clone(&self.queued_bytes),
        }) {
            Ok(()) => QueueSendResult::Enqueued,
            Err(mpsc::error::TrySendError::Full(datagram)) => {
                drop(datagram);
                QueueSendResult::Full
            }
            Err(mpsc::error::TrySendError::Closed(datagram)) => {
                drop(datagram);
                QueueSendResult::Closed
            }
        }
    }
}

struct SessionEntry {
    id: u64,
    queue: DatagramQueue,
}

type SessionTable = Arc<Mutex<HashMap<std::net::SocketAddr, SessionEntry>>>;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn serve(
    listener_name: &str,
    socket: UdpSocket,
    service: Arc<L4ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    proxy_protocol: Option<ProxyProtocolPolicy>,
    accounting: Arc<UdpAccounting>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let policy = service.udp_policy();
    let max_datagram_bytes = usize::try_from(policy.max_datagram_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP datagram limit is too large",
        )
    })?;
    let max_sessions = usize::try_from(policy.max_sessions).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP session limit is too large",
        )
    })?;
    let listener_is_ipv4 = socket.local_addr()?.is_ipv4();
    let listener_proxy_header_bytes =
        proxy_protocol.map_or(0, |policy| proxy_header_budget(policy, listener_is_ipv4));
    let upstream_proxy_header_bytes = service
        .proxy_protocol()
        .map_or(0, |policy| proxy_header_budget(policy, listener_is_ipv4));
    let proxy_header_bytes = listener_proxy_header_bytes.max(upstream_proxy_header_bytes);
    if max_datagram_bytes > MAX_UDP_WIRE_DATAGRAM_BYTES.saturating_sub(proxy_header_bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP datagram limit leaves no room for the configured PROXY protocol header",
        ));
    }
    let max_received_bytes = max_datagram_bytes
        .checked_add(listener_proxy_header_bytes)
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "UDP receive limit overflowed")
        })?;
    let socket = Arc::new(socket);
    let table: SessionTable = Arc::new(Mutex::new(HashMap::with_capacity(max_sessions)));
    let listener = ListenerRuntime::new(metrics.clone());
    let next_id = AtomicU64::new(0);
    let mut sessions = JoinSet::new();
    let mut receive_buffer = vec![0_u8; max_received_bytes];

    loop {
        while let Some(result) = sessions.try_join_next() {
            if let Err(error) = result {
                UdpAccounting::increment(&accounting.sessions_failed);
                warn!("UDP listener `{listener_name}` pseudo-session task failed: {error}");
            }
        }
        tokio::select! {
            biased;
            () = wait_for_shutdown(&mut shutdown) => break,
            received = socket.recv_from(&mut receive_buffer) => {
                let (length, client) = received?;
                UdpAccounting::increment(&accounting.datagrams_received);
                if proxy_protocol.is_none() && length > max_datagram_bytes {
                    UdpAccounting::increment(&accounting.datagrams_dropped);
                    debug!("UDP listener `{listener_name}` dropped an oversized datagram");
                    continue;
                }
                if length >= max_received_bytes {
                    UdpAccounting::increment(&accounting.datagrams_dropped);
                    debug!("UDP listener `{listener_name}` dropped an oversized datagram");
                    continue;
                }
                let payload = receive_buffer[..length].to_vec();
                let mut table_guard = table
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(entry) = table_guard.get(&client) {
                    if length > max_datagram_bytes {
                        UdpAccounting::increment(&accounting.datagrams_dropped);
                        debug!("UDP listener `{listener_name}` dropped an oversized session datagram");
                        continue;
                    }
                    match entry.queue.try_send(payload) {
                        QueueSendResult::Enqueued => {}
                        QueueSendResult::Full => {
                            UdpAccounting::increment(&accounting.datagrams_dropped);
                            debug!("UDP listener `{listener_name}` dropped a queued datagram at its session limit");
                        }
                        QueueSendResult::Closed => {
                            UdpAccounting::increment(&accounting.datagrams_dropped);
                            let id = entry.id;
                            if table_guard
                                .get(&client)
                                .is_some_and(|entry| entry.id == id)
                            {
                                table_guard.remove(&client);
                            }
                        }
                    }
                    continue;
                }
                if table_guard.len() >= max_sessions {
                    UdpAccounting::increment(&accounting.datagrams_dropped);
                    debug!("UDP listener `{listener_name}` rejected a new pseudo-session at its table limit");
                    continue;
                }
                let (payload, logical_client) = match proxy_protocol {
                    Some(policy) => match parse_initial_datagram(
                        &payload,
                        policy,
                        max_datagram_bytes,
                    ) {
                        Ok(parsed) => {
                            if let Err(error) = metrics.record_proxy_protocol(
                                ProxyProtocolResult::Accepted,
                            ) {
                                debug!("could not account for UDP PROXY protocol: {error}");
                            }
                            parsed
                        }
                        Err(error) => {
                            UdpAccounting::increment(&accounting.datagrams_dropped);
                            if let Err(metric_error) = metrics.record_proxy_protocol(error.result()) {
                                debug!("could not account for UDP PROXY protocol rejection: {metric_error}");
                            }
                            debug!("UDP listener `{listener_name}` rejected PROXY protocol: {error}");
                            continue;
                        }
                    },
                    None => (payload, Some(client)),
                };
                let traffic_lease =
                    match listener.admit_runtime_connection(&generation, RuntimeReferenceKind::Udp) {
                    Ok(lease) => lease,
                    Err(error) => {
                        UdpAccounting::increment(&accounting.datagrams_dropped);
                        debug!("UDP listener `{listener_name}` rejected a pseudo-session: {error}");
                        continue;
                    }
                };
                let (queue, queue_receiver) = DatagramQueue::new(policy)?;
                let id = next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
                let entry = SessionEntry {
                    id,
                    queue: queue.clone(),
                };
                table_guard.insert(client, entry);
                UdpAccounting::increment(&accounting.sessions_started);
                drop(table_guard);
                let table_for_task = Arc::clone(&table);
                let socket_for_task = Arc::clone(&socket);
                let service_for_task = Arc::clone(&service);
                let generation_for_task = Arc::clone(&generation);
                let accounting_for_task = Arc::clone(&accounting);
                let listener_name = listener_name.to_owned();
                let shutdown_for_task = shutdown.clone();
                sessions.spawn(async move {
                    let result = run_session(
                        socket_for_task,
                        client,
                        payload,
                        queue_receiver,
                        service_for_task,
                        generation_for_task,
                        traffic_lease,
                        listener_is_ipv4,
                        logical_client,
                        shutdown_for_task,
                    )
                    .await;
                    let mut table = table_for_task
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if table.get(&client).is_some_and(|entry| entry.id == id) {
                        table.remove(&client);
                    }
                    match result {
                        Ok(()) => {
                            UdpAccounting::increment(&accounting_for_task.sessions_completed);
                            debug!("UDP listener `{listener_name}` pseudo-session ended: completed");
                        }
                        Err(outcome) => {
                            UdpAccounting::increment(&accounting_for_task.sessions_failed);
                            debug!("UDP listener `{listener_name}` pseudo-session ended: {outcome}");
                        }
                    }
                });
            }
        }
    }

    table
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    while let Some(result) = sessions.join_next().await {
        if let Err(error) = result {
            UdpAccounting::increment(&accounting.sessions_failed);
            warn!("UDP listener `{listener_name}` pseudo-session task failed: {error}");
        }
    }
    Ok(())
}

fn parse_initial_datagram(
    input: &[u8],
    policy: ProxyProtocolPolicy,
    max_datagram_bytes: usize,
) -> Result<(Vec<u8>, Option<std::net::SocketAddr>), ProxyProtocolError> {
    let header = parse_header(input, policy.version, ProxyProtocolTransport::Datagram)?
        .ok_or_else(|| ProxyProtocolError::new(ProxyProtocolErrorKind::UnexpectedEof))?;
    let payload = &input[header.consumed..];
    if payload.len() > max_datagram_bytes {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidLength,
        ));
    }
    Ok((payload.to_vec(), Some(header.source)))
}

#[derive(Debug)]
enum SessionEnd {
    Cancelled,
    IdleTimeout,
    LifetimeTimeout,
    Connect(io::Error),
    UpstreamConnect(io::Error),
    UpstreamSend(io::Error),
    UpstreamReceive(io::Error),
    UpstreamProtocol(io::Error),
    UpstreamProxyProtocol(ProxyProtocolError),
    ClientSend(io::Error),
    ProxyProtocol(ProxyProtocolError),
    Accounting,
    SessionBytesLimit,
}

impl std::fmt::Display for SessionEnd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("cancelled"),
            Self::IdleTimeout => formatter.write_str("idle_timeout"),
            Self::LifetimeTimeout => formatter.write_str("lifetime_timeout"),
            Self::Connect(error) => write!(formatter, "connect_error: {error}"),
            Self::UpstreamConnect(error) => write!(formatter, "upstream_connect_error: {error}"),
            Self::UpstreamSend(error) => write!(formatter, "upstream_send_error: {error}"),
            Self::UpstreamReceive(error) => write!(formatter, "upstream_receive_error: {error}"),
            Self::UpstreamProtocol(error) => write!(formatter, "upstream_protocol_error: {error}"),
            Self::UpstreamProxyProtocol(error) => {
                write!(formatter, "upstream_proxy_protocol: {error}")
            }
            Self::ClientSend(error) => write!(formatter, "client_send_error: {error}"),
            Self::ProxyProtocol(error) => write!(formatter, "proxy_protocol: {error}"),
            Self::Accounting => formatter.write_str("accounting_error"),
            Self::SessionBytesLimit => formatter.write_str("session_bytes_limit"),
        }
    }
}

impl SessionEnd {
    fn passive_failure(&self) -> Option<HealthFailure> {
        match self {
            Self::UpstreamConnect(error) => classify_upstream_io(error, true),
            Self::UpstreamSend(error) | Self::UpstreamReceive(error) => {
                classify_upstream_io(error, false)
            }
            Self::UpstreamProtocol(_) => Some(HealthFailure::ProtocolError),
            Self::UpstreamProxyProtocol(error) => match error.kind() {
                ProxyProtocolErrorKind::Cancelled => None,
                ProxyProtocolErrorKind::Timeout => Some(HealthFailure::Timeout),
                ProxyProtocolErrorKind::Io
                | ProxyProtocolErrorKind::UnexpectedEof
                | ProxyProtocolErrorKind::InvalidSignature
                | ProxyProtocolErrorKind::HeaderTooLarge
                | ProxyProtocolErrorKind::InvalidVersion
                | ProxyProtocolErrorKind::UnsupportedCommand
                | ProxyProtocolErrorKind::UnsupportedFamily
                | ProxyProtocolErrorKind::ProtocolMismatch
                | ProxyProtocolErrorKind::InvalidAddress
                | ProxyProtocolErrorKind::InvalidPort
                | ProxyProtocolErrorKind::InvalidLength => Some(HealthFailure::ProtocolError),
            },
            Self::Cancelled
            | Self::IdleTimeout
            | Self::LifetimeTimeout
            | Self::Connect(_)
            | Self::ClientSend(_)
            | Self::ProxyProtocol(_)
            | Self::Accounting
            | Self::SessionBytesLimit => None,
        }
    }
}

fn classify_upstream_io(error: &io::Error, connecting: bool) -> Option<HealthFailure> {
    match error.kind() {
        io::ErrorKind::Interrupted => None,
        io::ErrorKind::TimedOut => Some(HealthFailure::Timeout),
        _ if connecting => Some(HealthFailure::ConnectFailed),
        _ => Some(HealthFailure::ProtocolError),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_session(
    listener: Arc<UdpSocket>,
    client: std::net::SocketAddr,
    initial: Vec<u8>,
    queue_receiver: mpsc::Receiver<QueuedDatagram>,
    service: Arc<L4ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    traffic_lease: TrafficLease,
    listener_is_ipv4: bool,
    logical_client: Option<std::net::SocketAddr>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), SessionEnd> {
    let lease = tokio::select! {
        biased;
        () = wait_for_shutdown(&mut shutdown) => return Err(SessionEnd::Cancelled),
        lease = service.select_wait() => lease.ok_or_else(|| SessionEnd::Connect(io::Error::new(
            io::ErrorKind::NotFound,
            "UDP upstream pool has no selectable endpoint",
        )))?,
    };
    let result = relay_session(
        &lease,
        listener,
        client,
        initial,
        queue_receiver,
        &service,
        &traffic_lease,
        listener_is_ipv4,
        logical_client,
        &mut shutdown,
    )
    .await;
    if generation.accepting()
        && !*shutdown.borrow()
        && let Err(outcome) = &result
        && let Some(failure) = outcome.passive_failure()
    {
        lease.record_passive_failure(failure);
    }
    result
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn relay_session(
    lease: &EndpointLease,
    listener: Arc<UdpSocket>,
    client: std::net::SocketAddr,
    initial: Vec<u8>,
    mut queue_receiver: mpsc::Receiver<QueuedDatagram>,
    service: &L4ServicePlan,
    traffic_lease: &TrafficLease,
    listener_is_ipv4: bool,
    logical_client: Option<std::net::SocketAddr>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), SessionEnd> {
    let policy = service.udp_policy();
    let relay_policy = service.policy();
    let (upstream, destination) = tokio::select! {
        biased;
        () = wait_for_shutdown(shutdown) => return Err(SessionEnd::Cancelled),
        result = timeout(relay_policy.connect, connect_upstream(lease, listener_is_ipv4)) => {
            match result {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => return Err(SessionEnd::UpstreamConnect(error)),
                Err(_) => return Err(SessionEnd::UpstreamConnect(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "UDP upstream connect timed out",
                ))),
            }
        }
    };

    let mut session_bytes = 0_u64;
    account_received(traffic_lease, &mut session_bytes, initial.len(), policy)?;
    let outbound_proxy = service.proxy_protocol().zip(logical_client);
    if let Some((proxy_policy, source)) = outbound_proxy {
        let header = encode_header(
            proxy_policy.version,
            ProxyProtocolTransport::Datagram,
            source,
            destination,
        )
        .map_err(|error| {
            let _ = traffic_lease.record_proxy_protocol(error.result());
            SessionEnd::ProxyProtocol(error)
        })?;
        let mut datagram = Vec::with_capacity(header.len() + initial.len());
        datagram.extend_from_slice(&header);
        datagram.extend_from_slice(&initial);
        if datagram.len() > MAX_UDP_WIRE_DATAGRAM_BYTES {
            let error = ProxyProtocolError::new(ProxyProtocolErrorKind::InvalidLength);
            let _ = traffic_lease.record_proxy_protocol(error.result());
            return Err(SessionEnd::UpstreamProxyProtocol(error));
        }
        send_proxy_datagram(
            &upstream,
            &datagram,
            Duration::from_millis(proxy_policy.timeout_ms),
            shutdown,
        )
        .await
        .map_err(|error| {
            let _ = traffic_lease.record_proxy_protocol(error.result());
            SessionEnd::UpstreamProxyProtocol(error)
        })?;
        traffic_lease
            .record_proxy_protocol(ProxyProtocolResult::Sent)
            .map_err(|_| SessionEnd::Accounting)?;
    } else {
        send_datagram(&upstream, &initial, None, shutdown)
            .await
            .map_err(SessionEnd::UpstreamSend)?;
    }

    let mut idle = relay_policy.idle.map(|duration| Box::pin(sleep(duration)));
    let lifetime = wait_for_duration(relay_policy.lifetime);
    tokio::pin!(lifetime);
    let mut upstream_buffer =
        vec![0_u8; usize::try_from(policy.max_datagram_bytes).unwrap_or(65_507) + 1];

    loop {
        tokio::select! {
            biased;
            () = wait_for_shutdown(shutdown) => return Err(SessionEnd::Cancelled),
            () = wait_for_sleep(&mut idle) => return Err(SessionEnd::IdleTimeout),
            () = &mut lifetime => return Err(SessionEnd::LifetimeTimeout),
            queued = queue_receiver.recv() => {
                let Some(queued) = queued else { return Ok(()) };
                account_received(
                    traffic_lease,
                    &mut session_bytes,
                    queued.payload.len(),
                    policy,
                )?;
                send_datagram(&upstream, &queued.payload, None, shutdown)
                    .await
                    .map_err(SessionEnd::UpstreamSend)?;
                reset_sleep(&mut idle, relay_policy.idle);
            }
            upstream_result = upstream.recv(&mut upstream_buffer) => {
                let length = upstream_result.map_err(SessionEnd::UpstreamReceive)?;
                if length > usize::try_from(policy.max_datagram_bytes).unwrap_or(65_507) {
                    return Err(SessionEnd::UpstreamProtocol(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "UDP upstream returned an oversized datagram",
                    )));
                }
                account_session_bytes(&mut session_bytes, length, policy)?;
                listener
                    .send_to(&upstream_buffer[..length], client)
                    .await
                    .map_err(SessionEnd::ClientSend)?;
                traffic_lease
                    .record_bytes_sent(u64::try_from(length).unwrap_or(u64::MAX))
                    .map_err(|_| SessionEnd::Accounting)?;
                reset_sleep(&mut idle, relay_policy.idle);
            }
        }
    }
}

fn account_received(
    traffic_lease: &TrafficLease,
    session_bytes: &mut u64,
    length: usize,
    policy: UdpPolicy,
) -> Result<(), SessionEnd> {
    account_session_bytes(session_bytes, length, policy)?;
    traffic_lease
        .record_bytes_received(u64::try_from(length).unwrap_or(u64::MAX))
        .map_err(|_| SessionEnd::Accounting)
}

fn account_session_bytes(
    session_bytes: &mut u64,
    length: usize,
    policy: UdpPolicy,
) -> Result<(), SessionEnd> {
    let length = u64::try_from(length).unwrap_or(u64::MAX);
    *session_bytes = session_bytes
        .checked_add(length)
        .filter(|bytes| *bytes <= policy.max_session_bytes)
        .ok_or(SessionEnd::SessionBytesLimit)?;
    Ok(())
}

fn reset_sleep(sleep: &mut Option<std::pin::Pin<Box<Sleep>>>, duration: Option<Duration>) {
    if let (Some(sleep), Some(duration)) = (sleep.as_mut(), duration) {
        sleep.as_mut().reset(Instant::now() + duration);
    }
}

async fn wait_for_sleep(sleep: &mut Option<std::pin::Pin<Box<Sleep>>>) {
    match sleep.as_mut() {
        Some(sleep) => sleep.as_mut().await,
        None => pending().await,
    }
}

async fn wait_for_duration(duration: Option<Duration>) {
    match duration {
        Some(duration) => sleep(duration).await,
        None => pending().await,
    }
}

async fn connect_upstream(
    lease: &EndpointLease,
    listener_is_ipv4: bool,
) -> io::Result<(UdpSocket, std::net::SocketAddr)> {
    let addresses = lease.resolve_addresses().await?;
    let mut last_error = None;
    for address in addresses {
        if address.is_ipv4() != listener_is_ipv4 {
            continue;
        }
        let local = if address.is_ipv4() {
            std::net::SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            std::net::SocketAddr::from(([0_u16; 8], 0))
        };
        let socket = UdpSocket::bind(local).await?;
        match socket.connect(address).await {
            Ok(()) => return Ok((socket, address)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "UDP upstream has no address matching the listener address family",
        )
    }))
}

async fn send_datagram(
    socket: &UdpSocket,
    payload: &[u8],
    timeout_duration: Option<Duration>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), io::Error> {
    let send = async {
        match timeout_duration {
            Some(duration) => timeout(duration, socket.send(payload))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP send timed out"))?,
            None => socket.send(payload).await,
        }
        .map(|_| ())
    };
    tokio::select! {
        biased;
        () = wait_for_shutdown(shutdown) => Err(io::Error::new(io::ErrorKind::Interrupted, "UDP send cancelled")),
        result = send => result,
    }
}

async fn send_proxy_datagram(
    socket: &UdpSocket,
    payload: &[u8],
    timeout_duration: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), ProxyProtocolError> {
    tokio::select! {
        biased;
        () = wait_for_shutdown(shutdown) => Err(ProxyProtocolError::new(ProxyProtocolErrorKind::Cancelled)),
        result = timeout(timeout_duration, socket.send(payload)) => match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(ProxyProtocolError::io(error)),
            Err(_) => Err(ProxyProtocolError::new(ProxyProtocolErrorKind::Timeout)),
        },
    }
}

fn proxy_header_budget(policy: ProxyProtocolPolicy, listener_is_ipv4: bool) -> usize {
    match policy.version {
        oxiroute_config::ProxyProtocolVersion::V1 => MAX_V1_HEADER_BYTES,
        oxiroute_config::ProxyProtocolVersion::V2 | oxiroute_config::ProxyProtocolVersion::Auto => {
            if listener_is_ipv4 {
                MAX_V2_IPV4_ADDRESS_HEADER_BYTES
            } else {
                MAX_V2_IPV6_ADDRESS_HEADER_BYTES
            }
        }
    }
}

#[cfg(test)]
mod tests;
