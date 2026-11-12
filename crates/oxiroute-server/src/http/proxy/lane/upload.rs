//! The part of a request body that has not arrived with the head. It is read from the client as it comes and written
//! to the origin as it is read, so what the proxy holds of an upload is one read buffer, and an origin that reads
//! slowly slows the client down through the socket. The exchange watches the origin's response while the upload goes
//! on: an answer before the body is over is the response, and the rest of the upload is not sent. A request whose body
//! began to flow is not sent again.

use std::{future::poll_fn, pin::pin, time::Duration};

use bytes::BufMut as _;
use futures_util::FutureExt as _;
use pingora::{
    Error, ErrorType,
    protocols::{Stream, http::v1::client::HttpSession},
};
use tokio::io::AsyncReadExt as _;

use super::{
    buffers,
    timing::{Expired, Wait, within_for},
};

pub(in crate::http::proxy) struct Upload {
    /// Body bytes the client has still to send.
    remaining: usize,
    /// The origin does not take more of the body: it answered, or a write to it failed.
    refused: bool,
}

impl Upload {
    /// The upload of a body of `announced` bytes of which `arrived` came with the head.
    pub(in crate::http::proxy) const fn new(announced: usize, arrived: usize) -> Self {
        Self {
            remaining: announced - arrived,
            refused: false,
        }
    }

    /// The client sent the whole body and the origin got it: both connections can serve another request.
    pub(in crate::http::proxy) const fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    /// Whether the client's side of the exchange has something to wait for.
    pub(in crate::http::proxy) const fn pumping(&self) -> bool {
        self.remaining > 0 && !self.refused
    }

    /// The upload ends where it is: the origin answered without reading the rest, or took no more of it.
    pub(in crate::http::proxy) fn refuse(&mut self) {
        self.refused = true;
    }

    /// Resolves when the client has bytes of the body to read, or has left (which the read tells). No buffer is held
    /// while it waits. A client that sends nothing for `idle` has left too.
    pub(in crate::http::proxy) async fn readable(
        &self,
        stream: &mut Stream,
        idle: Option<Duration>,
    ) -> Result<(), Box<Error>> {
        let ready = within_for(
            Wait::Step,
            idle,
            pin!(poll_fn(|cx| stream.poll_read_ready(cx))),
        )
        .await;
        ready
            .map_err(|Expired| {
                Error::explain(
                    ErrorType::ReadTimedout,
                    format!("reading downstream request body, timeout: {idle:?}"),
                )
                .into_down()
            })?
            .map_err(|error| {
                Error::because(
                    ErrorType::ReadError,
                    "while reading downstream request body",
                    error,
                )
                .into_down()
            })
    }

    /// Reads what the client has of the body, up to what is left of it, and writes it to the origin. The last piece
    /// ends the body. How many bytes went to the origin: 0 when the client has nothing after all. `readable` can
    /// report readiness that a read of the first request (which does not go through the I/O driver) left behind, and a
    /// read that waited here would not watch the origin's answer: the caller goes back to its wait instead.
    pub(in crate::http::proxy) async fn forward(
        &mut self,
        stream: &mut Stream,
        session: &mut HttpSession,
    ) -> Result<usize, Box<Error>> {
        let mut buffer = buffers::take_upload();
        let Some(read) = stream
            .read_buf(&mut (&mut buffer).limit(self.remaining))
            .now_or_never()
        else {
            buffers::give_upload(buffer);
            return Ok(0);
        };
        let result = match read {
            Ok(0) => Err(Error::explain(
                ErrorType::ConnectionClosed,
                "downstream closed inside the request body",
            )
            .into_down()),
            Ok(count) => Ok(count),
            Err(error) => Err(Error::because(
                ErrorType::ReadError,
                "while reading downstream request body",
                error,
            )
            .into_down()),
        };
        let written = match result {
            Ok(count) => self.write(session, &buffer, count).await,
            Err(error) => Err(error),
        };
        buffers::give_upload(buffer);
        written
    }

    async fn write(
        &mut self,
        session: &mut HttpSession,
        piece: &[u8],
        count: usize,
    ) -> Result<usize, Box<Error>> {
        session.write_body(piece).await.map_err(Error::into_up)?;
        self.remaining -= count;
        if self.remaining == 0 {
            session.finish_body().await.map_err(Error::into_up)?;
        }
        Ok(count)
    }
}
