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
    ConnectionGuard, EndpointLease, HealthFailure, L4ServicePlan, ListenerMetrics,
    ListenerReservation, MAX_V1_HEADER_BYTES, ProxyProtocolError, ProxyProtocolErrorKind,
    ProxyProtocolResult, ProxyProtocolTransport, RuntimeGeneration, RuntimeReferenceKind,
    encode_header, parse_header,
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
                let Some(generation_reference) =
                    generation.begin_reference(RuntimeReferenceKind::Udp)
                else {
                    UdpAccounting::increment(&accounting.datagrams_dropped);
                    continue;
                };
                let listener_connection = match metrics.begin_connection() {
                    Ok(connection) => connection,
                    Err(error) => {
                        UdpAccounting::increment(&accounting.datagrams_dropped);
                        debug!("UDP listener `{listener_name}` rejected a pseudo-session: {error}");
                        drop(generation_reference);
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
                        listener_connection,
                        generation_reference,
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
    connection: ConnectionGuard,
    _generation_reference: crate::GenerationReference,
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
        connection,
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
    connection: ConnectionGuard,
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
    account_received(&connection, &mut session_bytes, initial.len(), policy)?;
    let outbound_proxy = service.proxy_protocol().zip(logical_client);
    if let Some((proxy_policy, source)) = outbound_proxy {
        let header = encode_header(
            proxy_policy.version,
            ProxyProtocolTransport::Datagram,
            source,
            destination,
        )
        .map_err(|error| {
            let _ = connection.record_proxy_protocol(error.result());
            SessionEnd::ProxyProtocol(error)
        })?;
        let mut datagram = Vec::with_capacity(header.len() + initial.len());
        datagram.extend_from_slice(&header);
        datagram.extend_from_slice(&initial);
        if datagram.len() > MAX_UDP_WIRE_DATAGRAM_BYTES {
            let error = ProxyProtocolError::new(ProxyProtocolErrorKind::InvalidLength);
            let _ = connection.record_proxy_protocol(error.result());
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
            let _ = connection.record_proxy_protocol(error.result());
            SessionEnd::UpstreamProxyProtocol(error)
        })?;
        connection
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
                account_received(&connection, &mut session_bytes, queued.payload.len(), policy)?;
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
                connection
                    .record_bytes_sent(u64::try_from(length).unwrap_or(u64::MAX))
                    .map_err(|_| SessionEnd::Accounting)?;
                reset_sleep(&mut idle, relay_policy.idle);
            }
        }
    }
}

