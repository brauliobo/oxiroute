use std::{
    error::Error,
    fmt, io,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use log::warn;
use oxiroute_config::ProxyProtocolPolicy;
use pingora::{protocols::Stream, server::ShutdownWatch};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, split},
    net::TcpStream,
    time::{Instant, sleep_until, timeout},
};

#[cfg(unix)]
use tokio::net::UnixStream;

use crate::{
    ConnectionGuard, EndpointLease, HealthFailure, L4ServicePlan, MetricsError, ProxyProtocolError,
    ProxyProtocolErrorKind, ProxyProtocolResult, ProxyProtocolTransport, RuntimeEndpoint,
    TcpRelayResult, encode_header,
};

/// Memory used for each direction of a relayed connection until it moves bulk data.
pub const RELAY_BUFFER_SIZE: usize = 16 * 1024;
/// The most memory a direction grows to once a read fills its buffer: a transfer costs about the same instructions
/// whatever its size, so a connection that moves a lot of data takes fewer, larger ones.
const RELAY_BULK_BUFFER_SIZE: usize = 64 * 1024;

/// Timeout policy for one TCP relay connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayPolicy {
    /// Maximum time allowed to establish the upstream connection.
    pub connect: Duration,
    /// Maximum period without a successful payload read or write.
    pub idle: Option<Duration>,
    /// Maximum duration of the established relay, regardless of activity.
    pub lifetime: Option<Duration>,
}

impl RelayPolicy {
    #[must_use]
    pub const fn new(connect_timeout: Duration) -> Self {
        Self {
            connect: connect_timeout,
            idle: None,
            lifetime: None,
        }
    }
}

/// Successfully accounted traffic observed at the downstream side of a relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayStats {
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// Direction in which a relay I/O failure occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayDirection {
    ClientToUpstream,
    UpstreamToClient,
}

impl fmt::Display for RelayDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientToUpstream => formatter.write_str("client to upstream"),
            Self::UpstreamToClient => formatter.write_str("upstream to client"),
        }
    }
}

/// I/O operation that failed while relaying a direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayOperation {
    Read,
    Write,
    Shutdown,
}

impl fmt::Display for RelayOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => formatter.write_str("read"),
            Self::Write => formatter.write_str("write"),
            Self::Shutdown => formatter.write_str("shutdown"),
        }
    }
}

/// Reason a relay stopped before both directions reached EOF.
#[derive(Debug)]
pub enum RelayFailureKind {
    Connect(io::Error),
    ConnectTimeout(Duration),
    ProxyProtocol(ProxyProtocolError),
    IdleTimeout(Duration),
    LifetimeTimeout(Duration),
    Cancelled,
    Io {
        direction: RelayDirection,
        operation: RelayOperation,
        source: io::Error,
    },
    Accounting(MetricsError),
}

/// Relay failure together with traffic successfully handled before it stopped.
#[derive(Debug)]
pub struct RelayFailure {
    pub kind: RelayFailureKind,
    pub stats: RelayStats,
}

impl fmt::Display for RelayFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            RelayFailureKind::Connect(source) => {
                write!(formatter, "could not connect upstream: {source}")
            }
            RelayFailureKind::ConnectTimeout(duration) => {
                write!(
                    formatter,
                    "upstream connection timed out after {duration:?}"
                )
            }
            RelayFailureKind::ProxyProtocol(source) => {
                write!(formatter, "TCP relay PROXY protocol failed: {source}")
            }
            RelayFailureKind::IdleTimeout(duration) => {
                write!(formatter, "TCP relay was idle for {duration:?}")
            }
            RelayFailureKind::LifetimeTimeout(duration) => {
                write!(formatter, "TCP relay reached its {duration:?} lifetime")
            }
            RelayFailureKind::Cancelled => formatter.write_str("TCP relay was cancelled"),
            RelayFailureKind::Io {
                direction,
                operation,
                source,
            } => write!(
                formatter,
                "TCP relay {operation} failed from {direction}: {source}"
            ),
            RelayFailureKind::Accounting(source) => {
                write!(
                    formatter,
                    "could not account for TCP relay traffic: {source}"
                )
            }
        }
    }
}

