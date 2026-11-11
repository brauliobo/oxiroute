//! The connection driver: `accept` runs the read, write, flush and timer machinery of one connection.
use std::future::{Future, poll_fn};
use std::io;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf, BytesMut};
use http::request::Parts;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Sleep;

use super::{Config, Shared, Stream, buffer};
use crate::conn::{Connection, FieldCache, Output, RequestView, empty_slices};
use crate::error::{Error, Initiator, Reason};
use crate::frame::{DEFAULT_MAX_FRAME_SIZE, MAX_MAX_FRAME_SIZE, MAX_WINDOW, PREFACE};

/// Output buffers larger than this are released once written.
const WRITE_BUFFER_KEEP: usize = 4096;

/// An HTTP/2 server connection.
pub struct Server<IO> {
    io: IO,
    shared: Arc<Shared>,
    input: BytesMut,
    /// Frames taken from the connection and being written.
    writing: Output,
    needs_flush: bool,
    /// The writer let the tasks that were ready go first, so that one write carries what they queue.
    yielded: bool,
    timer: Option<Pin<Box<Sleep>>>,
    done: bool,
    /// Equal fields of consecutive requests, reused by [`Server::accept`] when it builds the `http` request head.
    fields: FieldCache,
}

fn check(config: &Config) -> Result<(), Error> {
    let frame = DEFAULT_MAX_FRAME_SIZE..=MAX_MAX_FRAME_SIZE;
    if !frame.contains(&config.max_frame_size)
        || config.initial_stream_window > MAX_WINDOW
        || config.initial_connection_window > MAX_WINDOW
    {
        return Err(Error::user("invalid HTTP/2 configuration"));
    }
    Ok(())
}

