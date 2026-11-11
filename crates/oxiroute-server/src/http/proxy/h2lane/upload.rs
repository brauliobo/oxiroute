//! The request body on its way to the origin. Each piece the client sent is written to the origin and only then does
//! the stream's credit go back to the client, so what the proxy holds of an upload is bounded by the stream's window
//! and an origin that reads slowly slows the client down. Nothing of the body is replayed: a request whose body began
//! to flow is not sent again.

use std::{future::poll_fn, time::Duration};

use bytes::Bytes;
use futures_util::FutureExt as _;
use pingora::{
    Error, ErrorType,
    protocols::http::{v1::client::HttpSession, v2::server::H2Stream},
};

use crate::http::proxy::lane::timing::{Expired, within_for};

/// What happens on the client's side of an exchange.
pub(super) enum Event {
    Piece(Bytes),
    /// The client sent all of the body.
    End,
    /// The client left, or its stream failed, or it stopped sending.
    Left(Box<Error>),
}

/// The upload of one request.
pub(super) struct Upload {
    /// The client has not sent all of the body yet.
    pending: bool,
    /// The origin does not take more of the body: it answered, or a write to it failed.
    refused: bool,
}

impl Upload {
    /// The upload of a request whose body is `length` bytes.
    pub(super) const fn new(length: usize) -> Self {
        Self {
            pending: length > 0,
            refused: false,
        }
    }

    /// The origin got the whole body: its connection can serve another request.
    pub(super) const fn is_complete(&self) -> bool {
        !self.pending
    }

    /// The upload ends where it is: the origin answered without reading the rest, or took no more of it.
    pub(super) fn refuse(&mut self) {
        self.refused = true;
    }

    pub(super) const fn pumping(&self) -> bool {
        self.pending && !self.refused
    }

    /// What the client has already sent, when it has: nothing waits.
    pub(super) fn ready(&self, stream: &H2Stream) -> Option<Event> {
        self.event(stream, None).now_or_never()
    }

    /// The next thing the client does with the body: a piece of it, the end of it, or its failure. A client that
    /// sends nothing for `idle` while the body is open has left too. The client's reset is the exchange's to watch.
    pub(super) async fn event(&self, stream: &H2Stream, idle: Option<Duration>) -> Event {
        let next = poll_fn(|cx| {
            stream.poll_data(cx).map(|data| match data {
                Some(Ok(piece)) => Event::Piece(piece),
                Some(Err(error)) => Event::Left(
                    Error::because(
                        ErrorType::ReadError,
                        "while reading downstream request body",
                        error,
                    )
                    .into_down(),
                ),
                None => Event::End,
            })
        });
        within_for(idle, next).await.unwrap_or_else(|Expired| {
            Event::Left(
                Error::explain(
                    ErrorType::ReadTimedout,
                    format!("reading downstream request body, timeout: {idle:?}"),
                )
                .into_down(),
            )
        })
    }

    /// Applies an event to the origin's session; the error ends the exchange. Returns the bytes written.
    pub(super) async fn forward(
        &mut self,
        event: Event,
        session: &mut HttpSession,
        stream: &H2Stream,
    ) -> Result<usize, Box<Error>> {
        match event {
            Event::Left(error) => Err(error),
            Event::End => {
                self.pending = false;
                session.finish_body().await.map_err(Error::into_up)?;
                Ok(0)
            }
            Event::Piece(piece) => {
                session.write_body(&piece).await.map_err(Error::into_up)?;
                stream.release_capacity(piece.len());
                Ok(piece.len())
            }
        }
    }
}
