//! The application's handle on one request/response exchange.
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderMap, StatusCode};

use super::Shared;
use crate::error::{Error, Reason};

/// One accepted request. Costs a reference count on the connection plus two integers; all state lives in the
/// connection's stream table, nothing is buffered per stream on the sending side (see [`Stream::poll_capacity`]).
///
/// Dropping it before the response is complete cancels the stream with `RST_STREAM(CANCEL)`; dropping it after
/// a complete response while the request body is still arriving sends `RST_STREAM(NO_ERROR)`, as `h2` does.
pub struct Stream {
    pub(super) shared: Arc<Shared>,
    pub(super) id: u32,
    pub(super) idx: u32,
}

impl Stream {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Next piece of the request body (a slice of the read buffer); `None` after the last one.
    pub fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        self.shared.lock().poll_data(self.idx, self.id, cx)
    }

    /// Gives `n` octets of request-body credit back; `WINDOW_UPDATE`s are batched.
    pub fn release_capacity(&mut self, n: usize) {
        self.shared.lock().release_capacity(self.idx, self.id, n);
    }

    /// The request has been fully received and every piece of it was read.
    #[must_use]
    pub fn is_end_stream(&self) -> bool {
        self.shared.lock().is_end_stream(self.idx, self.id)
    }

    /// Request trailers; call after `poll_data` returned `None`.
    pub fn poll_trailers(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<HeaderMap, Error>>> {
        self.shared.lock().poll_trailers(self.idx, self.id, cx)
    }

    /// Queues the response HEADERS; `end` sets END_STREAM. Fields are written as given: the caller strips
    /// connection-specific headers.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection closed or a response was already sent.
    pub fn send_response(
        &mut self,
        status: StatusCode,
        headers: HeaderMap,
        end: bool,
    ) -> Result<(), Error> {
        self.shared
            .lock()
            .send_response(self.idx, self.id, status, &headers, end)
    }

    /// How many octets (at least 1, at most `want`) can be framed right now within the stream and connection
    /// send windows. Pending, with the waker registered, while either window is empty; connection-window waiters
    /// are woken in FIFO order. The grant stays reserved for this stream until [`Stream::send_data`].
    pub fn poll_capacity(
        &mut self,
        cx: &mut Context<'_>,
        want: usize,
    ) -> Poll<Result<usize, Error>> {
        self.shared
            .lock()
            .poll_capacity(self.idx, self.id, cx, want)
    }

    /// Queues `data` (at most the last grant) as DATA frames of at most the peer's maximum frame size; payloads
    /// above 4 KiB are queued without copying. `end` sets END_STREAM on the last frame.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection closed, no response is in progress or `data` exceeds the grant.
    pub fn send_data(&mut self, data: Bytes, end: bool) -> Result<(), Error> {
        self.shared.lock().send_data(self.idx, self.id, data, end)
    }

    /// Sends trailers and ends the stream.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection closed or no response is in progress.
    pub fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.shared
            .lock()
            .send_trailers(self.idx, self.id, &trailers)
    }

    pub fn send_reset(&mut self, reason: Reason) {
        self.shared.lock().send_reset(self.idx, self.id, reason);
    }

    /// Ready with the code when the stream was reset (by the peer or locally), with an error when the
    /// connection closed; Pending otherwise.
    pub fn poll_reset(&mut self, cx: &mut Context<'_>) -> Poll<Result<Reason, Error>> {
        self.shared.lock().poll_reset(self.idx, self.id, cx)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.lock().drop_stream(self.idx, self.id);
    }
}