impl Error for RelayFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            RelayFailureKind::Connect(source) | RelayFailureKind::Io { source, .. } => Some(source),
            RelayFailureKind::ProxyProtocol(source) => Some(source),
            RelayFailureKind::Accounting(source) => Some(source),
            RelayFailureKind::ConnectTimeout(_)
            | RelayFailureKind::IdleTimeout(_)
            | RelayFailureKind::LifetimeTimeout(_)
            | RelayFailureKind::Cancelled => None,
        }
    }
}

impl TcpRelayResult {
    pub(crate) const fn from_failure_kind(kind: &RelayFailureKind) -> Self {
        match kind {
            RelayFailureKind::Connect(_) => Self::ConnectError,
            RelayFailureKind::ConnectTimeout(_) => Self::ConnectTimeout,
            RelayFailureKind::ProxyProtocol(_) => Self::ProxyProtocolError,
            RelayFailureKind::IdleTimeout(_) => Self::IdleTimeout,
            RelayFailureKind::LifetimeTimeout(_) => Self::LifetimeTimeout,
            RelayFailureKind::Cancelled => Self::Cancelled,
            RelayFailureKind::Io { .. } => Self::IoError,
            RelayFailureKind::Accounting(_) => Self::AccountingError,
        }
    }
}

/// Selected upstream lease and policy for a raw TCP relay.
pub struct TcpRelayCore {
    upstream: EndpointLease,
    policy: RelayPolicy,
    proxy_protocol: Option<(ProxyProtocolPolicy, Option<std::net::SocketAddr>)>,
}

impl TcpRelayCore {
    #[must_use]
    pub const fn new(upstream: EndpointLease, policy: RelayPolicy) -> Self {
        Self {
            upstream,
            policy,
            proxy_protocol: None,
        }
    }

    #[must_use]
    pub fn with_proxy_protocol(
        mut self,
        policy: Option<ProxyProtocolPolicy>,
        client_address: Option<std::net::SocketAddr>,
    ) -> Self {
        self.proxy_protocol = policy.map(|policy| (policy, client_address));
        self
    }

    /// Connects to the configured upstream and relays the Pingora stream.
    ///
    /// # Errors
    ///
    /// Returns a failure when connection establishment, transport I/O, accounting, a configured
    /// timeout, or shutdown prevents both directions from completing normally.
    pub async fn relay<D>(
        self,
        downstream: D,
        connection: &ConnectionGuard,
        mut shutdown: ShutdownWatch,
    ) -> Result<RelayStats, RelayFailure>
    where
        D: AsyncRead + AsyncWrite + Unpin,
    {
        let started_at = Instant::now();
        let upstream = tokio::select! {
            biased;
            () = wait_for_shutdown(&mut shutdown) => Err(failure(RelayFailureKind::Cancelled, RelayStats::default())),
            result = timeout(
                self.policy.connect,
                connect_upstream(&self.upstream),
            ) => {
                match result {
                    Ok(Ok(upstream)) => {
                        self.upstream.record_passive_success();
                        Ok(upstream)
                    }
                    Ok(Err(source)) => {
                        Err(failure(
                            RelayFailureKind::Connect(source),
                            RelayStats::default(),
                        ))
                    }
                    Err(_) => {
                        Err(failure(
                            RelayFailureKind::ConnectTimeout(self.policy.connect),
                            RelayStats::default(),
                        ))
                    }
                }
            }
        };

        let result = match upstream {
            Ok(mut upstream) => {
                if let Err(error) = send_proxy_header(
                    &mut upstream.stream,
                    self.proxy_protocol,
                    upstream.address,
                    &mut shutdown,
                )
                .await
                {
                    let _ = connection.record_proxy_protocol(error.result());
                    Err(failure(
                        RelayFailureKind::ProxyProtocol(error),
                        RelayStats::default(),
                    ))
                } else {
                    if self.proxy_protocol.is_some() {
                        let _ = connection.record_proxy_protocol(ProxyProtocolResult::Sent);
                    }
                    relay_streams(
                        downstream,
                        upstream.stream,
                        connection,
                        shutdown,
                        self.policy,
                    )
                    .await
                }
            }
            Err(failure) => Err(failure),
        };
        if let Err(failure) = &result {
            match &failure.kind {
                RelayFailureKind::Connect(_) => {
                    self.upstream
                        .record_passive_failure(HealthFailure::ConnectFailed);
                }
                RelayFailureKind::ConnectTimeout(_) => {
                    self.upstream.record_passive_failure(HealthFailure::Timeout);
                }
                RelayFailureKind::IdleTimeout(_)
                | RelayFailureKind::LifetimeTimeout(_)
                | RelayFailureKind::Cancelled
                | RelayFailureKind::Io { .. }
                | RelayFailureKind::ProxyProtocol(_)
                | RelayFailureKind::Accounting(_) => {}
            }
        }
        let category = result.as_ref().map_or_else(
            |failure| TcpRelayResult::from_failure_kind(&failure.kind),
            |_| TcpRelayResult::Success,
        );
        if let Err(error) = connection.record_tcp_relay(category, started_at.elapsed()) {
            warn!("could not account for TCP relay metrics: {error}");
        }
        result
    }
}