fn account_received(
    connection: &ConnectionGuard,
    session_bytes: &mut u64,
    length: usize,
    policy: UdpPolicy,
) -> Result<(), SessionEnd> {
    account_session_bytes(session_bytes, length, policy)?;
    connection
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
mod tests {
    use std::net::{SocketAddr, UdpSocket as StdUdpSocket};

    use oxiroute_config::{
        Config, DownstreamTimeoutPolicy, L4Service, Listener, ListenerBind, PassiveHealthPolicy,
        PassiveObserve, PassiveOnError, Protocol, ProxyProtocolPolicy, ProxyProtocolVersion,
        UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
    };
    use oxiroute_config_source::ConfigFormat;

    use crate::{
        GenerationManager, RuntimeReferenceKind, ServiceKind,
        config_coordinator::{CanonicalConfigDocument, ConfigRevision},
    };

    use super::*;

    fn policy() -> UdpPolicy {
        UdpPolicy {
            max_datagram_bytes: 8,
            max_sessions: 2,
            max_session_bytes: 64,
            max_queue_datagrams: 2,
            max_queue_bytes: 12,
        }
    }

    #[tokio::test]
    async fn queue_enforces_datagram_and_byte_bounds() {
        let (queue, mut receiver) = DatagramQueue::new(policy()).expect("queue");
        assert!(matches!(
            queue.try_send(vec![0; 8]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(
            queue.try_send(vec![0; 4]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));
        let datagram = receiver.recv().await.expect("queued datagram");
        assert_eq!(datagram.payload.len(), 8);
        drop(datagram);
        assert!(matches!(
            queue.try_send(vec![0; 4]),
            QueueSendResult::Enqueued
        ));
    }

    #[tokio::test]
    async fn queue_rejects_datagrams_at_the_count_bound() {
        let mut policy = policy();
        policy.max_queue_datagrams = 2;
        policy.max_queue_bytes = 64;
        let (queue, mut receiver) = DatagramQueue::new(policy).expect("queue");

        assert!(matches!(
            queue.try_send(vec![0; 1]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(
            queue.try_send(vec![0; 1]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));

        drop(receiver.recv().await.expect("first queued datagram"));
        assert!(matches!(
            queue.try_send(vec![0; 1]),
            QueueSendResult::Enqueued
        ));
    }

    #[tokio::test]
    async fn queue_rejects_datagrams_at_the_byte_bound() {
        let mut policy = policy();
        policy.max_queue_datagrams = 4;
        policy.max_queue_bytes = 5;
        let (queue, mut receiver) = DatagramQueue::new(policy).expect("queue");

        assert!(matches!(
            queue.try_send(vec![0; 3]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(
            queue.try_send(vec![0; 2]),
            QueueSendResult::Enqueued
        ));
        assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));

        drop(receiver.recv().await.expect("first queued datagram"));
        assert!(matches!(
            queue.try_send(vec![0; 1]),
            QueueSendResult::Enqueued
        ));
    }

    #[tokio::test]
    async fn queue_reports_closed_session_without_retaining_bytes() {
        let (queue, receiver) = DatagramQueue::new(policy()).expect("queue");
        drop(receiver);
        assert!(matches!(
            queue.try_send(vec![0; 4]),
            QueueSendResult::Closed
        ));
        assert_eq!(queue.queued_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn session_byte_accounting_rejects_only_bytes_after_the_bound() {
        let mut total = 0;
        assert!(account_session_bytes(&mut total, 64, policy()).is_ok());
        assert_eq!(total, 64);
        assert!(matches!(
            account_session_bytes(&mut total, 1, policy()),
            Err(SessionEnd::SessionBytesLimit)
        ));
        assert_eq!(total, 64);
    }

    #[test]
    fn session_failure_attribution_only_observes_genuine_upstream_outcomes() {
        assert_eq!(
            SessionEnd::UpstreamConnect(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "refused"
            ))
            .passive_failure(),
            Some(HealthFailure::ConnectFailed)
        );
        assert_eq!(
            SessionEnd::UpstreamSend(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"))
                .passive_failure(),
            Some(HealthFailure::ProtocolError)
        );
        assert_eq!(
            SessionEnd::UpstreamReceive(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
                .passive_failure(),
            Some(HealthFailure::Timeout)
        );
        assert_eq!(
            SessionEnd::UpstreamProtocol(io::Error::new(io::ErrorKind::InvalidData, "oversized"))
                .passive_failure(),
            Some(HealthFailure::ProtocolError)
        );
        assert_eq!(
            SessionEnd::UpstreamProxyProtocol(ProxyProtocolError::new(
                ProxyProtocolErrorKind::Cancelled,
            ))
            .passive_failure(),
            None
        );

        let excluded = [
            SessionEnd::Cancelled,
            SessionEnd::IdleTimeout,
            SessionEnd::LifetimeTimeout,
            SessionEnd::Connect(io::Error::other("pool unavailable")),
            SessionEnd::ClientSend(io::Error::other("client closed")),
            SessionEnd::ProxyProtocol(ProxyProtocolError::new(
                ProxyProtocolErrorKind::InvalidLength,
            )),
            SessionEnd::Accounting,
            SessionEnd::SessionBytesLimit,
        ];
        assert!(
            excluded
                .iter()
                .all(|outcome| outcome.passive_failure().is_none())
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_routes_replies_and_releases_its_generation_reference() {
        let upstream = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("upstream socket");
        let upstream_address = upstream.local_addr().expect("upstream address");
        let listener_address = StdUdpSocket::bind(("127.0.0.1", 0))
            .expect("listener probe")
            .local_addr()
            .expect("listener address");
        let config = udp_config(listener_address, upstream_address);
        let manager = GenerationManager::new();
        let candidate = manager
            .prepare(document(config))
            .expect("prepare UDP generation");
        let generation = manager
            .activate(&candidate)
            .expect("activate UDP generation");
        generation
            .metrics()
            .register_configured_listener(
                "relay",
                "udp",
                &ListenerBind::Udp {
                    address: listener_address,
                },
                Some(8),
            )
            .expect("register UDP listener metrics");
        let service = match &generation.plan().services[0].kind {
            ServiceKind::Udp(service) => Arc::clone(service),
            _ => panic!("test service must be UDP"),
        };
        let reservation = generation
            .reservations()
            .get("relay")
            .cloned()
            .expect("UDP reservation");
        let metrics = generation
            .metrics()
            .listener("relay")
            .expect("listener metrics registry")
            .expect("UDP listener metrics");
        let (shutdown_tx, shutdown) = watch::channel(false);
        let runtime = UdpRuntime::start(
            "relay".into(),
            reservation,
            service,
            Arc::clone(&generation),
            metrics,
            None,
            shutdown,
        )
        .expect("start UDP runtime");
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(b"query", listener_address)
            .await
            .expect("send query");

        let mut received = [0_u8; 128];
        let (length, upstream_peer) =
            timeout(Duration::from_secs(2), upstream.recv_from(&mut received))
                .await
                .expect("upstream receive timeout")
                .expect("upstream receive");
        assert_eq!(&received[..length], b"query");
        upstream
            .send_to(b"response", upstream_peer)
            .await
            .expect("send response");

        let (length, _) = timeout(Duration::from_secs(2), client.recv_from(&mut received))
            .await
            .expect("client receive timeout")
            .expect("client receive");
        assert_eq!(&received[..length], b"response");

        shutdown_tx.send(true).expect("shutdown UDP runtime");
        runtime.join().expect("join UDP runtime");
        assert_eq!(generation.active_references(RuntimeReferenceKind::Udp), 0);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_connect_failure_records_one_passive_failure() {
        let harness = start_harness_with_options(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
            Some(passive_health(1, 1_000)),
            Some(SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                9,
            )),
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(b"connect", harness.listener)
            .await
            .expect("connect failure datagram");
        wait_for_udp_passive_failures(&harness.generation, 1).await;
        wait_for_udp_references(&harness.generation, 0).await;

        let endpoint = &harness.generation.plan().pools[0]
            .health_snapshot()
            .endpoints[0];
        assert_eq!(endpoint.passive_failure_count, 1);
        assert_eq!(
            endpoint.passive_ejection_reason,
            Some(HealthFailure::ConnectFailed)
        );
        assert!(endpoint.passive_ejected);
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_upstream_send_failure_is_passively_classified() {
        let peer = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("peer socket");
        let socket = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("upstream socket");
        socket
            .connect(peer.local_addr().expect("peer address"))
            .await
            .expect("connect upstream socket");
        let socket = socket.into_std().expect("convert upstream socket");
        rustix::net::shutdown(&socket, rustix::net::Shutdown::Both).expect("close upstream socket");
        let socket = UdpSocket::from_std(socket).expect("restore upstream socket");
        let (_shutdown_sender, mut shutdown) = watch::channel(false);

        let error = send_datagram(&socket, b"send-failure", None, &mut shutdown)
            .await
            .expect_err("closed upstream send");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(
            SessionEnd::UpstreamSend(error).passive_failure(),
            Some(HealthFailure::ProtocolError)
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_upstream_receive_failure_records_one_passive_failure() {
        let UdpHarness {
            listener,
            upstream,
            generation,
            shutdown,
            runtime,
        } = start_harness_with_options(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
            Some(passive_health(1, 1_000)),
            None,
            None,
        )
        .await;
        drop(upstream);
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(b"recv", listener)
            .await
            .expect("receive failure datagram");
        wait_for_udp_passive_failures(&generation, 1).await;
        wait_for_udp_references(&generation, 0).await;

        let endpoint = &generation.plan().pools[0].health_snapshot().endpoints[0];
        assert_eq!(endpoint.passive_failure_count, 1);
        assert_eq!(
            endpoint.passive_ejection_reason,
            Some(HealthFailure::ProtocolError)
        );
        shutdown.send(true).expect("shutdown UDP runtime");
        runtime.join().expect("join UDP runtime");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_oversized_reply_records_one_passive_failure() {
        let mut udp_policy = policy();
        udp_policy.max_datagram_bytes = 8;
        let harness = start_harness_with_options(
            udp_policy,
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
            Some(passive_health(1, 1_000)),
            None,
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(b"query", harness.listener)
            .await
            .expect("oversized reply datagram");
        let mut received = [0_u8; 32];
        let (_, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("upstream query timeout")
        .expect("upstream query receive");
        harness
            .upstream
            .send_to(&[0; 9], peer)
            .await
            .expect("oversized upstream reply");
        wait_for_udp_passive_failures(&harness.generation, 1).await;
        wait_for_udp_references(&harness.generation, 0).await;
        assert!(
            timeout(Duration::from_millis(100), client.recv_from(&mut received))
                .await
                .is_err(),
            "oversized reply reached the client"
        );

        let endpoint = &harness.generation.plan().pools[0]
            .health_snapshot()
            .endpoints[0];
        assert_eq!(endpoint.passive_failure_count, 1);
        assert_eq!(
            endpoint.passive_ejection_reason,
            Some(HealthFailure::ProtocolError)
        );
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_passive_failure_threshold_ejects_and_expiry_readmits() {
        let mut udp_policy = policy();
        udp_policy.max_datagram_bytes = 8;
        let harness = start_harness_with_options(
            udp_policy,
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
            Some(passive_health(2, 100)),
            None,
            None,
        )
        .await;
        let mut received = [0_u8; 32];
        for (failure_count, query) in [&b"first"[..], &b"second"[..]].into_iter().enumerate() {
            let client = UdpSocket::bind(("127.0.0.1", 0))
                .await
                .expect("client socket");
            client
                .send_to(query, harness.listener)
                .await
                .expect("failure datagram");
            let (_, peer) = timeout(
                Duration::from_secs(2),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .expect("upstream query timeout")
            .expect("upstream query receive");
            harness
                .upstream
                .send_to(&[0; 9], peer)
                .await
                .expect("oversized upstream reply");
            wait_for_udp_passive_failures(
                &harness.generation,
                u64::try_from(failure_count + 1).expect("failure count"),
            )
            .await;
            wait_for_udp_references(&harness.generation, 0).await;
        }

        let ejected = &harness.generation.plan().pools[0]
            .health_snapshot()
            .endpoints[0];
        assert_eq!(ejected.passive_failure_count, 2);
        assert_eq!(ejected.passive_ejection_count, 1);
        assert!(ejected.passive_ejected);

        let rejected = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("rejected client socket");
        rejected
            .send_to(b"rejected", harness.listener)
            .await
            .expect("rejected datagram");
        assert!(
            timeout(
                Duration::from_millis(25),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .is_err(),
            "ejected endpoint received a new session"
        );

        sleep(Duration::from_millis(125)).await;
        let recovered = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("recovered client socket");
        recovered
            .send_to(b"recover", harness.listener)
            .await
            .expect("recovered datagram");
        let (length, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("recovered query timeout")
        .expect("recovered query receive");
        assert_eq!(&received[..length], b"recover");
        harness
            .upstream
            .send_to(b"reply", peer)
            .await
            .expect("recovered reply");
        let (length, _) = timeout(Duration::from_secs(2), recovered.recv_from(&mut received))
            .await
            .expect("recovered reply timeout")
            .expect("recovered reply receive");
        assert_eq!(&received[..length], b"reply");
        assert!(
            !harness.generation.plan().pools[0]
                .health_snapshot()
                .endpoints[0]
                .passive_ejected
        );
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_datagrams_are_dropped_before_session_admission() {
        let mut policy = policy();
        policy.max_datagram_bytes = 4;
        let harness = start_harness(
            policy,
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");

        client
            .send_to(b"12345", harness.listener)
            .await
            .expect("oversized datagram");
        assert!(
            timeout(
                Duration::from_millis(100),
                harness.upstream.recv_from(&mut [0; 32])
            )
            .await
            .is_err(),
            "oversized datagram reached the upstream"
        );

        client
            .send_to(b"1234", harness.listener)
            .await
            .expect("bounded datagram");
        let mut received = [0; 32];
        let (length, _) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("bounded datagram timeout")
        .expect("bounded datagram receive");
        assert_eq!(&received[..length], b"1234");

        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_drop_and_terminal_counters_account_bounded_work() {
        let mut udp_policy = policy();
        udp_policy.max_datagram_bytes = 4;
        let harness = start_harness(
            udp_policy,
            Duration::from_millis(40),
            Some(Duration::from_secs(2)),
            8,
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(b"12345", harness.listener)
            .await
            .expect("oversized datagram");
        timeout(Duration::from_secs(2), async {
            loop {
                let stats = harness.runtime.stats();
                if stats.datagrams_received == 1 && stats.datagrams_dropped == 1 {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("UDP drop accounting timeout");

        client
            .send_to(b"1234", harness.listener)
            .await
            .expect("bounded datagram");
        timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut [0_u8; 32]),
        )
        .await
        .expect("bounded datagram upstream timeout")
        .expect("bounded datagram upstream receive");
        timeout(Duration::from_secs(2), async {
            loop {
                if harness.runtime.stats().sessions_failed == 1 {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("UDP terminal accounting timeout");

        assert_eq!(
            harness.runtime.stats(),
            UdpRelayStats {
                datagrams_received: 2,
                datagrams_dropped: 1,
                sessions_started: 1,
                sessions_completed: 0,
                sessions_failed: 1,
            }
        );
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_session_table_rejects_new_clients_at_max_sessions() {
        let mut policy = policy();
        policy.max_sessions = 1;
        let harness = start_harness(
            policy,
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
        )
        .await;
        let first = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("first client socket");
        let second = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("second client socket");
        let mut received = [0; 32];

        first
            .send_to(b"first", harness.listener)
            .await
            .expect("first session datagram");
        let (length, first_peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("first session timeout")
        .expect("first session receive");
        assert_eq!(&received[..length], b"first");

        second
            .send_to(b"second", harness.listener)
            .await
            .expect("second session datagram");
        assert!(
            timeout(
                Duration::from_millis(100),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .is_err(),
            "table overflow reached the upstream"
        );

        first
            .send_to(b"again", harness.listener)
            .await
            .expect("existing session datagram");
        let (length, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("existing session timeout")
        .expect("existing session receive");
        assert_eq!(&received[..length], b"again");
        assert_eq!(peer, first_peer);

        assert_eq!(udp_passive_failure_count(&harness.generation), 0);
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_session_enforces_byte_cap_and_releases_table_entry() {
        let mut policy = policy();
        policy.max_session_bytes = 10;
        let harness = start_harness(
            policy,
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        let mut received = [0; 32];

        client
            .send_to(&[0; 6], harness.listener)
            .await
            .expect("initial datagram");
        let (length, _) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("initial datagram timeout")
        .expect("initial datagram receive");
        assert_eq!(length, 6);

        client
            .send_to(&[0; 5], harness.listener)
            .await
            .expect("over-limit datagram");
        assert!(
            timeout(
                Duration::from_millis(100),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .is_err(),
            "over-limit session datagram reached the upstream"
        );
        wait_for_udp_references(&harness.generation, 0).await;

        client
            .send_to(&[0; 3], harness.listener)
            .await
            .expect("new session datagram");
        let (length, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("new session timeout")
        .expect("new session receive");
        assert_eq!(length, 3);

        harness
            .upstream
            .send_to(&[0; 8], peer)
            .await
            .expect("over-limit upstream datagram");
        assert!(
            timeout(Duration::from_millis(100), client.recv_from(&mut received))
                .await
                .is_err(),
            "over-limit upstream datagram reached the client"
        );
        wait_for_udp_references(&harness.generation, 0).await;
        assert_eq!(udp_passive_failure_count(&harness.generation), 0);

        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_sessions_expire_after_idle_and_lifetime_deadlines() {
        let idle_harness = start_harness(
            policy(),
            Duration::from_millis(40),
            Some(Duration::from_secs(2)),
            8,
            None,
        )
        .await;
        let idle_client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("idle client socket");
        idle_client
            .send_to(b"idle", idle_harness.listener)
            .await
            .expect("idle datagram");
        let mut received = [0; 32];
        timeout(
            Duration::from_secs(2),
            idle_harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("idle upstream timeout")
        .expect("idle upstream receive");
        wait_for_udp_references(&idle_harness.generation, 0).await;
        assert_eq!(udp_passive_failure_count(&idle_harness.generation), 0);
        idle_harness.stop();

        let lifetime_harness = start_harness(
            policy(),
            Duration::from_secs(2),
            Some(Duration::from_millis(80)),
            8,
            None,
        )
        .await;
        let lifetime_client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("lifetime client socket");
        lifetime_client
            .send_to(b"life", lifetime_harness.listener)
            .await
            .expect("lifetime datagram");
        timeout(
            Duration::from_secs(2),
            lifetime_harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("lifetime upstream timeout")
        .expect("lifetime upstream receive");
        sleep(Duration::from_millis(100)).await;
        wait_for_udp_references(&lifetime_harness.generation, 0).await;
        assert_eq!(udp_passive_failure_count(&lifetime_harness.generation), 0);
        lifetime_harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_replies_keep_client_affinity_without_cross_client_leakage() {
        let harness = start_harness(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
        )
        .await;
        let first = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("first client socket");
        let second = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("second client socket");
        let mut received = [0; 32];

        first
            .send_to(b"one", harness.listener)
            .await
            .expect("first query");
        second
            .send_to(b"two", harness.listener)
            .await
            .expect("second query");
        let mut peers = Vec::new();
        for _ in 0..2 {
            let (length, peer) = timeout(
                Duration::from_secs(2),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .expect("upstream query timeout")
            .expect("upstream query receive");
            peers.push((received[..length].to_vec(), peer));
        }

        for (query, peer) in peers {
            let reply = if query == b"one" {
                b"r-one".as_slice()
            } else {
                b"r-two".as_slice()
            };
            harness
                .upstream
                .send_to(reply, peer)
                .await
                .expect("upstream reply");
        }

        let (length, _) = timeout(Duration::from_secs(2), first.recv_from(&mut received))
            .await
            .expect("first reply timeout")
            .expect("first reply receive");
        assert_eq!(&received[..length], b"r-one");
        let (length, _) = timeout(Duration::from_secs(2), second.recv_from(&mut received))
            .await
            .expect("second reply timeout")
            .expect("second reply receive");
        assert_eq!(&received[..length], b"r-two");
        assert!(
            timeout(Duration::from_millis(100), first.recv_from(&mut received))
                .await
                .is_err(),
            "first client received a second client's reply"
        );
        assert!(
            timeout(Duration::from_millis(100), second.recv_from(&mut received))
                .await
                .is_err(),
            "second client received a first client's reply"
        );

        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_proxy_v2_datagrams_are_rejected_before_upstream_delivery() {
        let harness = start_harness(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            Some(ProxyProtocolPolicy {
                version: ProxyProtocolVersion::V2,
                timeout_ms: 1_000,
            }),
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(
                &[0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x00, 0x00, 0x00],
                harness.listener,
            )
            .await
            .expect("malformed PROXY v2 datagram");

        assert!(
            timeout(
                Duration::from_millis(100),
                harness.upstream.recv_from(&mut [0; 32])
            )
            .await
            .is_err(),
            "malformed PROXY v2 datagram reached the upstream"
        );
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_proxy_v2_is_accepted_and_propagated_on_the_wire() {
        let proxy_policy = ProxyProtocolPolicy {
            version: ProxyProtocolVersion::V2,
            timeout_ms: 1_000,
        };
        let harness = start_harness_with_options(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            Some(proxy_policy),
            None,
            None,
            Some(proxy_policy),
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        let source = SocketAddr::from(([192, 0, 2, 10], 1234));
        let mut datagram = encode_header(
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Datagram,
            source,
            harness.listener,
        )
        .expect("client PROXY header");
        datagram.extend_from_slice(b"query");
        client
            .send_to(&datagram, harness.listener)
            .await
            .expect("PROXY query");

        let mut received = [0_u8; 128];
        let (length, upstream_peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("upstream PROXY query timeout")
        .expect("upstream PROXY query receive");
        let header = parse_header(
            &received[..length],
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Datagram,
        )
        .expect("upstream PROXY parse")
        .expect("complete upstream PROXY header");
        assert_eq!(header.source, source);
        assert_eq!(
            header.destination,
            harness.upstream.local_addr().expect("upstream address")
        );
        assert_eq!(&received[header.consumed..length], b"query");

        harness
            .upstream
            .send_to(b"response", upstream_peer)
            .await
            .expect("upstream response");
        let (length, _) = timeout(Duration::from_secs(2), client.recv_from(&mut received))
            .await
            .expect("client response timeout")
            .expect("client response receive");
        assert_eq!(&received[..length], b"response");
        harness.stop();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_reload_rejects_new_sessions_and_shutdown_cancels_active_sessions() {
        let harness = start_harness(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
        )
        .await;
        let active = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("active client socket");
        let new_client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("new client socket");
        let mut received = [0; 32];

        active
            .send_to(b"active", harness.listener)
            .await
            .expect("active session datagram");
        timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("active session timeout")
        .expect("active session receive");
        harness.generation.stop_accepting();

        new_client
            .send_to(b"reload", harness.listener)
            .await
            .expect("reload datagram");
        assert!(
            timeout(
                Duration::from_millis(100),
                harness.upstream.recv_from(&mut received),
            )
            .await
            .is_err(),
            "reload admitted new UDP work"
        );
        sleep(Duration::from_millis(100)).await;
        assert!(
            !harness
                .runtime
                .thread
                .as_ref()
                .is_some_and(JoinHandle::is_finished),
            "reload terminated the UDP listener before shutdown"
        );

        harness.shutdown.send(true).expect("shutdown UDP runtime");
        harness.runtime.join().expect("join UDP runtime");
        assert_eq!(udp_passive_failure_count(&harness.generation), 0);
        assert_eq!(
            harness
                .generation
                .active_references(RuntimeReferenceKind::Udp),
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_administrative_drain_does_not_record_active_upstream_failure() {
        let UdpHarness {
            listener,
            upstream,
            generation,
            shutdown,
            runtime,
        } = start_harness_with_options(
            policy(),
            Duration::from_secs(10),
            Some(Duration::from_secs(30)),
            8,
            None,
            Some(passive_health(1, 1_000)),
            None,
            None,
        )
        .await;
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        let mut received = [0_u8; 32];
        client
            .send_to(b"active", listener)
            .await
            .expect("active datagram");
        timeout(Duration::from_secs(2), upstream.recv_from(&mut received))
            .await
            .expect("active upstream timeout")
            .expect("active upstream receive");
        generation.stop_accepting();
        drop(upstream);
        client
            .send_to(b"again", listener)
            .await
            .expect("draining datagram");
        wait_for_udp_references(&generation, 0).await;
        assert_eq!(udp_passive_failure_count(&generation), 0);
        shutdown.send(true).expect("shutdown UDP runtime");
        runtime.join().expect("join UDP runtime");
    }

    #[cfg(unix)]
    struct UdpHarness {
        listener: SocketAddr,
        upstream: UdpSocket,
        generation: Arc<RuntimeGeneration>,
        shutdown: watch::Sender<bool>,
        runtime: UdpRuntime,
    }

    #[cfg(unix)]
    impl UdpHarness {
        fn stop(self) {
            self.shutdown.send(true).expect("shutdown UDP runtime");
            self.runtime.join().expect("join UDP runtime");
        }
    }

    #[cfg(unix)]
    fn passive_health(error_limit: u16, backoff_ms: u64) -> PassiveHealthPolicy {
        PassiveHealthPolicy {
            observe: PassiveObserve::Layer7,
            on_error: PassiveOnError::Count,
            error_limit,
            mark_down: false,
            mark_up: false,
            initial_backoff_ms: backoff_ms,
            max_backoff_ms: backoff_ms,
            recovery_threshold: 1,
        }
    }

    #[cfg(unix)]
    async fn start_harness(
        policy: UdpPolicy,
        idle_timeout: Duration,
        lifetime_timeout: Option<Duration>,
        max_connections: u64,
        proxy_protocol: Option<ProxyProtocolPolicy>,
    ) -> UdpHarness {
        start_harness_with_options(
            policy,
            idle_timeout,
            lifetime_timeout,
            max_connections,
            proxy_protocol,
            None,
            None,
            None,
        )
        .await
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    async fn start_harness_with_options(
        policy: UdpPolicy,
        idle_timeout: Duration,
        lifetime_timeout: Option<Duration>,
        max_connections: u64,
        proxy_protocol: Option<ProxyProtocolPolicy>,
        passive_health: Option<PassiveHealthPolicy>,
        configured_upstream: Option<SocketAddr>,
        upstream_proxy_protocol: Option<ProxyProtocolPolicy>,
    ) -> UdpHarness {
        let upstream = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("upstream socket");
        let upstream_address =
            configured_upstream.unwrap_or_else(|| upstream.local_addr().expect("upstream address"));
        let listener = StdUdpSocket::bind(("127.0.0.1", 0))
            .expect("listener probe")
            .local_addr()
            .expect("listener address");
        let manager = GenerationManager::new();
        let mut config = udp_config(listener, upstream_address);
        config.listeners[0].max_connections = Some(max_connections);
        config.listeners[0].proxy_protocol = proxy_protocol;
        config.l4_services[0].idle_timeout_ms =
            u64::try_from(idle_timeout.as_millis()).expect("test idle timeout fits");
        config.l4_services[0].lifetime_timeout_ms = lifetime_timeout
            .map(|value| u64::try_from(value.as_millis()).expect("test lifetime fits"));
        config.l4_services[0].proxy_protocol = upstream_proxy_protocol;
        config.l4_services[0].udp = Some(policy);
        config.upstream_pools[0].passive_health = passive_health;
        let candidate = manager
            .prepare(document(config))
            .expect("prepare UDP generation");
        let generation = manager
            .activate(&candidate)
            .expect("activate UDP generation");
        generation
            .metrics()
            .register_configured_listener(
                "relay",
                "udp",
                &ListenerBind::Udp { address: listener },
                Some(max_connections),
            )
            .expect("register UDP listener metrics");
        let service = match &generation.plan().services[0].kind {
            ServiceKind::Udp(service) => Arc::clone(service),
            _ => panic!("test service must be UDP"),
        };
        let reservation = generation
            .reservations()
            .get("relay")
            .cloned()
            .expect("UDP reservation");
        let metrics = generation
            .metrics()
            .listener("relay")
            .expect("listener metrics registry")
            .expect("UDP listener metrics");
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let runtime = UdpRuntime::start(
            "relay".into(),
            reservation,
            service,
            Arc::clone(&generation),
            metrics,
            proxy_protocol,
            shutdown_receiver,
        )
        .expect("start UDP runtime");
        UdpHarness {
            listener,
            upstream,
            generation,
            shutdown,
            runtime,
        }
    }

    #[cfg(unix)]
    async fn wait_for_udp_references(generation: &RuntimeGeneration, expected: u64) {
        timeout(Duration::from_secs(2), async {
            loop {
                if generation.active_references(RuntimeReferenceKind::Udp) == expected {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("UDP generation reference timeout");
    }

    #[cfg(unix)]
    async fn wait_for_udp_passive_failures(generation: &RuntimeGeneration, expected: u64) {
        timeout(Duration::from_secs(2), async {
            loop {
                let failures =
                    generation.plan().pools[0].health_snapshot().endpoints[0].passive_failure_count;
                if failures >= expected {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("UDP passive failure timeout");
    }

    #[cfg(unix)]
    fn udp_passive_failure_count(generation: &RuntimeGeneration) -> u64 {
        generation.plan().pools[0].health_snapshot().endpoints[0].passive_failure_count
    }

    #[cfg(unix)]
    fn udp_config(listener: SocketAddr, upstream: SocketAddr) -> Config {
        Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: vec![Listener {
                name: "relay".into(),
                bind: ListenerBind::Udp { address: listener },
                protocol: Protocol::Udp,
                service: Some("relay".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: Some(8),
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
            cache_stores: Vec::new(),
            upstream_pools: vec![UpstreamPool {
                name: "upstream".into(),
                servers: Vec::new(),
                endpoints: vec![UpstreamEndpoint::Socket { address: upstream }],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                passive_health: None,
                tls: None,
                http_versions: oxiroute_config::HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::default(),
            }],
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: vec![L4Service {
                name: "relay".into(),
                upstream_pool: "upstream".into(),
                connect_timeout_ms: 1_000,
                idle_timeout_ms: 10_000,
                lifetime_timeout_ms: Some(30_000),
                proxy_protocol: None,
                udp: Some(policy()),
            }],
        }
    }

    fn document(config: Config) -> CanonicalConfigDocument {
        CanonicalConfigDocument {
            disk_revision: ConfigRevision::from_bytes(b"udp-test"),
            candidate_revision: ConfigRevision::from_bytes(b"udp-test"),
            normalized_config: config,
            format: ConfigFormat::Lua,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        }
    }
}
