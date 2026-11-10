//! The receive side of a request stream: the chunks quinn delivers, fed to the frame parser.
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use http::HeaderMap;

use super::shared::Shared;
use crate::error::Error;
use crate::parse::{Event, Fault, Phase, RequestParser};

pub(super) struct Rx {
    parser: RequestParser,
    pub trailers: Option<HeaderMap>,
    /// A read error met while peeking for the end, reported by the next read.
    deferred: Option<quinn::ReadError>,
}

impl Rx {
    pub fn new() -> Self {
        Self {
            parser: RequestParser::new(),
            trailers: None,
            deferred: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.parser.phase()
    }

    /// Whether the peer's FIN was read.
    pub fn is_finished(&self) -> bool {
        self.parser.is_finished()
    }

    /// Notes whether the peer already finished the stream, without waiting: a stream that is read to its end is
    /// closed quietly, one dropped earlier makes quinn send `STOP_SENDING`.
    pub fn peek_end(&mut self, recv: &mut quinn::RecvStream) {
        if self.parser.is_finished() || self.parser.has_input() || self.deferred.is_some() {
            return;
        }
        let mut cx = Context::from_waker(Waker::noop());
        match pin!(recv.read_chunk(usize::MAX, true)).poll(&mut cx) {
            Poll::Ready(Ok(Some(chunk))) => self.parser.push(chunk.bytes),
            Poll::Ready(Ok(None)) => self.parser.finish_input(),
            Poll::Ready(Err(error)) => self.deferred = Some(error),
            Poll::Pending => {}
        }
    }

    /// Advances to the next head, trailers, data piece or the end. Cancel-safe: the only suspension point is the
    /// chunk read, and everything read is stored before the next one.
    pub async fn next(
        &mut self,
        recv: &mut quinn::RecvStream,
        shared: &Shared,
        response_done: bool,
    ) -> Result<Event, Error> {
        loop {
            if let Some(event) = self
                .parser
                .step(shared.config.max_headers_frame())
                .map_err(|fault| fail(shared, fault))?
            {
                return Ok(event);
            }
            if !self.fill(recv, shared, response_done).await? {
                return self.parser.end().map_err(|fault| fail(shared, fault));
            }
        }
    }

    /// Reads the next chunk into the parser; `false` once the stream ended.
    async fn fill(
        &mut self,
        recv: &mut quinn::RecvStream,
        shared: &Shared,
        response_done: bool,
    ) -> Result<bool, Error> {
        if self.parser.is_finished() {
            return Ok(false);
        }
        let read = match self.deferred.take() {
            Some(error) => Err(error),
            None => recv.read_chunk(usize::MAX, true).await,
        };
        match read {
            Ok(Some(chunk)) => {
                self.parser.push(chunk.bytes);
                Ok(true)
            }
            Ok(None) => {
                self.parser.finish_input();
                Ok(false)
            }
            Err(quinn::ReadError::Reset(code)) if !response_done => {
                Err(shared.peer_reset().unwrap_or(Error::StreamReset(code)))
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn fail(shared: &Shared, fault: Fault) -> Error {
    match fault {
        Fault::Connection(code, reason) => shared.fail(code, reason),
        Fault::Stream(code, reason) => Error::Stream { code, reason },
    }
}