impl<IO: AsyncRead + AsyncWrite + Unpin + Send + 'static> Server<IO> {
    /// Writes our SETTINGS and reads the client connection preface. The client's first SETTINGS frame is
    /// processed by `accept`.
    ///
    /// # Errors
    ///
    /// Transport errors, an invalid configuration, or a preface that is not the HTTP/2 preface.
    pub async fn handshake(mut io: IO, config: Config) -> Result<Self, Error> {
        check(&config)?;
        let mut conn = Connection::new(config);
        let mut out = Output::default();
        conn.take_output(&mut out);
        io.write_all(&out.tail).await?;
        io.flush().await?;
        let mut input = buffer::take();
        while input.len() < PREFACE.len() {
            if io.read_buf(&mut input).await? == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
        }
        if input[..PREFACE.len()] != PREFACE[..] {
            return Err(Error::go_away(Reason::PROTOCOL_ERROR, Initiator::Library));
        }
        input.advance(PREFACE.len());
        conn.feed(&mut input);
        Ok(Self {
            io,
            shared: Arc::new(Shared::new(conn)),
            input,
            writing: Output::default(),
            needs_flush: false,
            yielded: false,
            timer: None,
            done: false,
            fields: FieldCache::default(),
        })
    }

    /// Next request. Drives all I/O while waiting; `None` when the connection is over (closed by the peer, or
    /// drained after a `GOAWAY`); an error when it failed.
    pub async fn accept(&mut self) -> Option<Result<(Parts, Stream), Error>> {
        let (view, stream) = match self.accept_view().await? {
            Ok(accepted) => accepted,
            Err(e) => return Some(Err(e)),
        };
        Some(Ok((view.into_parts_cached(&mut self.fields), stream)))
    }

    /// Like [`Server::accept`] but hands out the request head as a [`RequestView`]: borrowed bytes in a pooled
    /// arena, no `http` types built. Validation, resets and errors are exactly those of `accept`, which is this
    /// method followed by [`RequestView::into_parts`]; a connection may mix the two per request.
    pub async fn accept_view(&mut self) -> Option<Result<(RequestView, Stream), Error>> {
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    /// The `http` request head of a view this connection accepted, built as [`Server::accept`] builds it (equal
    /// fields of consecutive requests are shared).
    pub fn request_parts(&mut self, view: RequestView) -> Parts {
        view.into_parts_cached(&mut self.fields)
    }

    /// `GOAWAY(2^31-1)`, a PING, then the final `GOAWAY`: streams the client opened before it saw the first one
    /// are still returned by [`Server::accept`], which must keep being polled until it returns `None`.
    pub fn graceful_shutdown(&mut self) {
        self.shared.lock().graceful_shutdown();
    }

    /// Sends `GOAWAY(reason)` and fails every stream; `accept` then returns `None`.
    pub fn abrupt_shutdown(&mut self, reason: Reason) {
        self.shared.lock().abrupt_shutdown(reason);
    }

    fn poll_accept(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<(RequestView, Stream), Error>>> {
        loop {
            {
                let mut c = self.shared.lock();
                if let Some((idx, id, view)) = c.pop_accept() {
                    c.disarm_driver();
                    drop(c);
                    let stream = Stream {
                        shared: self.shared.clone(),
                        id,
                        idx,
                    };
                    return Poll::Ready(Some(Ok((view, stream))));
                }
                if self.done {
                    return Poll::Ready(c.take_report().map(Err));
                }
            }
            if self.drive(cx).is_pending() {
                return Poll::Pending;
            }
        }
    }

    /// One pass over write, termination checks, read and timers. Ready when something changed (the caller
    /// looks for accepted streams again), Pending when parked on the transport or a timer.
    fn drive(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.shared.lock().arm_driver(cx.waker());
        let mut progress = false;
        match self.poll_write(cx) {
            Poll::Ready(Ok(wrote)) => progress |= wrote,
            Poll::Ready(Err(e)) => return self.fail(&e),
            Poll::Pending => {}
        }
        {
            let c = self.shared.lock();
            let idle = self.writing.is_empty() && !self.needs_flush && !c.has_output();
            if idle && (c.closed().is_some() || c.is_drained()) {
                self.done = true;
                return Poll::Ready(());
            }
            if c.closed().is_some() {
                return if progress {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                };
            }
        }
        progress |= match self.poll_read(cx) {
            Poll::Ready(Ok(read)) => read,
            Poll::Ready(Err(e)) => return self.fail(&e),
            Poll::Pending => false,
        };
        progress |= self.poll_timer(cx);
        if progress {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn fail(&mut self, err: &io::Error) -> Poll<()> {
        self.shared.lock().transport_closed(Some(Error::io(err)));
        self.done = true;
        Poll::Ready(())
    }

    /// Writes everything queued with vectored writes, then flushes once. `Ready(true)` when anything was written.
    fn poll_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<bool>> {
        let mut wrote = false;
        loop {
            if self.writing.is_empty() {
                let mut c = self.shared.lock();
                if !c.has_output() {
                    break;
                }
                if !self.yielded {
                    // The scheduler runs the tasks that are ready before this one: the responses they queue leave
                    // in the same write (and so in the same TLS record).
                    self.yielded = true;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                self.yielded = false;
                c.take_output(&mut self.writing);
            }
            let mut slices = empty_slices();
            let n = self.writing.slices(&mut slices);
            match Pin::new(&mut self.io).poll_write_vectored(cx, &slices[..n]) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(written)) => {
                    self.writing.advance(written);
                    self.needs_flush = true;
                    wrote = true;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        if self.needs_flush {
            match Pin::new(&mut self.io).poll_flush(cx) {
                Poll::Ready(Ok(())) => {
                    self.needs_flush = false;
                    self.writing.shrink(WRITE_BUFFER_KEEP);
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(wrote))
    }

    /// Reads once and parses what arrived. `Ready(true)` when data or EOF was processed.
    fn poll_read(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<bool>> {
        if self.input.capacity() == 0 {
            self.input = buffer::take();
        }
        let want = self
            .shared
            .lock()
            .needed()
            .saturating_sub(self.input.len())
            .max(buffer::READ_SIZE);
        if self.input.capacity() - self.input.len() < want {
            self.input.reserve(want);
        }
        let read = {
            let fut = self.io.read_buf(&mut self.input);
            let fut = pin!(fut);
            fut.poll(cx)
        };
        match read {
            Poll::Pending => {
                if self.input.is_empty() {
                    buffer::give(std::mem::take(&mut self.input));
                }
                Poll::Pending
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(0)) => {
                let truncated = !self.input.is_empty();
                let err = truncated.then(|| Error::io(&io::ErrorKind::UnexpectedEof.into()));
                self.shared.lock().transport_closed(err);
                Poll::Ready(Ok(true))
            }
            Poll::Ready(Ok(_)) => {
                let mut c = self.shared.lock();
                c.feed(&mut self.input);
                Poll::Ready(Ok(true))
            }
        }
    }

    /// The peer must acknowledge our SETTINGS in time.
    fn poll_timer(&mut self, cx: &mut Context<'_>) -> bool {
        let timeout = {
            let c = self.shared.lock();
            if c.awaiting_settings_ack() {
                c.settings_ack_timeout()
            } else {
                std::time::Duration::ZERO
            }
        };
        if timeout.is_zero() {
            self.timer = None;
            return false;
        }
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
        if timer.as_mut().poll(cx).is_ready() {
            self.timer = None;
            self.shared.lock().conn_error(Reason::SETTINGS_TIMEOUT);
            return true;
        }
        false
    }
}

impl<IO> Drop for Server<IO> {
    fn drop(&mut self) {
        self.shared.lock().transport_closed(None);
        buffer::give(std::mem::take(&mut self.input));
    }
}
