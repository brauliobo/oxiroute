//! The server side of one HTTP/3 connection.
use std::pin::pin;
use std::sync::Arc;
use std::task::{Poll, ready};

use super::Config;
use super::peer::PeerStreams;
use super::request::RequestStream;
use super::shared::Shared;
use crate::code::Code;
use crate::error::Error;
use crate::frame::settings::PeerSettings;
use crate::frame::{self, settings, stream_type};
use crate::varint;

/// An HTTP/3 server connection: opens the control stream, accepts request streams, and reads the peer's control and
/// QPACK streams while it waits for them.
///
/// Dropping it leaves the QUIC connection to the owner; the control stream stays open until the last request
/// stream is gone.
pub struct Connection {
    shared: Arc<Shared>,
    control: Option<quinn::SendStream>,
    /// Bytes for the control stream not written yet.
    control_out: Vec<u8>,
    peer: PeerStreams,
    /// Stream id of the last request accepted.
    last_accepted: Option<u64>,
    /// The identifier of the GOAWAY frame queued or sent: request streams from it on are refused.
    goaway: Option<u64>,
}

impl Connection {
    /// Opens the control stream and sends `SETTINGS`: QPACK table capacity 0 and no blocked streams (so no QPACK
    /// stream is opened), and the field section limit of `config`.
    ///
    /// # Errors
    ///
    /// The control stream could not be opened or written, or the limit does not fit a variable-length integer.
    pub async fn new(conn: quinn::Connection, config: Config) -> Result<Self, Error> {
        if config.max_field_section_size > varint::MAX {
            return Err(Error::State(
                "max_field_section_size does not fit a variable-length integer",
            ));
        }
        let mut out = Vec::with_capacity(24);
        varint::encode(&mut out, stream_type::CONTROL);
        settings::encode(&mut out, config.max_field_section_size);
        let control = conn.open_uni().await?;
        let mut connection = Self {
            shared: Arc::new(Shared::new(conn, config)),
            control: Some(control),
            control_out: out,
            peer: PeerStreams::default(),
            last_accepted: None,
            goaway: None,
        };
        connection.flush_control().await?;
        Ok(connection)
    }

    /// The QUIC connection.
    #[must_use]
    pub fn quic(&self) -> &quinn::Connection {
        &self.shared.conn
    }

    /// The settings of the peer, once its control stream delivered them.
    #[must_use]
    pub fn peer_settings(&self) -> Option<PeerSettings> {
        self.peer.settings()
    }

    /// Cancel-safe. Waits for the next request stream, reading the peer's other streams meanwhile.
    ///
    /// Returns `None` when the peer closed the connection without an error, and also once a GOAWAY was sent or
    /// received and every request stream handed out is dropped. Streams that arrive after [`Connection::shutdown`]
    /// with an id from the one advertised on are refused with `H3_REQUEST_REJECTED`.
    ///
    /// # Errors
    ///
    /// The connection was lost, or the peer violated the protocol; the connection is then closed with the matching
    /// error code.
    pub async fn accept(&mut self) -> Result<Option<RequestStream>, Error> {
        loop {
            let accepted = self.poll_accept().await?;
            match accepted {
                Some((send, recv)) if self.is_refused(&send) => reject(send, recv),
                Some((send, recv)) => return Ok(Some(self.admit(send, recv))),
                None => return Ok(None),
            }
        }
    }

    async fn poll_accept(
        &mut self,
    ) -> Result<Option<(quinn::SendStream, quinn::RecvStream)>, Error> {
        let Self { shared, peer, .. } = self;
        let mut bi = pin!(shared.conn.accept_bi());
        let mut uni = pin!(shared.conn.accept_uni());
        std::future::poll_fn(|cx| {
            while peer.has_room() {
                match uni.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        peer.add(stream);
                        uni.set(shared.conn.accept_uni());
                    }
                    Poll::Ready(Err(_)) | Poll::Pending => break,
                }
            }
            if peer.poll(cx, shared)?.goaway {
                shared.start_closing();
            }
            if shared.is_drained(cx.waker()) {
                return Poll::Ready(Ok(None));
            }
            match ready!(bi.as_mut().poll(cx)) {
                Ok(streams) => Poll::Ready(Ok(Some(streams))),
                Err(error) => Poll::Ready(graceful_end(error)),
            }
        })
        .await
    }

    fn is_refused(&self, send: &quinn::SendStream) -> bool {
        self.goaway.is_some_and(|limit| stream_id(send) >= limit)
    }

    fn admit(&mut self, send: quinn::SendStream, recv: quinn::RecvStream) -> RequestStream {
        self.last_accepted = Some(stream_id(&send));
        RequestStream::new(Arc::clone(&self.shared), send, recv)
    }

    /// Sends `GOAWAY` naming the first request stream that will not be processed: every stream already accepted
    /// finishes, later ones are refused (see [`Connection::accept`]). Calling it again changes nothing. Cancel-safe:
    /// a call that is dropped midway is completed by the next one.
    ///
    /// # Errors
    ///
    /// The control stream could not be written.
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        if self.goaway.is_none() {
            let id = self.last_accepted.map_or(0, |last| last + 4);
            frame::write_header(
                &mut self.control_out,
                frame::GOAWAY,
                varint::size(id) as u64,
            );
            varint::encode(&mut self.control_out, id);
            self.goaway = Some(id);
            self.shared.start_closing();
        }
        self.flush_control().await
    }

    async fn flush_control(&mut self) -> Result<(), Error> {
        while !self.control_out.is_empty() {
            let control = self
                .control
                .as_mut()
                .expect("the control stream lives as long as the connection");
            let written = control
                .write(&self.control_out)
                .await
                .map_err(Error::from)?;
            self.control_out.drain(..written);
        }
        Ok(())
    }
}

fn stream_id(send: &quinn::SendStream) -> u64 {
    u64::from(quinn::VarInt::from(send.id()))
}

fn reject(mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    let code = Code::H3_REQUEST_REJECTED.varint();
    let _ = recv.stop(code);
    let _ = send.reset(code);
}

/// A connection the peer closed without an error ends the accept loop quietly.
fn graceful_end(
    error: quinn::ConnectionError,
) -> Result<Option<(quinn::SendStream, quinn::RecvStream)>, Error> {
    let no_error = match &error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            let code = close.error_code;
            code == Code::H3_NO_ERROR.varint() || code == quinn::VarInt::from_u32(0)
        }
        quinn::ConnectionError::ConnectionClosed(close) => {
            close.error_code == quinn::TransportErrorCode::NO_ERROR
        }
        _ => false,
    };
    if no_error {
        Ok(None)
    } else {
        Err(error.into())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            self.shared.keep_control(control);
        }
    }
}
