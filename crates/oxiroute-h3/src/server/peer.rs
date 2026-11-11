//! The unidirectional streams of the peer: control, QPACK encoder and decoder streams, and the ones that are none
//! of these (RFC 9114 section 6.2, RFC 9204 section 4.2).
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;

use super::shared::Shared;
use crate::code::Code;
use crate::error::Error;
use crate::frame::settings::PeerSettings;
use crate::frame::stream_type;
use crate::parse::{Control, Violation};
use crate::varint;

/// Streams whose type is not known yet. A peer that opens more and sends nothing only holds back its own stream
/// credit: further streams are not accepted until some of these resolve.
const MAX_UNTYPED: usize = 8;

/// A stream the type varint of which is still arriving.
struct Untyped {
    stream: quinn::RecvStream,
    buf: [u8; 8],
    have: usize,
    need: usize,
}

#[derive(Clone, Copy)]
enum Typed {
    Control,
    Push,
    Encoder,
    Decoder,
    Unknown,
}

impl Untyped {
    fn new(stream: quinn::RecvStream) -> Self {
        Self {
            stream,
            buf: [0; 8],
            have: 0,
            need: 1,
        }
    }

    /// `Ready(None)` when the stream ended or was reset before its type arrived, which a receiver must tolerate.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Option<Typed>> {
        while self.have < self.need {
            match self
                .stream
                .poll_read(cx, &mut self.buf[self.have..self.need])
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(n)) if n > 0 => self.have += n,
                Poll::Ready(_) => return Poll::Ready(None),
            }
            if self.have == 1 {
                self.need = varint::len_from_first(self.buf[0]);
            }
        }
        let mut view = &self.buf[..self.need];
        Poll::Ready(varint::decode(&mut view).map(|kind| match kind {
            stream_type::CONTROL => Typed::Control,
            stream_type::PUSH => Typed::Push,
            stream_type::QPACK_ENCODER => Typed::Encoder,
            stream_type::QPACK_DECODER => Typed::Decoder,
            _ => Typed::Unknown,
        }))
    }
}

/// Reads `stream` into `buf`: `Ready(Some(bytes))` for data, `Ready(None)` once it ended or was reset.
fn poll_chunk(
    stream: &mut quinn::RecvStream,
    cx: &mut Context<'_>,
    buf: &mut [u8],
) -> Poll<Option<usize>> {
    match stream.poll_read(cx, buf) {
        Poll::Pending => Poll::Pending,
        Poll::Ready(Ok(n)) if n > 0 => Poll::Ready(Some(n)),
        Poll::Ready(_) => Poll::Ready(None),
    }
}

struct ControlStream {
    stream: quinn::RecvStream,
    control: Control,
}

/// Peer decoder stream (RFC 9204 section 4.4): only `Stream Cancellation` can be valid, because this server never
/// inserts into the peer's table and sends no field section that needs an acknowledgment.
struct DecoderStream {
    stream: quinn::RecvStream,
    /// Continuation octets of a `Stream Cancellation` integer still expected.
    continuation: Option<u8>,
}

