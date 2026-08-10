use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use bytes::{Buf as _, Bytes};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf},
    sync::{Notify, mpsc},
    time::{Instant, sleep_until},
};

const MAX_TUNNEL_BUFFER_SIZE: usize = 1024 * 1024;

/// An I/O stream that replays bytes consumed beyond an HTTP header before reading the socket.
#[derive(Debug)]
pub struct OverreadIo<S> {
    prefix: Bytes,
    inner: S,
}

/// The per-stream operations required to relay a classic HTTP/2 CONNECT tunnel.
///
/// Implementations must release protocol receive capacity after frame bytes are read and before
/// returning them. `wait_closed` is used after the request half closes to observe a stream reset
/// while the upstream continues sending data.
#[async_trait]
pub trait H2TunnelStream: Send {
    async fn recv_data(&mut self) -> io::Result<Option<Bytes>>;

    async fn send_data(&mut self, data: Bytes, end: bool) -> io::Result<()>;

    async fn wait_closed(&mut self) -> io::Result<()>;

    async fn reset(&mut self);
}

impl<S> OverreadIo<S> {
    #[must_use]
    pub fn new(inner: S, prefix: Bytes) -> Self {
        Self { prefix, inner }
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for OverreadIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.prefix.has_remaining() && buffer.remaining() > 0 {
            let length = self.prefix.remaining().min(buffer.remaining());
            buffer.put_slice(&self.prefix[..length]);
            self.prefix.advance(length);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for OverreadIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TunnelLimits {
    pub max_bytes_per_direction: u64,
    pub idle_timeout: Duration,
    pub lifetime_timeout: Duration,
    pub buffer_size: usize,
}

impl Default for TunnelLimits {
    fn default() -> Self {
        Self {
            max_bytes_per_direction: 64 * 1024 * 1024,
            idle_timeout: Duration::from_mins(1),
            lifetime_timeout: Duration::from_hours(1),
            buffer_size: 16 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TunnelConfigError {
    #[error("tunnel byte limit must be nonzero")]
    ZeroByteLimit,
    #[error("tunnel idle timeout must be nonzero")]
    ZeroIdleTimeout,
    #[error("tunnel lifetime must be nonzero")]
    ZeroLifetimeTimeout,
    #[error("tunnel buffer size must be between 1 and 1048576 bytes")]
    InvalidBufferSize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TunnelStats {
    pub left_to_right: u64,
    pub right_to_left: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunnelEnd {
    Eof,
    ByteLimitLeftToRight,
    ByteLimitRightToLeft,
    IdleTimeout,
    LifetimeTimeout,
}

impl TunnelEnd {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::ByteLimitLeftToRight => "byte_limit_left_to_right",
            Self::ByteLimitRightToLeft => "byte_limit_right_to_left",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeTimeout => "lifetime_timeout",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunnelOutcomeKind {
    Eof,
    ByteLimitLeftToRight,
    ByteLimitRightToLeft,
    IdleTimeout,
    LifetimeTimeout,
    IoError,
}

impl TunnelOutcomeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::ByteLimitLeftToRight => "byte_limit_left_to_right",
            Self::ByteLimitRightToLeft => "byte_limit_right_to_left",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeTimeout => "lifetime_timeout",
            Self::IoError => "io_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelOutcome {
    #[error("tunnel ended: {end:?}")]
    Ended { end: TunnelEnd, stats: TunnelStats },
    #[error("tunnel I/O failed after {stats:?}: {source}")]
    Io {
        stats: TunnelStats,
        #[source]
        source: io::Error,
    },
}

impl TunnelOutcome {
    #[must_use]
    pub const fn kind(&self) -> TunnelOutcomeKind {
        match self {
            Self::Ended { end, .. } => match end {
                TunnelEnd::Eof => TunnelOutcomeKind::Eof,
                TunnelEnd::ByteLimitLeftToRight => TunnelOutcomeKind::ByteLimitLeftToRight,
                TunnelEnd::ByteLimitRightToLeft => TunnelOutcomeKind::ByteLimitRightToLeft,
                TunnelEnd::IdleTimeout => TunnelOutcomeKind::IdleTimeout,
                TunnelEnd::LifetimeTimeout => TunnelOutcomeKind::LifetimeTimeout,
            },
            Self::Io { .. } => TunnelOutcomeKind::IoError,
        }
    }

    #[must_use]
    pub const fn stats(&self) -> TunnelStats {
        match self {
            Self::Ended { stats, .. } | Self::Io { stats, .. } => *stats,
        }
    }
}

pub struct BoundedTunnel {
    limits: TunnelLimits,
}

struct TunnelCoordinator {
    limits: TunnelLimits,
    left_to_right: Arc<AtomicU64>,
    right_to_left: Arc<AtomicU64>,
    lifetime_deadline: Instant,
    idle_deadline: Instant,
}

impl TunnelCoordinator {
    fn new(
        limits: TunnelLimits,
        left_to_right: Arc<AtomicU64>,
        right_to_left: Arc<AtomicU64>,
    ) -> Self {
        let started = Instant::now();
        Self {
            limits,
            left_to_right,
            right_to_left,
            lifetime_deadline: started + limits.lifetime_timeout,
            idle_deadline: started + limits.idle_timeout,
        }
    }

    fn progress(&mut self) {
        self.idle_deadline = Instant::now() + self.limits.idle_timeout;
    }

    fn stats(&self) -> TunnelStats {
        TunnelStats {
            left_to_right: self.left_to_right.load(Ordering::Relaxed),
            right_to_left: self.right_to_left.load(Ordering::Relaxed),
        }
    }

    fn ended(&self, end: TunnelEnd) -> TunnelOutcome {
        TunnelOutcome::Ended {
            end,
            stats: self.stats(),
        }
    }

    fn io(&self, source: io::Error) -> TunnelOutcome {
        TunnelOutcome::Io {
            stats: self.stats(),
            source,
        }
    }
}

impl BoundedTunnel {
    /// Creates a relay with finite byte, time, and allocation bounds.
    ///
    /// # Errors
    ///
    /// Returns [`TunnelConfigError`] when any limit is zero or the buffer exceeds one MiB.
    pub fn new(limits: TunnelLimits) -> Result<Self, TunnelConfigError> {
        if limits.max_bytes_per_direction == 0 {
            return Err(TunnelConfigError::ZeroByteLimit);
        }
        if limits.idle_timeout.is_zero() {
            return Err(TunnelConfigError::ZeroIdleTimeout);
        }
        if limits.lifetime_timeout.is_zero() {
            return Err(TunnelConfigError::ZeroLifetimeTimeout);
        }
        if limits.buffer_size == 0 || limits.buffer_size > MAX_TUNNEL_BUFFER_SIZE {
            return Err(TunnelConfigError::InvalidBufferSize);
        }
        Ok(Self { limits })
    }

    pub async fn relay<L, R>(&self, left: L, right: R) -> TunnelOutcome
    where
        L: AsyncRead + AsyncWrite + Unpin,
        R: AsyncRead + AsyncWrite + Unpin,
    {
        let left_to_right = Arc::new(AtomicU64::new(0));
        let right_to_left = Arc::new(AtomicU64::new(0));
        let (activity_tx, mut activity_rx) = mpsc::channel(1);
        let left_limit = Arc::new(Notify::new());
        let right_limit = Arc::new(Notify::new());
        let mut left = LimitedIo::new(
            left,
            self.limits,
            Arc::clone(&right_to_left),
            activity_tx.clone(),
            Arc::clone(&right_limit),
        );
        let mut right = LimitedIo::new(
            right,
            self.limits,
            Arc::clone(&left_to_right),
            activity_tx,
            Arc::clone(&left_limit),
        );
        let copy = tokio::io::copy_bidirectional_with_sizes(
            &mut left,
            &mut right,
            self.limits.buffer_size,
            self.limits.buffer_size,
        );
        tokio::pin!(copy);
        let mut coordinator = TunnelCoordinator::new(self.limits, left_to_right, right_to_left);
        loop {
            tokio::select! {
                result = &mut copy => {
                    return match result {
                        Ok(_) => coordinator.ended(TunnelEnd::Eof),
                        Err(source) => coordinator.io(source),
                    };
                }
                () = left_limit.notified() => {
                    return coordinator.ended(TunnelEnd::ByteLimitLeftToRight);
                }
                () = right_limit.notified() => {
                    return coordinator.ended(TunnelEnd::ByteLimitRightToLeft);
                }
                () = sleep_until(coordinator.lifetime_deadline) => {
                    return coordinator.ended(TunnelEnd::LifetimeTimeout);
                }
                () = sleep_until(coordinator.idle_deadline) => {
                    return coordinator.ended(TunnelEnd::IdleTimeout);
                }
                Some(()) = activity_rx.recv() => {
                    coordinator.progress();
                }
            }
        }
    }

    /// Relays an HTTP/3 request stream to a byte-stream upstream using DATA frames.
    ///
    /// A QUIC FIN in either direction is propagated as a half-close. Stream resets, framing
    /// failures, and upstream I/O failures return [`TunnelOutcome::Io`].
    pub async fn relay_h3<S, R>(
        &self,
        downstream: h3::server::RequestStream<S, Bytes>,
        mut upstream: R,
    ) -> TunnelOutcome
    where
        S: h3::quic::BidiStream<Bytes>,
        R: AsyncRead + AsyncWrite + Unpin,
    {
        let left_to_right = Arc::new(AtomicU64::new(0));
        let right_to_left = Arc::new(AtomicU64::new(0));
        let (activity_tx, activity_rx) = mpsc::channel(1);
        let (downstream_send, downstream_recv) = downstream.split();
        let (upstream_reader, upstream_writer) = tokio::io::split(&mut upstream);
        let left_pump = pump_h3_to_io(
            downstream_recv,
            upstream_writer,
            self.limits,
            Arc::clone(&left_to_right),
            activity_tx.clone(),
        );
        let right_pump = pump_io_to_h3(
            upstream_reader,
            downstream_send,
            self.limits,
            Arc::clone(&right_to_left),
            activity_tx,
        );
        coordinate(
            self.limits,
            left_pump,
            right_pump,
            left_to_right,
            right_to_left,
            activity_rx,
        )
        .await
    }

    /// Relays an HTTP/2 classic CONNECT stream to a byte-stream upstream using DATA frames.
    ///
    /// The stream adapter owns only one HTTP/2 stream, never the connection. Request DATA is
    /// consumed before upstream writes, response DATA is bounded by downstream flow control, and
    /// `END_STREAM` is propagated as a half-close in each direction.
    #[allow(clippy::too_many_lines)]
    pub async fn relay_h2<S, R>(&self, mut downstream: S, mut upstream: R) -> TunnelOutcome
    where
        S: H2TunnelStream,
        R: AsyncRead + AsyncWrite + Unpin,
    {
        let left_to_right = Arc::new(AtomicU64::new(0));
        let right_to_left = Arc::new(AtomicU64::new(0));
        let mut buffer = vec![0; self.limits.buffer_size];
        let mut coordinator = TunnelCoordinator::new(self.limits, left_to_right, right_to_left);
        let mut downstream_open = true;
        let mut upstream_read_open = true;
        let mut upstream_write_open = true;
        let mut response_open = true;

        loop {
            if !response_open && !upstream_write_open {
                return TunnelOutcome::Ended {
                    end: TunnelEnd::Eof,
                    stats: coordinator.stats(),
                };
            }

            let right_to_left_current = coordinator.right_to_left.load(Ordering::Relaxed);
            if right_to_left_current >= self.limits.max_bytes_per_direction {
                return h2_ended(
                    &mut downstream,
                    TunnelEnd::ByteLimitRightToLeft,
                    coordinator.stats(),
                )
                .await;
            }
            let read_limit = buffer.len().min(
                usize::try_from(self.limits.max_bytes_per_direction - right_to_left_current)
                    .unwrap_or(usize::MAX),
            );

            let event = if downstream_open {
                tokio::select! {
                    () = sleep_until(coordinator.lifetime_deadline) => H2RelayEvent::LifetimeTimeout,
                    () = sleep_until(coordinator.idle_deadline) => H2RelayEvent::IdleTimeout,
                    result = downstream.recv_data() => H2RelayEvent::DownstreamData(result),
                    result = upstream.read(&mut buffer[..read_limit]), if upstream_read_open => {
                        H2RelayEvent::UpstreamData(result)
                    }
                }
            } else {
                tokio::select! {
                    () = sleep_until(coordinator.lifetime_deadline) => H2RelayEvent::LifetimeTimeout,
                    () = sleep_until(coordinator.idle_deadline) => H2RelayEvent::IdleTimeout,
                    result = downstream.wait_closed() => H2RelayEvent::DownstreamClosed(result),
                    result = upstream.read(&mut buffer[..read_limit]), if upstream_read_open => {
                        H2RelayEvent::UpstreamData(result)
                    }
                }
            };

            match event {
                H2RelayEvent::LifetimeTimeout => {
                    return h2_ended(
                        &mut downstream,
                        TunnelEnd::LifetimeTimeout,
                        coordinator.stats(),
                    )
                    .await;
                }
                H2RelayEvent::IdleTimeout => {
                    return h2_ended(&mut downstream, TunnelEnd::IdleTimeout, coordinator.stats())
                        .await;
                }
                H2RelayEvent::DownstreamData(result) => {
                    let data = match result {
                        Ok(Some(data)) => data,
                        Ok(None) => {
                            downstream_open = false;
                            if upstream_write_open {
                                match shutdown_with_deadlines(
                                    &mut upstream,
                                    coordinator.lifetime_deadline,
                                    coordinator.idle_deadline,
                                )
                                .await
                                {
                                    Ok(()) => upstream_write_open = false,
                                    Err(error) => {
                                        return h2_operation_failure(
                                            &mut downstream,
                                            error,
                                            coordinator.stats(),
                                        )
                                        .await;
                                    }
                                }
                            }
                            continue;
                        }
                        Err(source) => {
                            return h2_io(&mut downstream, coordinator.stats(), source).await;
                        }
                    };
                    let left_to_right_current = coordinator.left_to_right.load(Ordering::Relaxed);
                    if left_to_right_current >= self.limits.max_bytes_per_direction {
                        return h2_ended(
                            &mut downstream,
                            TunnelEnd::ByteLimitLeftToRight,
                            coordinator.stats(),
                        )
                        .await;
                    }
                    let allowed = data.len().min(
                        usize::try_from(
                            self.limits.max_bytes_per_direction - left_to_right_current,
                        )
                        .unwrap_or(usize::MAX),
                    );
                    if allowed > 0 {
                        if let Err(error) = write_with_deadlines(
                            &mut upstream,
                            &data[..allowed],
                            coordinator.lifetime_deadline,
                            coordinator.idle_deadline,
                        )
                        .await
                        {
                            return h2_operation_failure(
                                &mut downstream,
                                error,
                                coordinator.stats(),
                            )
                            .await;
                        }
                        coordinator.left_to_right.fetch_add(
                            u64::try_from(allowed).unwrap_or(u64::MAX),
                            Ordering::Relaxed,
                        );
                        coordinator.progress();
                    }
                    if allowed < data.len()
                        || coordinator.left_to_right.load(Ordering::Relaxed)
                            >= self.limits.max_bytes_per_direction
                    {
                        return h2_ended(
                            &mut downstream,
                            TunnelEnd::ByteLimitLeftToRight,
                            coordinator.stats(),
                        )
                        .await;
                    }
                }
                H2RelayEvent::DownstreamClosed(result) => {
                    let source = match result {
                        Ok(()) => {
                            io::Error::new(io::ErrorKind::BrokenPipe, "HTTP/2 stream was reset")
                        }
                        Err(source) => source,
                    };
                    return h2_io(&mut downstream, coordinator.stats(), source).await;
                }
                H2RelayEvent::UpstreamData(result) => {
                    match relay_h2_upstream_read(
                        &mut downstream,
                        result,
                        &buffer,
                        &coordinator.right_to_left,
                        self.limits.max_bytes_per_direction,
                        coordinator.lifetime_deadline,
                        coordinator.idle_deadline,
                    )
                    .await
                    {
                        Ok(H2UpstreamEvent::Continue) => coordinator.progress(),
                        Ok(H2UpstreamEvent::Eof) => {
                            upstream_read_open = false;
                            response_open = false;
                        }
                        Ok(H2UpstreamEvent::Limit) => {
                            coordinator.progress();
                            return h2_ended(
                                &mut downstream,
                                TunnelEnd::ByteLimitRightToLeft,
                                coordinator.stats(),
                            )
                            .await;
                        }
                        Err(error) => {
                            return h2_operation_failure(
                                &mut downstream,
                                error,
                                coordinator.stats(),
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum OperationError {
    End(TunnelEnd),
    Io(io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum H2UpstreamEvent {
    Continue,
    Eof,
    Limit,
}

#[derive(Debug)]
enum H2RelayEvent {
    LifetimeTimeout,
    IdleTimeout,
    DownstreamData(io::Result<Option<Bytes>>),
    DownstreamClosed(io::Result<()>),
    UpstreamData(io::Result<usize>),
}

#[allow(clippy::too_many_arguments)]
async fn relay_h2_upstream_read<S>(
    downstream: &mut S,
    result: io::Result<usize>,
    buffer: &[u8],
    transferred: &AtomicU64,
    limit: u64,
    lifetime_deadline: Instant,
    idle_deadline: Instant,
) -> Result<H2UpstreamEvent, OperationError>
where
    S: H2TunnelStream,
{
    let length = result.map_err(OperationError::Io)?;
    if length == 0 {
        send_with_deadlines(
            downstream,
            Bytes::new(),
            true,
            lifetime_deadline,
            idle_deadline,
        )
        .await?;
        return Ok(H2UpstreamEvent::Eof);
    }
    let current = transferred.load(Ordering::Relaxed);
    let allowed = length.min(usize::try_from(limit - current).unwrap_or(usize::MAX));
    let end = allowed < length || current + u64::try_from(allowed).unwrap_or(u64::MAX) >= limit;
    send_with_deadlines(
        downstream,
        Bytes::copy_from_slice(&buffer[..allowed]),
        end,
        lifetime_deadline,
        idle_deadline,
    )
    .await?;
    transferred.fetch_add(
        u64::try_from(allowed).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    if end {
        Ok(H2UpstreamEvent::Limit)
    } else {
        Ok(H2UpstreamEvent::Continue)
    }
}

async fn write_with_deadlines<R>(
    writer: &mut R,
    data: &[u8],
    lifetime_deadline: Instant,
    idle_deadline: Instant,
) -> Result<(), OperationError>
where
    R: AsyncWrite + Unpin,
{
    let write = writer.write_all(data);
    tokio::pin!(write);
    tokio::select! {
        () = sleep_until(lifetime_deadline) => Err(OperationError::End(TunnelEnd::LifetimeTimeout)),
        () = sleep_until(idle_deadline) => Err(OperationError::End(TunnelEnd::IdleTimeout)),
        result = &mut write => result.map_err(OperationError::Io),
    }
}

async fn shutdown_with_deadlines<R>(
    writer: &mut R,
    lifetime_deadline: Instant,
    idle_deadline: Instant,
) -> Result<(), OperationError>
where
    R: AsyncWrite + Unpin,
{
    let shutdown = writer.shutdown();
    tokio::pin!(shutdown);
    tokio::select! {
        () = sleep_until(lifetime_deadline) => Err(OperationError::End(TunnelEnd::LifetimeTimeout)),
        () = sleep_until(idle_deadline) => Err(OperationError::End(TunnelEnd::IdleTimeout)),
        result = &mut shutdown => result.map_err(OperationError::Io),
    }
}

async fn send_with_deadlines<S>(
    downstream: &mut S,
    data: Bytes,
    end: bool,
    lifetime_deadline: Instant,
    idle_deadline: Instant,
) -> Result<(), OperationError>
where
    S: H2TunnelStream,
{
    let send = downstream.send_data(data, end);
    tokio::pin!(send);
    tokio::select! {
        () = sleep_until(lifetime_deadline) => Err(OperationError::End(TunnelEnd::LifetimeTimeout)),
        () = sleep_until(idle_deadline) => Err(OperationError::End(TunnelEnd::IdleTimeout)),
        result = &mut send => result.map_err(OperationError::Io),
    }
}

async fn h2_ended<S>(downstream: &mut S, end: TunnelEnd, stats: TunnelStats) -> TunnelOutcome
where
    S: H2TunnelStream,
{
    downstream.reset().await;
    TunnelOutcome::Ended { end, stats }
}

async fn h2_io<S>(downstream: &mut S, stats: TunnelStats, source: io::Error) -> TunnelOutcome
where
    S: H2TunnelStream,
{
    downstream.reset().await;
    TunnelOutcome::Io { stats, source }
}

async fn h2_operation_failure<S>(
    downstream: &mut S,
    error: OperationError,
    stats: TunnelStats,
) -> TunnelOutcome
where
    S: H2TunnelStream,
{
    match error {
        OperationError::End(end) => h2_ended(downstream, end, stats).await,
        OperationError::Io(source) => h2_io(downstream, stats, source).await,
    }
}

struct LimitedIo<T> {
    inner: T,
    remaining: u64,
    transferred: Arc<AtomicU64>,
    activity: mpsc::Sender<()>,
    limit: Arc<Notify>,
}

impl<T> LimitedIo<T> {
    fn new(
        inner: T,
        limits: TunnelLimits,
        transferred: Arc<AtomicU64>,
        activity: mpsc::Sender<()>,
        limit: Arc<Notify>,
    ) -> Self {
        Self {
            inner,
            remaining: limits.max_bytes_per_direction,
            transferred,
            activity,
            limit,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for LimitedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for LimitedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.remaining == 0 {
            return match Pin::new(&mut self.inner).poll_flush(context) {
                Poll::Ready(Ok(())) => {
                    self.limit.notify_one();
                    Poll::Pending
                }
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Pending => Poll::Pending,
            };
        }
        let length = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let result = Pin::new(&mut self.inner).poll_write(context, &buffer[..length]);
        if let Poll::Ready(Ok(written)) = result {
            let written = u64::try_from(written).unwrap_or(u64::MAX);
            self.remaining = self.remaining.saturating_sub(written);
            self.transferred.fetch_add(written, Ordering::Relaxed);
            if written > 0 {
                let _ = self.activity.try_send(());
            }
            Poll::Ready(Ok(usize::try_from(written).unwrap_or(usize::MAX)))
        } else {
            result
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(context);
        if matches!(result, Poll::Ready(Ok(()))) && self.remaining == 0 {
            self.limit.notify_one();
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

async fn coordinate<L, R>(
    limits: TunnelLimits,
    left_pump: L,
    right_pump: R,
    left_to_right: Arc<AtomicU64>,
    right_to_left: Arc<AtomicU64>,
    mut activity_rx: mpsc::Receiver<()>,
) -> TunnelOutcome
where
    L: Future<Output = io::Result<PumpEnd>>,
    R: Future<Output = io::Result<PumpEnd>>,
{
    let mut coordinator = TunnelCoordinator::new(limits, left_to_right, right_to_left);
    tokio::pin!(left_pump);
    tokio::pin!(right_pump);
    let mut left_end = None;
    let mut right_end = None;

    loop {
        if left_end == Some(PumpEnd::Eof) && right_end == Some(PumpEnd::Eof) {
            return coordinator.ended(TunnelEnd::Eof);
        }
        tokio::select! {
            () = sleep_until(coordinator.lifetime_deadline) => {
                return coordinator.ended(TunnelEnd::LifetimeTimeout);
            }
            () = sleep_until(coordinator.idle_deadline) => {
                return coordinator.ended(TunnelEnd::IdleTimeout);
            }
            Some(()) = activity_rx.recv() => {
                coordinator.progress();
            }
            result = &mut left_pump, if left_end.is_none() => {
                match result {
                    Ok(PumpEnd::Eof) => left_end = Some(PumpEnd::Eof),
                    Ok(PumpEnd::Limit) => {
                        return coordinator.ended(TunnelEnd::ByteLimitLeftToRight);
                    }
                    Err(source) => return coordinator.io(source),
                }
            }
            result = &mut right_pump, if right_end.is_none() => {
                match result {
                    Ok(PumpEnd::Eof) => right_end = Some(PumpEnd::Eof),
                    Ok(PumpEnd::Limit) => {
                        return coordinator.ended(TunnelEnd::ByteLimitRightToLeft);
                    }
                    Err(source) => return coordinator.io(source),
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PumpEnd {
    Eof,
    Limit,
}

async fn pump_h3_to_io<S, W>(
    mut reader: h3::server::RequestStream<S, Bytes>,
    mut writer: W,
    limits: TunnelLimits,
    transferred: Arc<AtomicU64>,
    activity: mpsc::Sender<()>,
) -> io::Result<PumpEnd>
where
    S: h3::quic::RecvStream,
    W: AsyncWrite + Unpin,
{
    loop {
        let Some(mut data) = reader.recv_data().await.map_err(h3_io_error)? else {
            writer.shutdown().await?;
            return Ok(PumpEnd::Eof);
        };
        while data.has_remaining() {
            let current = transferred.load(Ordering::Relaxed);
            if current == limits.max_bytes_per_direction {
                return Ok(PumpEnd::Limit);
            }
            let remaining = limits.max_bytes_per_direction - current;
            let available = data.chunk();
            let length = available
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            writer.write_all(&available[..length]).await?;
            data.advance(length);
            let length = u64::try_from(length).unwrap_or(remaining);
            transferred.fetch_add(length, Ordering::Relaxed);
            let _ = activity.try_send(());
        }
    }
}

async fn pump_io_to_h3<R, S>(
    mut reader: R,
    mut writer: h3::server::RequestStream<S, Bytes>,
    limits: TunnelLimits,
    transferred: Arc<AtomicU64>,
    activity: mpsc::Sender<()>,
) -> io::Result<PumpEnd>
where
    R: AsyncRead + Unpin,
    S: h3::quic::SendStream<Bytes>,
{
    let mut buffer = vec![0; limits.buffer_size];
    loop {
        let current = transferred.load(Ordering::Relaxed);
        if current == limits.max_bytes_per_direction {
            return Ok(PumpEnd::Limit);
        }
        let remaining = limits.max_bytes_per_direction - current;
        let read_limit = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let length = reader.read(&mut buffer[..read_limit]).await?;
        if length == 0 {
            writer.finish().await.map_err(h3_io_error)?;
            return Ok(PumpEnd::Eof);
        }
        writer
            .send_data(Bytes::copy_from_slice(&buffer[..length]))
            .await
            .map_err(h3_io_error)?;
        let length = u64::try_from(length).unwrap_or(remaining);
        transferred.fetch_add(length, Ordering::Relaxed);
        let _ = activity.try_send(());
    }
}

fn h3_io_error(error: h3::error::StreamError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _, BufWriter, duplex},
        time::timeout,
    };

    use super::*;

    #[tokio::test]
    async fn overread_bytes_are_delivered_before_the_socket() {
        let (mut peer, stream) = duplex(64);
        peer.write_all(b"socket").await.unwrap();
        let mut stream = OverreadIo::new(stream, Bytes::from_static(b"prefix-"));
        let mut bytes = vec![0; 13];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"prefix-socket");
    }

    #[tokio::test]
    async fn tunnel_stops_exactly_at_direction_limit() {
        let (mut client, proxy_left) = duplex(64);
        let (proxy_right, mut server) = duplex(64);
        let proxy_right = BufWriter::with_capacity(64, proxy_right);
        let tunnel = BoundedTunnel::new(TunnelLimits {
            max_bytes_per_direction: 4,
            idle_timeout: Duration::from_secs(1),
            lifetime_timeout: Duration::from_secs(2),
            buffer_size: 16,
        })
        .unwrap();
        let relay = tokio::spawn(async move { tunnel.relay(proxy_left, proxy_right).await });
        client.write_all(b"1234").await.unwrap();
        let mut received = [0; 4];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"1234");
        assert!(matches!(
            timeout(Duration::from_millis(100), relay)
                .await
                .unwrap()
                .unwrap(),
            TunnelOutcome::Ended {
                end: TunnelEnd::ByteLimitLeftToRight,
                stats: TunnelStats {
                    left_to_right: 4,
                    ..
                }
            }
        ));
    }

    #[tokio::test]
    async fn tunnel_relays_both_directions_and_half_closes() {
        let (mut client, proxy_left) = duplex(64);
        let (proxy_right, mut server) = duplex(64);
        let tunnel = BoundedTunnel::new(TunnelLimits {
            max_bytes_per_direction: 64,
            idle_timeout: Duration::from_secs(1),
            lifetime_timeout: Duration::from_secs(2),
            buffer_size: 16,
        })
        .unwrap();
        let relay = tokio::spawn(async move { tunnel.relay(proxy_left, proxy_right).await });

        client.write_all(b"request").await.unwrap();
        server.write_all(b"response").await.unwrap();
        let mut request = [0; 7];
        let mut response = [0; 8];
        server.read_exact(&mut request).await.unwrap();
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&request, b"request");
        assert_eq!(&response, b"response");
        client.shutdown().await.unwrap();
        server.shutdown().await.unwrap();

        assert!(matches!(
            relay.await.unwrap(),
            TunnelOutcome::Ended {
                end: TunnelEnd::Eof,
                stats: TunnelStats {
                    left_to_right: 7,
                    right_to_left: 8,
                },
            }
        ));
    }

    #[tokio::test]
    async fn tunnel_flushes_each_direction_when_the_peer_awaits_more_input() {
        let (mut client, proxy_left) = duplex(64);
        let (proxy_right, mut server) = duplex(64);
        let tunnel = BoundedTunnel::new(TunnelLimits {
            max_bytes_per_direction: 64,
            idle_timeout: Duration::from_secs(1),
            lifetime_timeout: Duration::from_secs(2),
            buffer_size: 16,
        })
        .unwrap();
        let relay =
            tokio::spawn(
                async move { tunnel.relay(proxy_left, BufWriter::new(proxy_right)).await },
            );

        client.write_all(b"request").await.unwrap();
        let mut request = [0; 7];
        timeout(Duration::from_secs(1), server.read_exact(&mut request))
            .await
            .expect("buffered tunnel flush")
            .unwrap();
        assert_eq!(&request, b"request");
        client.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        assert!(matches!(
            relay.await.unwrap(),
            TunnelOutcome::Ended {
                end: TunnelEnd::Eof,
                ..
            }
        ));
    }

    #[test]
    fn tunnel_outcomes_have_stable_safe_labels() {
        let ended = TunnelOutcome::Ended {
            end: TunnelEnd::IdleTimeout,
            stats: TunnelStats::default(),
        };
        let failed = TunnelOutcome::Io {
            stats: TunnelStats::default(),
            source: io::Error::other("not logged"),
        };

        assert_eq!(ended.kind(), TunnelOutcomeKind::IdleTimeout);
        assert_eq!(ended.kind().as_str(), "idle_timeout");
        assert_eq!(failed.kind(), TunnelOutcomeKind::IoError);
        assert_eq!(failed.stats(), TunnelStats::default());
    }
}