/// Waits for one upstream capacity lease without allowing shutdown to strand the caller.
pub async fn select_upstream_with_shutdown(
    service: &L4ServicePlan,
    shutdown: &ShutdownWatch,
) -> Option<EndpointLease> {
    let mut shutdown = shutdown.clone();
    tokio::select! {
        biased;
        () = wait_for_shutdown(&mut shutdown) => None,
        upstream = service.select_wait() => upstream,
    }
}

struct ConnectedUpstream {
    stream: Stream,
    address: Option<std::net::SocketAddr>,
}

async fn connect_upstream(upstream: &EndpointLease) -> io::Result<ConnectedUpstream> {
    match upstream.endpoint() {
        RuntimeEndpoint::Socket { address } => {
            let stream = TcpStream::connect(address).await?;
            Ok(ConnectedUpstream {
                stream: Box::new(pingora::protocols::l4::stream::Stream::from(stream)),
                address: Some(*address),
            })
        }
        RuntimeEndpoint::Dns { .. } => {
            let addresses = upstream.resolve_addresses().await?;
            let (stream, address) = connect_addresses(&addresses).await?;
            Ok(ConnectedUpstream {
                stream: Box::new(pingora::protocols::l4::stream::Stream::from(stream)),
                address: Some(address),
            })
        }
        #[cfg(unix)]
        RuntimeEndpoint::Unix { path } => {
            let stream = UnixStream::connect(path).await?;
            Ok(ConnectedUpstream {
                stream: Box::new(pingora::protocols::l4::stream::Stream::from(stream)),
                address: None,
            })
        }
        #[cfg(not(unix))]
        RuntimeEndpoint::Unix { path } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unix endpoint `{}` is unsupported", path.display()),
        )),
    }
}

async fn connect_addresses(
    addresses: &[std::net::SocketAddr],
) -> io::Result<(TcpStream, std::net::SocketAddr)> {
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok((stream, *address)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("resolved endpoint address sets are nonempty"))
}

