use std::{error::Error, fmt, future::pending, io, time::Duration};

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

/// Memory used for each direction of a relayed connection.
pub const RELAY_BUFFER_SIZE: usize = 16 * 1024;

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
                    Ok(Ok(upstream)) => Ok(upstream),
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
    let mut client_to_upstream = DirectionState::new();
    let mut upstream_to_client = DirectionState::new();
    let started_at = Instant::now();
    let mut idle_deadline = policy.idle.map(|duration| started_at + duration);
    let lifetime_deadline = policy.lifetime.map(|duration| started_at + duration);
    let mut stats = RelayStats::default();

    loop {
        if client_to_upstream.is_finished() && upstream_to_client.is_finished() {
            return Ok(stats);
        }

        let poll_client_to_upstream = !client_to_upstream.is_finished();
        let poll_upstream_to_client = !upstream_to_client.is_finished();
        let next_progress = async {
            tokio::select! {
                result = client_to_upstream.step(
                    &mut downstream_reader,
                    &mut upstream_writer,
                ), if poll_client_to_upstream => (RelayDirection::ClientToUpstream, result),
                result = upstream_to_client.step(
                    &mut upstream_reader,
                    &mut downstream_writer,
                ), if poll_upstream_to_client => (RelayDirection::UpstreamToClient, result),
            }
        };

        let (direction, progress) = tokio::select! {
            biased;
            () = wait_for_shutdown(&mut shutdown) => {
                return Err(failure(RelayFailureKind::Cancelled, stats));
            }
            duration = wait_for_deadline(lifetime_deadline, policy.lifetime) => {
                return Err(failure(RelayFailureKind::LifetimeTimeout(duration), stats));
            }
            progress = next_progress => progress,
            duration = wait_for_deadline(idle_deadline, policy.idle) => {
                return Err(failure(RelayFailureKind::IdleTimeout(duration), stats));
            }
        };

        let progress = progress.map_err(|error| {
            failure(
                RelayFailureKind::Io {
                    direction,
                    operation: error.operation,
                    source: error.source,
                },
                stats,
            )
        })?;
        if progress.is_payload_io() {
            account_progress(connection, &mut stats, direction, progress)
                .map_err(|source| failure(RelayFailureKind::Accounting(source), stats))?;
            idle_deadline = policy.idle.map(|duration| Instant::now() + duration);
        }
    }
}

fn account_progress(
    connection: &ConnectionGuard,
    stats: &mut RelayStats,
    direction: RelayDirection,
    progress: DirectionProgress,
) -> Result<(), MetricsError> {
    match (direction, progress) {
        (RelayDirection::ClientToUpstream, DirectionProgress::Read(bytes)) => {
            let bytes = bytes_to_u64(bytes);
            connection.record_bytes_received(bytes)?;
            stats.bytes_received = stats
                .bytes_received
                .checked_add(bytes)
                .expect("connection metrics overflow before relay-local statistics");
        }
        (RelayDirection::UpstreamToClient, DirectionProgress::Written(bytes)) => {
            let bytes = bytes_to_u64(bytes);
            connection.record_bytes_sent(bytes)?;
            stats.bytes_sent = stats
                .bytes_sent
                .checked_add(bytes)
                .expect("connection metrics overflow before relay-local statistics");
        }
        _ => {}
    }
    Ok(())
}

const fn bytes_to_u64(bytes: usize) -> u64 {
    bytes as u64
}

fn failure(kind: RelayFailureKind, stats: RelayStats) -> RelayFailure {
    RelayFailure { kind, stats }
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
        let listener = tokio::net::TcpListener::bind("127.0.0.2:0")
            .await
            .expect("second address listener");
        let second = listener.local_addr().expect("second address");
        let first = std::net::SocketAddr::from(([127, 0, 0, 1], second.port()));
        drop(
            tokio::net::TcpListener::bind(first)
                .await
                .expect("first address must be unused"),
        );

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

async fn wait_for_deadline(
    deadline: Option<Instant>,
    configured_duration: Option<Duration>,
) -> Duration {
    match (deadline, configured_duration) {
        (Some(deadline), Some(duration)) => {
            sleep_until(deadline).await;
            duration
        }
        _ => pending().await,
    }
}

struct DirectionState {
    buffer: Box<[u8; RELAY_BUFFER_SIZE]>,
    read: usize,
    written: usize,
    finished: bool,
}

impl DirectionState {
    fn new() -> Self {
        Self {
            buffer: Box::new([0; RELAY_BUFFER_SIZE]),
            read: 0,
            written: 0,
            finished: false,
        }
    }

    const fn is_finished(&self) -> bool {
        self.finished
    }

    async fn step<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> Result<DirectionProgress, DirectionIoError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        if self.written < self.read {
            let bytes = writer
                .write(&self.buffer[self.written..self.read])
                .await
                .map_err(|source| DirectionIoError {
                    operation: RelayOperation::Write,
                    source,
                })?;
            if bytes == 0 {
                return Err(DirectionIoError {
                    operation: RelayOperation::Write,
                    source: io::Error::new(
                        io::ErrorKind::WriteZero,
                        "relay writer accepted no buffered bytes",
                    ),
                });
            }
            writer.flush().await.map_err(|source| DirectionIoError {
                operation: RelayOperation::Write,
                source,
            })?;
            self.written += bytes;
            return Ok(DirectionProgress::Written(bytes));
        }

        let bytes = reader
            .read(self.buffer.as_mut())
            .await
            .map_err(|source| DirectionIoError {
                operation: RelayOperation::Read,
                source,
            })?;
        if bytes > 0 {
            self.read = bytes;
            self.written = 0;
            return Ok(DirectionProgress::Read(bytes));
        }

        writer.shutdown().await.map_err(|source| DirectionIoError {
            operation: RelayOperation::Shutdown,
            source,
        })?;
        self.finished = true;
        Ok(DirectionProgress::Finished)
    }
}

#[derive(Clone, Copy)]
enum DirectionProgress {
    Read(usize),
    Written(usize),
    Finished,
}

impl DirectionProgress {
    const fn is_payload_io(self) -> bool {
        matches!(self, Self::Read(_) | Self::Written(_))
    }
}

struct DirectionIoError {
    operation: RelayOperation,
    source: io::Error,
}