impl DecoderStream {
    fn feed(&mut self, data: &[u8]) -> Result<(), Violation> {
        for &byte in data {
            self.continuation = match self.continuation {
                Some(left) if byte & 0x80 != 0 => match left.checked_sub(1) {
                    Some(left) => Some(left),
                    None => {
                        return Err((
                            Code::QPACK_DECODER_STREAM_ERROR,
                            "stream identifier too long",
                        ));
                    }
                },
                Some(_) => None,
                None if byte & 0xc0 == 0x40 => (byte & 0x3f == 0x3f).then_some(8),
                None => {
                    return Err((
                        Code::QPACK_DECODER_STREAM_ERROR,
                        "acknowledgment of a table that does not exist",
                    ));
                }
            };
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct PeerStreams {
    untyped: Vec<Untyped>,
    control: Option<ControlStream>,
    encoder: Option<quinn::RecvStream>,
    decoder: Option<DecoderStream>,
}

/// What polling the peer's streams found.
pub(super) struct Polled {
    pub goaway: bool,
}

/// Reads the peer's unidirectional streams for the life of the connection: one task per connection, woken only by
/// those streams. It ends when the connection does, or after closing it for a violation.
pub(super) async fn drive(shared: Arc<Shared>) {
    let mut peer = PeerStreams::default();
    let mut accept = pin!(shared.conn.accept_uni());
    std::future::poll_fn(|cx| {
        while peer.has_room() {
            match accept.as_mut().poll(cx) {
                Poll::Ready(Ok(stream)) => {
                    peer.add(stream);
                    accept.set(shared.conn.accept_uni());
                }
                Poll::Ready(Err(_)) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }
        let Ok(polled) = peer.poll(cx, &shared) else {
            return Poll::Ready(());
        };
        if let Some(settings) = peer.settings() {
            let _ = shared.peer_settings.set(settings);
        }
        if polled.goaway {
            shared.start_closing();
        }
        Poll::Pending
    })
    .await;
}

impl PeerStreams {
    fn settings(&self) -> Option<PeerSettings> {
        self.control
            .as_ref()
            .and_then(|control| control.control.settings())
    }

    /// Whether more streams may be accepted.
    pub fn has_room(&self) -> bool {
        self.untyped.len() < MAX_UNTYPED
    }

    pub fn add(&mut self, stream: quinn::RecvStream) {
        self.untyped.push(Untyped::new(stream));
    }

    /// Resolves stream types and reads the streams that are known; never completes, errors close the connection.
    pub fn poll(&mut self, cx: &mut Context<'_>, shared: &Shared) -> Result<Polled, Error> {
        let mut polled = Polled { goaway: false };
        self.resolve(cx, shared)?;
        let violation = self.read_known(cx, &mut polled);
        violation.map_err(|(code, reason)| shared.fail(code, reason))?;
        Ok(polled)
    }

    fn resolve(&mut self, cx: &mut Context<'_>, shared: &Shared) -> Result<(), Error> {
        let mut i = 0;
        while i < self.untyped.len() {
            match self.untyped[i].poll(cx) {
                Poll::Pending => i += 1,
                Poll::Ready(typed) => {
                    let Untyped { stream, .. } = self.untyped.swap_remove(i);
                    if let Some(typed) = typed {
                        self.assign(typed, stream, shared)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn assign(
        &mut self,
        typed: Typed,
        mut stream: quinn::RecvStream,
        shared: &Shared,
    ) -> Result<(), Error> {
        let duplicate = |code| shared.fail(code, "a second stream of a kind that exists once");
        match typed {
            Typed::Control if self.control.is_some() => {
                Err(duplicate(Code::H3_STREAM_CREATION_ERROR))
            }
            Typed::Encoder if self.encoder.is_some() => {
                Err(duplicate(Code::H3_STREAM_CREATION_ERROR))
            }
            Typed::Decoder if self.decoder.is_some() => {
                Err(duplicate(Code::H3_STREAM_CREATION_ERROR))
            }
            Typed::Control => {
                self.control = Some(ControlStream {
                    stream,
                    control: Control::new(),
                });
                Ok(())
            }
            Typed::Encoder => {
                self.encoder = Some(stream);
                Ok(())
            }
            Typed::Decoder => {
                self.decoder = Some(DecoderStream {
                    stream,
                    continuation: None,
                });
                Ok(())
            }
            Typed::Push => Err(shared.fail(
                Code::H3_STREAM_CREATION_ERROR,
                "a client must not open a push stream",
            )),
            Typed::Unknown => {
                let _ = stream.stop(Code::H3_STREAM_CREATION_ERROR.varint());
                Ok(())
            }
        }
    }

    fn read_known(&mut self, cx: &mut Context<'_>, polled: &mut Polled) -> Result<(), Violation> {
        let mut buf = [0u8; 512];
        let closed = (
            Code::H3_CLOSED_CRITICAL_STREAM,
            "a critical stream was closed",
        );
        if let Some(control) = &mut self.control {
            while let Poll::Ready(read) = poll_chunk(&mut control.stream, cx, &mut buf) {
                let n = read.ok_or(closed)?;
                polled.goaway |= control.control.feed(Bytes::copy_from_slice(&buf[..n]))?;
            }
        }
        if let Some(stream) = &mut self.encoder {
            while let Poll::Ready(read) = poll_chunk(stream, cx, &mut buf) {
                let n = read.ok_or(closed)?;
                if buf[..n].iter().any(|&byte| byte != 0x20) {
                    return Err((
                        Code::QPACK_ENCODER_STREAM_ERROR,
                        "encoder instruction that needs table space",
                    ));
                }
            }
        }
        if let Some(decoder) = &mut self.decoder {
            while let Poll::Ready(read) = poll_chunk(&mut decoder.stream, cx, &mut buf) {
                decoder.feed(&buf[..read.ok_or(closed)?])?;
            }
        }
        Ok(())
    }
}