async fn send_proxy_header(
    upstream: &mut Stream,
    propagation: Option<(ProxyProtocolPolicy, Option<std::net::SocketAddr>)>,
    destination: Option<std::net::SocketAddr>,
    shutdown: &mut ShutdownWatch,
) -> Result<(), ProxyProtocolError> {
    let Some((policy, source)) = propagation else {
        return Ok(());
    };
    let source =
        source.ok_or_else(|| ProxyProtocolError::new(ProxyProtocolErrorKind::ProtocolMismatch))?;
    let destination = destination
        .ok_or_else(|| ProxyProtocolError::new(ProxyProtocolErrorKind::ProtocolMismatch))?;
    let header = encode_header(
        policy.version,
        ProxyProtocolTransport::Stream,
        source,
        destination,
    )?;
    tokio::select! {
        biased;
        () = wait_for_shutdown(shutdown) => Err(ProxyProtocolError::new(ProxyProtocolErrorKind::Cancelled)),
        result = timeout(Duration::from_millis(policy.timeout_ms), upstream.write_all(&header)) => match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(ProxyProtocolError::io(error)),
            Err(_) => Err(ProxyProtocolError::new(ProxyProtocolErrorKind::Timeout)),
        },
    }
}

/// Relays two established asynchronous streams until both directions reach EOF.
///
/// A fixed buffer is retained per direction. Each read must be fully written before another read
/// in that direction, providing bounded memory use and transport backpressure. EOF in one direction
/// shuts down only the opposite write half; the reverse direction remains active.
///
/// Both directions, the shutdown wait and the lifetime and idle timers are created once and polled for as long as
/// the relay runs: a payload transfer does not set up a wait or a timer.
///
/// # Errors
///
/// Returns a failure when transport I/O, accounting, a configured idle/lifetime timeout, or
/// shutdown prevents both directions from completing normally. The failure contains traffic
/// successfully handled before the error.
pub async fn relay_streams<D, U>(
    downstream: D,
    upstream: U,
    connection: &ConnectionGuard,
    mut shutdown: ShutdownWatch,
    policy: RelayPolicy,
) -> Result<RelayStats, RelayFailure>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut downstream_reader, mut downstream_writer) = split(downstream);
    let (mut upstream_reader, mut upstream_writer) = split(upstream);
    let mut client_buffer = vec![0; RELAY_BUFFER_SIZE];
    let mut upstream_buffer = vec![0; RELAY_BUFFER_SIZE];
    let started_at = Instant::now();
    let accounting = RelayAccounting::new(connection, started_at);

    let directions = async {
        tokio::try_join!(
            copy_direction(
                RelayDirection::ClientToUpstream,
                &mut downstream_reader,
                &mut upstream_writer,
                &mut client_buffer,
                &accounting,
            ),
            copy_direction(
                RelayDirection::UpstreamToClient,
                &mut upstream_reader,
                &mut downstream_writer,
                &mut upstream_buffer,
                &accounting,
            ),
        )
    };
    let cancelled = wait_for_shutdown(&mut shutdown);
    let lifetime = sleep_until(started_at + policy.lifetime.unwrap_or_default());
    let idle = sleep_until(started_at + policy.idle.unwrap_or_default());
    tokio::pin!(directions, cancelled, lifetime, idle);

    loop {
        tokio::select! {
            biased;
            () = &mut cancelled => return Err(failure(RelayFailureKind::Cancelled, accounting.stats())),
            () = &mut lifetime, if policy.lifetime.is_some() => {
                let duration = policy.lifetime.unwrap_or_default();
                return Err(failure(RelayFailureKind::LifetimeTimeout(duration), accounting.stats()));
            }
            result = &mut directions => {
                return match result {
                    Ok(_) => Ok(accounting.stats()),
                    Err(error) => Err(failure(error.into_kind(), accounting.stats())),
                };
            }
            () = &mut idle, if policy.idle.is_some() => {
                // The timer is armed from the last payload transfer seen when it was armed: it is only moved when
                // it fires and there was more traffic since, so a transfer never touches it.
                let duration = policy.idle.unwrap_or_default();
                let deadline = accounting.last_io() + duration;
                if deadline <= Instant::now() {
                    return Err(failure(RelayFailureKind::IdleTimeout(duration), accounting.stats()));
                }
                idle.as_mut().reset(deadline);
            }
        }
    }
}

