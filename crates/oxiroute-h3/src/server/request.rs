//! One request stream: the request head and body in, the response out.
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, Request, Response};

use super::recv::Rx;
use super::scratch;
use super::send::{COPY_LIMIT, Frames};
use super::shared::Shared;
use crate::code::Code;
use crate::error::{Error, HeadError};
use crate::head::{self, Failure};
use crate::parse::{Event, Phase};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tx {
    /// Nothing, or only interim responses, sent.
    Idle,
    /// The final response head was sent.
    Started,
    Finished,
}

/// A request the peer opened. Owned by the task that serves it; dropping it before [`RequestStream::finish`] resets
/// the response (`H3_REQUEST_CANCELLED`) and stops the request body (`H3_NO_ERROR`).
pub struct RequestStream {
    shared: Arc<Shared>,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    rx: Rx,
    tx: Tx,
}

impl RequestStream {
    pub(super) fn new(
        shared: Arc<Shared>,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    ) -> Self {
        shared.request_started();
        Self {
            shared,
            send,
            recv,
            rx: Rx::new(),
            tx: Tx::Idle,
        }
    }

    /// The QUIC stream id.
    #[must_use]
    pub fn id(&self) -> u64 {
        u64::from(quinn::VarInt::from(self.send.id()))
    }

    /// The peer's address.
    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.shared.conn.remote_address()
    }

    /// Cancel-safe: state is kept in the stream.
    ///
    /// Reads and validates the request head. The size of the decoded field section is counted incrementally
    /// (name plus value lengths) and decoding stops past the limit.
    ///
    /// # Errors
    ///
    /// [`HeadError::FieldSectionTooLarge`] leaves the stream usable for an answer. [`HeadError::Malformed`] reset it
    /// (`H3_MESSAGE_ERROR`). Anything else is a stream or connection failure.
    pub async fn read_head(&mut self) -> Result<Request<()>, HeadError> {
        if self.rx.phase() != Phase::Head {
            return Err(Error::State("the request head was already read").into());
        }
        let block = match self.next_event().await? {
            Event::Headers(block) => block,
            Event::End => {
                return Err(self
                    .abort(Code::H3_REQUEST_INCOMPLETE, Error::Incomplete)
                    .into());
            }
            Event::Data(_) => unreachable!("DATA is rejected before the head"),
        };
        self.rx.peek_end(&mut self.recv);
        match head::request(&block, self.shared.config.max_field_section_size) {
            Ok(request) => Ok(request),
            Err(failure) => Err(self.head_failure(failure)),
        }
    }

    fn head_failure(&mut self, failure: Failure) -> HeadError {
        match failure {
            Failure::TooLarge(size) => HeadError::FieldSectionTooLarge { size },
            Failure::Malformed(reason) => {
                self.abort(Code::H3_MESSAGE_ERROR, Error::State(reason));
                HeadError::Malformed(reason)
            }
            Failure::Qpack(error) => {
                HeadError::Error(self.shared.fail(Code::QPACK_DECOMPRESSION_FAILED, error.0))
            }
        }
    }

    /// Cancel-safe. The next piece of the request body (a zero-copy slice of what quinn received), `None` once the
    /// body and the trailers are complete.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection failed, or the message is malformed (the stream is then reset with
    /// `H3_MESSAGE_ERROR`).
    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            match self.next_event().await? {
                Event::Data(data) => return Ok(Some(data)),
                Event::End => return Ok(None),
                Event::Headers(block) => self.accept_trailers(&block)?,
            }
        }
    }

    fn accept_trailers(&mut self, block: &[u8]) -> Result<(), Error> {
        match head::trailers(block, self.shared.config.max_field_section_size) {
            Ok(trailers) => {
                self.rx.trailers = Some(trailers);
                Ok(())
            }
            Err(Failure::TooLarge(_)) => {
                Err(self.abort_error(Code::H3_EXCESSIVE_LOAD, "trailers over the size limit"))
            }
            Err(Failure::Malformed(reason)) => {
                Err(self.abort_error(Code::H3_MESSAGE_ERROR, reason))
            }
            Err(Failure::Qpack(error)) => {
                Err(self.shared.fail(Code::QPACK_DECOMPRESSION_FAILED, error.0))
            }
        }
    }

    /// Reads (and drops) what is left of the body, then returns the trailers, if the peer sent any.
    ///
    /// # Errors
    ///
    /// As [`RequestStream::recv_data`].
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        while self.recv_data().await?.is_some() {}
        Ok(self.rx.trailers.take())
    }

    async fn next_event(&mut self) -> Result<Event, Error> {
        let done = self.tx == Tx::Finished;
        let event = self.rx.next(&mut self.recv, &self.shared, done).await;
        if let Err(Error::Stream { code, reason }) = &event {
            return Err(self.abort_error(*code, reason));
        }
        event
    }

    /// Stops the request body; the peer sees `code`.
    pub fn stop_sending(&mut self, code: Code) {
        let _ = self.recv.stop(code.varint());
    }

    /// Resets both directions of the stream.
    fn abort(&mut self, code: Code, error: Error) -> Error {
        let _ = self.recv.stop(code.varint());
        let _ = self.send.reset(code.varint());
        self.tx = Tx::Finished;
        error
    }

    fn abort_error(&mut self, code: Code, reason: &'static str) -> Error {
        self.abort(code, Error::Stream { code, reason })
    }

    /// Sends a response head (an interim `1xx` head may precede the final one).
    ///
    /// # Errors
    ///
    /// The peer stopped reading or the connection is gone.
    pub async fn send_response(&mut self, response: Response<()>) -> Result<(), Error> {
        self.expect_unfinished()?;
        let frames = Frames::response(scratch::take(), &response);
        let result = self.write(frames.bytes()).await;
        scratch::give(frames.into_buffer());
        if !response.status().is_informational() {
            self.tx = Tx::Started;
        }
        result
    }

    /// Sends one `DATA` frame.
    ///
    /// # Errors
    ///
    /// As [`RequestStream::send_response`].
    pub async fn send_data(&mut self, data: Bytes) -> Result<(), Error> {
        self.expect_started()?;
        if data.is_empty() {
            return Ok(());
        }
        let mut frames = Frames::empty(scratch::take());
        let result = if data.len() <= COPY_LIMIT {
            frames.data(&data);
            self.write(frames.bytes()).await
        } else {
            frames.data_header(data.len());
            self.write_chunks(&mut [frames.to_bytes(), data]).await
        };
        scratch::give(frames.into_buffer());
        result
    }

    /// Sends the trailers; [`RequestStream::finish`] ends the response.
    ///
    /// # Errors
    ///
    /// As [`RequestStream::send_response`].
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.expect_started()?;
        let frames = Frames::trailers(scratch::take(), &trailers);
        let result = self.write(frames.bytes()).await;
        scratch::give(frames.into_buffer());
        result
    }

    /// Sends a whole response with as few writes as possible: the head and, unless `head_only`, the body go out in
    /// one write for bodies up to 16 KiB, and the stream is finished.
    ///
    /// # Errors
    ///
    /// As [`RequestStream::send_response`].
    pub async fn send_full_response(
        &mut self,
        response: Response<()>,
        body: Bytes,
        head_only: bool,
    ) -> Result<(), Error> {
        self.expect_unfinished()?;
        let mut frames = Frames::response(scratch::take(), &response);
        let result = if head_only || body.is_empty() {
            self.write(frames.bytes()).await
        } else if body.len() <= COPY_LIMIT {
            frames.data(&body);
            self.write(frames.bytes()).await
        } else {
            frames.data_header(body.len());
            self.write_chunks(&mut [frames.to_bytes(), body]).await
        };
        scratch::give(frames.into_buffer());
        self.tx = Tx::Started;
        result?;
        self.finish()
    }

    /// Ends the response.
    ///
    /// # Errors
    ///
    /// The response was already finished or reset.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.expect_started()?;
        self.send.finish()?;
        self.tx = Tx::Finished;
        Ok(())
    }

    fn expect_unfinished(&self) -> Result<(), Error> {
        match self.tx {
            Tx::Finished => Err(Error::State("the response is finished")),
            Tx::Started => Err(Error::State("the response head was already sent")),
            Tx::Idle => Ok(()),
        }
    }

    fn expect_started(&self) -> Result<(), Error> {
        match self.tx {
            Tx::Started => Ok(()),
            Tx::Idle => Err(Error::State("no response head sent yet")),
            Tx::Finished => Err(Error::State("the response is finished")),
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let result = self.send.write_all(bytes).await;
        self.write_result(result)
    }

    async fn write_chunks(&mut self, chunks: &mut [Bytes]) -> Result<(), Error> {
        let result = self.send.write_all_chunks(chunks).await;
        self.write_result(result)
    }

    fn write_result(&self, result: Result<(), quinn::WriteError>) -> Result<(), Error> {
        match result {
            Err(quinn::WriteError::Stopped(code)) => Err(self
                .shared
                .peer_reset()
                .unwrap_or(Error::StreamStopped(code))),
            other => other.map_err(Error::from),
        }
    }
}

impl Drop for RequestStream {
    fn drop(&mut self) {
        if self.tx != Tx::Finished {
            let _ = self.send.reset(Code::H3_REQUEST_CANCELLED.varint());
        }
        if !self.rx.is_finished() {
            let _ = self.recv.stop(Code::H3_NO_ERROR.varint());
        }
        self.shared.request_ended();
    }
}
