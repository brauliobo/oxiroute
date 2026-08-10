use std::future::Future;

use super::*;

pub(super) struct LimitedIo<T> {
    inner: T,
    remaining: u64,
    transferred: Arc<AtomicU64>,
    activity: mpsc::Sender<()>,
    limit: Arc<Notify>,
}

impl<T> LimitedIo<T> {
    pub(super) fn new(
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

pub(super) async fn coordinate<L, R>(
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
pub(super) enum PumpEnd {
    Eof,
    Limit,
}

pub(super) async fn pump_h3_to_io<S, W>(
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

pub(super) async fn pump_io_to_h3<R, S>(
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