/// The traffic counters of one relay, shared by its two directions. Each counter has one writer (a direction), and
/// everything runs in one task: the atomics are there for the `Send` bound only, and relaxed accesses are plain ones.
struct RelayAccounting<'a> {
    connection: &'a ConnectionGuard,
    started_at: Instant,
    received: AtomicU64,
    sent: AtomicU64,
    /// Nanoseconds from `started_at` to the last payload read or write.
    last_io: AtomicU64,
}

impl<'a> RelayAccounting<'a> {
    fn new(connection: &'a ConnectionGuard, started_at: Instant) -> Self {
        Self {
            connection,
            started_at,
            received: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            last_io: AtomicU64::new(0),
        }
    }

    fn stats(&self) -> RelayStats {
        RelayStats {
            bytes_received: self.received.load(Ordering::Relaxed),
            bytes_sent: self.sent.load(Ordering::Relaxed),
        }
    }

    fn last_io(&self) -> Instant {
        self.started_at + Duration::from_nanos(self.last_io.load(Ordering::Relaxed))
    }

    fn touch(&self) {
        let elapsed = u64::try_from(self.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_io.store(elapsed, Ordering::Relaxed);
    }

    /// Bytes read from a side; the client's are the relay's received bytes.
    fn read(&self, direction: RelayDirection, bytes: usize) -> Result<(), DirectionFailure> {
        if direction == RelayDirection::ClientToUpstream {
            let bytes = bytes_to_u64(bytes);
            self.connection.record_bytes_received(bytes)?;
            add_traffic(&self.received, bytes);
        }
        self.touch();
        Ok(())
    }

    /// Bytes written to a side; the client's are the relay's sent bytes.
    fn written(&self, direction: RelayDirection, bytes: usize) -> Result<(), DirectionFailure> {
        if direction == RelayDirection::UpstreamToClient {
            let bytes = bytes_to_u64(bytes);
            self.connection.record_bytes_sent(bytes)?;
            add_traffic(&self.sent, bytes);
        }
        self.touch();
        Ok(())
    }
}

fn add_traffic(total: &AtomicU64, bytes: u64) {
    let sum = total
        .load(Ordering::Relaxed)
        .checked_add(bytes)
        .expect("connection metrics overflow before relay-local statistics");
    total.store(sum, Ordering::Relaxed);
}

const fn bytes_to_u64(bytes: usize) -> u64 {
    bytes as u64
}

fn failure(kind: RelayFailureKind, stats: RelayStats) -> RelayFailure {
    RelayFailure { kind, stats }
}

async fn wait_for_shutdown(shutdown: &mut ShutdownWatch) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

/// Copies one direction until the reader reaches EOF, then shuts the writer down.
async fn copy_direction<R, W>(
    direction: RelayDirection,
    reader: &mut R,
    writer: &mut W,
    buffer: &mut Vec<u8>,
    accounting: &RelayAccounting<'_>,
) -> Result<(), DirectionFailure>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let read = reader
            .read(buffer)
            .await
            .map_err(|source| DirectionFailure::io(direction, RelayOperation::Read, source))?;
        if read == 0 {
            return writer.shutdown().await.map_err(|source| {
                DirectionFailure::io(direction, RelayOperation::Shutdown, source)
            });
        }
        accounting.read(direction, read)?;
        let full = read == buffer.len();
        let mut written = 0;
        while written < read {
            let bytes = writer
                .write(&buffer[written..read])
                .await
                .map_err(|source| DirectionFailure::io(direction, RelayOperation::Write, source))?;
            if bytes == 0 {
                let source = io::Error::new(
                    io::ErrorKind::WriteZero,
                    "relay writer accepted no buffered bytes",
                );
                return Err(DirectionFailure::io(
                    direction,
                    RelayOperation::Write,
                    source,
                ));
            }
            writer
                .flush()
                .await
                .map_err(|source| DirectionFailure::io(direction, RelayOperation::Write, source))?;
            written += bytes;
            accounting.written(direction, bytes)?;
        }
        if full && buffer.len() < RELAY_BULK_BUFFER_SIZE {
            buffer.resize(RELAY_BULK_BUFFER_SIZE, 0);
        }
    }
}

