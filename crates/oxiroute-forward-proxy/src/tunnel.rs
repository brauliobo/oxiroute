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

use bytes::{Buf as _, Bytes};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf},
    sync::mpsc,
    time::{Instant, sleep_until},
};

const MAX_TUNNEL_BUFFER_SIZE: usize = 1024 * 1024;

/// An I/O stream that replays bytes consumed beyond an HTTP header before reading the socket.
#[derive(Debug)]
pub struct OverreadIo<S> {
    prefix: Bytes,
    inner: S,
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
            idle_timeout: Duration::from_secs(60),
            lifetime_timeout: Duration::from_secs(60 * 60),
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

pub struct BoundedTunnel {
    limits: TunnelLimits,
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

    pub async fn relay<L, R>(&self, mut left: L, mut right: R) -> TunnelOutcome
    where
        L: AsyncRead + AsyncWrite + Unpin,
        R: AsyncRead + AsyncWrite + Unpin,
    {
        let left_to_right = Arc::new(AtomicU64::new(0));
        let right_to_left = Arc::new(AtomicU64::new(0));
        let (activity_tx, activity_rx) = mpsc::channel(1);
        let (left_reader, left_writer) = tokio::io::split(&mut left);
        let (right_reader, right_writer) = tokio::io::split(&mut right);
        let left_pump = pump(
            left_reader,
            right_writer,
            self.limits,
            Arc::clone(&left_to_right),
            activity_tx.clone(),
        );
        let right_pump = pump(
            right_reader,
            left_writer,
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
    let started = Instant::now();
    let lifetime_deadline = started + limits.lifetime_timeout;
    let mut idle_deadline = started + limits.idle_timeout;
    tokio::pin!(left_pump);
    tokio::pin!(right_pump);
    let mut left_end = None;
    let mut right_end = None;

    loop {
        if left_end == Some(PumpEnd::Eof) && right_end == Some(PumpEnd::Eof) {
            return TunnelOutcome::Ended {
                end: TunnelEnd::Eof,
                stats: load_stats(&left_to_right, &right_to_left),
            };
        }
        tokio::select! {
            () = sleep_until(lifetime_deadline) => {
                return TunnelOutcome::Ended {
                    end: TunnelEnd::LifetimeTimeout,
                    stats: load_stats(&left_to_right, &right_to_left),
                };
            }
            () = sleep_until(idle_deadline) => {
                return TunnelOutcome::Ended {
                    end: TunnelEnd::IdleTimeout,
                    stats: load_stats(&left_to_right, &right_to_left),
                };
            }
            Some(()) = activity_rx.recv() => {
                idle_deadline = Instant::now() + limits.idle_timeout;
            }
            result = &mut left_pump, if left_end.is_none() => {
                match result {
                    Ok(PumpEnd::Eof) => left_end = Some(PumpEnd::Eof),
                    Ok(PumpEnd::Limit) => return TunnelOutcome::Ended {
                        end: TunnelEnd::ByteLimitLeftToRight,
                        stats: load_stats(&left_to_right, &right_to_left),
                    },
                    Err(source) => return TunnelOutcome::Io {
                        stats: load_stats(&left_to_right, &right_to_left),
                        source,
                    },
                }
            }
            result = &mut right_pump, if right_end.is_none() => {
                match result {
                    Ok(PumpEnd::Eof) => right_end = Some(PumpEnd::Eof),
                    Ok(PumpEnd::Limit) => return TunnelOutcome::Ended {
                        end: TunnelEnd::ByteLimitRightToLeft,
                        stats: load_stats(&left_to_right, &right_to_left),
                    },
                    Err(source) => return TunnelOutcome::Io {
                        stats: load_stats(&left_to_right, &right_to_left),
                        source,
                    },
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

async fn pump<R, W>(
    mut reader: R,
    mut writer: W,
    limits: TunnelLimits,
    transferred: Arc<AtomicU64>,
    activity: mpsc::Sender<()>,
) -> io::Result<PumpEnd>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
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
            writer.shutdown().await?;
            return Ok(PumpEnd::Eof);
        }
        let mut written = 0;
        while written < length {
            let count = writer.write(&buffer[written..length]).await?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "tunnel endpoint accepted no bytes",
                ));
            }
            written += count;
            let count = u64::try_from(count).unwrap_or(remaining);
            transferred.fetch_add(count, Ordering::Relaxed);
            let _ = activity.try_send(());
        }
    }
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

fn load_stats(left_to_right: &AtomicU64, right_to_left: &AtomicU64) -> TunnelStats {
    TunnelStats {
        left_to_right: left_to_right.load(Ordering::Relaxed),
        right_to_left: right_to_left.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, duplex};

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
        let tunnel = BoundedTunnel::new(TunnelLimits {
            max_bytes_per_direction: 4,
            idle_timeout: Duration::from_secs(1),
            lifetime_timeout: Duration::from_secs(2),
            buffer_size: 16,
        })
        .unwrap();
        let relay = tokio::spawn(async move { tunnel.relay(proxy_left, proxy_right).await });
        client.write_all(b"123456").await.unwrap();
        let mut received = [0; 4];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"1234");
        assert!(matches!(
            relay.await.unwrap(),
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
}
