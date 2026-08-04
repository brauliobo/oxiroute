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
use oxiroute_config::UdpPolicy;
use tokio::{
    net::UdpSocket,
    runtime::Builder,
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, Sleep, sleep, timeout},
};

use crate::{
    ConnectionGuard, EndpointLease, HealthFailure, L4ServicePlan, ListenerMetrics,
    ListenerReservation, RuntimeGeneration, RuntimeReferenceKind,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// A generation-owned UDP listener and its bounded pseudo-session runtime.
pub struct UdpRuntime {
    thread: Option<JoinHandle<()>>,
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
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!("oxiroute-udp-{listener_name}"))
            .spawn(move || {
                let result = run(
                    &listener_name,
                    reservation,
                    service,
                    generation.clone(),
                    metrics,
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
}

#[cfg(unix)]
fn run(
    listener_name: &str,
    reservation: ListenerReservation,
    service: Arc<L4ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
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

#[allow(clippy::too_many_lines)]
async fn serve(
    listener_name: &str,
    socket: UdpSocket,
    service: Arc<L4ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
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
    let socket = Arc::new(socket);
    let table: SessionTable = Arc::new(Mutex::new(HashMap::with_capacity(max_sessions)));
    let next_id = AtomicU64::new(0);
    let mut sessions = JoinSet::new();
    let mut receive_buffer = vec![0_u8; max_datagram_bytes.saturating_add(1)];

    loop {
        while sessions.try_join_next().is_some() {}
        tokio::select! {
            _ = shutdown.changed() => break,
            received = socket.recv_from(&mut receive_buffer) => {
                let (length, client) = received?;
                if length > max_datagram_bytes {
                    debug!("UDP listener `{listener_name}` dropped an oversized datagram");
                    continue;
                }
                let payload = receive_buffer[..length].to_vec();
                let mut table_guard = table
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(entry) = table_guard.get(&client) {
                    match entry.queue.try_send(payload) {
                        QueueSendResult::Enqueued | QueueSendResult::Full => {}
                        QueueSendResult::Closed => {
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
                    debug!("UDP listener `{listener_name}` rejected a new pseudo-session at its table limit");
                    continue;
                }
                let Some(generation_reference) =
                    generation.begin_reference(RuntimeReferenceKind::Udp)
                else {
                    break;
                };
                let listener_connection = match metrics.begin_connection() {
                    Ok(connection) => connection,
                    Err(error) => {
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
                drop(table_guard);
                let table_for_task = Arc::clone(&table);
                let socket_for_task = Arc::clone(&socket);
                let service_for_task = Arc::clone(&service);
                let generation_for_task = Arc::clone(&generation);
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
                        shutdown_for_task,
                    )
                    .await;
                    let mut table = table_for_task
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if table.get(&client).is_some_and(|entry| entry.id == id) {
                        table.remove(&client);
                    }
                    if let Err(outcome) = result {
                        debug!("UDP listener `{listener_name}` pseudo-session ended: {outcome}");
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
            warn!("UDP listener `{listener_name}` pseudo-session task failed: {error}");
        }
    }
    Ok(())
}

#[derive(Debug)]
enum SessionEnd {
    Cancelled,
    IdleTimeout,
    LifetimeTimeout,
    Connect(io::Error),
    Io(io::Error),
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
            Self::Io(error) => write!(formatter, "io_error: {error}"),
            Self::Accounting => formatter.write_str("accounting_error"),
            Self::SessionBytesLimit => formatter.write_str("session_bytes_limit"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    listener: Arc<UdpSocket>,
    client: std::net::SocketAddr,
    initial: Vec<u8>,
    mut queue_receiver: mpsc::Receiver<QueuedDatagram>,
    service: Arc<L4ServicePlan>,
    _generation: Arc<RuntimeGeneration>,
    connection: ConnectionGuard,
    _generation_reference: crate::GenerationReference,
    listener_is_ipv4: bool,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), SessionEnd> {
    let policy = service.udp_policy();
    let relay_policy = service.policy();
    let lease = tokio::select! {
        _ = shutdown.changed() => return Err(SessionEnd::Cancelled),
        lease = service.select_wait() => lease.ok_or_else(|| SessionEnd::Connect(io::Error::new(
            io::ErrorKind::NotFound,
            "UDP upstream pool has no selectable endpoint",
        )))?,
    };
    let upstream = match timeout(
        relay_policy.connect,
        connect_upstream(&lease, listener_is_ipv4),
    )
    .await
    {
        Ok(Ok(socket)) => socket,
        Ok(Err(error)) => {
            lease.record_passive_failure(HealthFailure::ConnectFailed);
            return Err(SessionEnd::Connect(error));
        }
        Err(_) => {
            lease.record_passive_failure(HealthFailure::Timeout);
            return Err(SessionEnd::Connect(io::Error::new(
                io::ErrorKind::TimedOut,
                "UDP upstream connect timed out",
            )));
        }
    };

    let mut session_bytes = 0_u64;
    account_received(&connection, &mut session_bytes, initial.len(), policy)?;
    upstream.send(&initial).await.map_err(SessionEnd::Io)?;

    let mut idle = relay_policy.idle.map(|duration| Box::pin(sleep(duration)));
    let lifetime = wait_for_duration(relay_policy.lifetime);
    tokio::pin!(lifetime);
    let mut upstream_buffer =
        vec![0_u8; usize::try_from(policy.max_datagram_bytes).unwrap_or(65_507) + 1];

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Err(SessionEnd::Cancelled),
            () = wait_for_sleep(&mut idle) => return Err(SessionEnd::IdleTimeout),
            () = &mut lifetime => return Err(SessionEnd::LifetimeTimeout),
            queued = queue_receiver.recv() => {
                let Some(queued) = queued else { return Ok(()) };
                account_received(&connection, &mut session_bytes, queued.payload.len(), policy)?;
                upstream.send(&queued.payload).await.map_err(SessionEnd::Io)?;
                reset_sleep(&mut idle, relay_policy.idle);
            }
            upstream_result = upstream.recv(&mut upstream_buffer) => {
                let length = upstream_result.map_err(SessionEnd::Io)?;
                if length > usize::try_from(policy.max_datagram_bytes).unwrap_or(65_507) {
                    return Err(SessionEnd::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "UDP upstream returned an oversized datagram",
                    )));
                }
                account_session_bytes(&mut session_bytes, length, policy)?;
                listener
                    .send_to(&upstream_buffer[..length], client)
                    .await
                    .map_err(SessionEnd::Io)?;
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

async fn connect_upstream(lease: &EndpointLease, listener_is_ipv4: bool) -> io::Result<UdpSocket> {
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
            Ok(()) => return Ok(socket),
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

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, UdpSocket as StdUdpSocket};

    use oxiroute_config::{
        Config, DownstreamTimeoutPolicy, L4Service, Listener, ListenerBind, Protocol,
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