enum DirectionFailure {
    Io {
        direction: RelayDirection,
        operation: RelayOperation,
        source: io::Error,
    },
    Accounting(MetricsError),
}

impl DirectionFailure {
    fn io(direction: RelayDirection, operation: RelayOperation, source: io::Error) -> Self {
        Self::Io {
            direction,
            operation,
            source,
        }
    }

    fn into_kind(self) -> RelayFailureKind {
        match self {
            Self::Io {
                direction,
                operation,
                source,
            } => RelayFailureKind::Io {
                direction,
                operation,
                source,
            },
            Self::Accounting(source) => RelayFailureKind::Accounting(source),
        }
    }
}

impl From<MetricsError> for DirectionFailure {
    fn from(source: MetricsError) -> Self {
        Self::Accounting(source)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oxiroute_config::{UdpPolicy, UpstreamAlgorithm};
    use tokio::{sync::watch, time::timeout};

    use crate::RoundRobinPool;

    use super::*;

    #[tokio::test]
    async fn tcp_connection_falls_back_to_the_second_resolved_address() {
        let first = crate::test_ports::unused_tcp_address();
        let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::from((
            [127, 0, 0, 2],
            first.port(),
        )))
        .await
        .expect("second address listener");
        let second = listener.local_addr().expect("second address");

        let (connection, address) = connect_addresses(&[first, second])
            .await
            .expect("second address connection");
        let (_accepted, _) = listener.accept().await.expect("second address accept");

        assert_eq!(connection.peer_addr().expect("connected peer"), second);
        assert_eq!(address, second);
    }

    #[tokio::test]
    async fn upstream_capacity_wait_is_cancelled_by_shutdown() {
        let pool = Arc::new(
            RoundRobinPool::new_named_servers(
                "tcp-wait".into(),
                [crate::routing::RuntimeServer {
                    name: "upstream".into(),
                    endpoint: RuntimeEndpoint::Socket {
                        address: "127.0.0.1:1".parse().expect("upstream address"),
                    },
                    max_connections: Some(1),
                    pinned_addresses: None,
                    protected_addresses: Arc::from([]),
                }],
                UpstreamAlgorithm::First,
                None,
                Some(Duration::from_secs(30)),
            )
            .expect("capacity-limited pool"),
        );
        let held = pool.select().expect("initial upstream capacity");
        let service = L4ServicePlan::new(
            RelayPolicy::new(Duration::from_secs(1)),
            Arc::clone(&pool),
            None,
            UdpPolicy::default(),
        );
        let (shutdown_tx, shutdown) = watch::channel(false);
        let waiter =
            tokio::spawn(async move { select_upstream_with_shutdown(&service, &shutdown).await });

        timeout(Duration::from_secs(1), async {
            while pool.health_snapshot().queued != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capacity waiter registration");
        shutdown_tx.send(true).expect("shutdown signal");
        assert!(
            timeout(Duration::from_secs(1), waiter)
                .await
                .expect("shutdown waiter completion")
                .expect("shutdown waiter task")
                .is_none()
        );
        drop(held);
        assert_eq!(pool.health_snapshot().queued, 0);
    }

    #[test]
    fn relay_failures_use_bounded_result_categories() {
        assert_eq!(
            TcpRelayResult::from_failure_kind(&RelayFailureKind::Connect(io::Error::other("down"))),
            TcpRelayResult::ConnectError
        );
        assert_eq!(
            TcpRelayResult::from_failure_kind(&RelayFailureKind::IdleTimeout(Duration::from_secs(
                1
            ))),
            TcpRelayResult::IdleTimeout
        );
        assert_eq!(
            TcpRelayResult::from_failure_kind(&RelayFailureKind::Cancelled),
            TcpRelayResult::Cancelled
        );
    }
}
