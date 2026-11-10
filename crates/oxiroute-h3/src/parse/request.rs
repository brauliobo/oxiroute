//! Frames of a request stream (RFC 9114 section 7) cut from the bytes the transport delivers, without any I/O.
use bytes::{Buf, Bytes, BytesMut};

use crate::code::Code;
use crate::frame::{self, HeaderReader};

/// What the frames of the stream turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A complete `HEADERS` payload: the request head, or the trailers once the body started.
    Headers(Bytes),
    /// A piece of a `DATA` payload (a zero-copy slice of the bytes pushed).
    Data(Bytes),
    /// The peer finished the stream cleanly between frames.
    End,
}

/// A violation by the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The connection must be closed with the code.
    Connection(Code, &'static str),
    /// The stream must be reset with the code.
    Stream(Code, &'static str),
}

/// Where in the message the stream is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the head.
    Head,
    /// The head arrived: `DATA` frames, then optionally trailers.
    Body,
    /// Trailers arrived: nothing may follow.
    Done,
}

enum Current {
    Between,
    Data(u64),
    Skip(u64),
    Headers(u64),
}

/// The receive-side frame state machine of one request stream. Buffers at most one chunk plus a `HEADERS` payload
/// of at most `max_headers_frame` bytes; unknown frames are skipped without being stored.
pub struct RequestParser {
    carry: Bytes,
    header: HeaderReader,
    current: Current,
    phase: Phase,
    eof: bool,
    /// Payload of a `HEADERS` frame that spans chunks.
    block: BytesMut,
}

impl Default for RequestParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            carry: Bytes::new(),
            header: HeaderReader::default(),
            current: Current::Between,
            phase: Phase::Head,
            eof: false,
            block: BytesMut::new(),
        }
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether the peer's FIN was seen.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.eof
    }

    /// Whether pushed bytes are waiting to be parsed.
    #[must_use]
    pub fn has_input(&self) -> bool {
        !self.carry.is_empty()
    }

    /// Supplies the next bytes of the stream; call only when [`RequestParser::has_input`] is false.
    pub fn push(&mut self, bytes: Bytes) {
        debug_assert!(self.carry.is_empty());
        self.carry = bytes;
    }

    /// Records the FIN.
    pub fn finish_input(&mut self) {
        self.eof = true;
    }

    /// The event for the end of the stream: [`Event::End`] when it fell between frames.
    ///
    /// # Errors
    ///
    /// The stream ended inside a frame (`H3_FRAME_ERROR`).
    pub fn end(&self) -> Result<Event, Fault> {
        if matches!(self.current, Current::Between) && !self.header.is_partial() {
            return Ok(Event::End);
        }
        Err(Fault::Connection(
            Code::H3_FRAME_ERROR,
            "request stream ended inside a frame",
        ))
    }

    /// Consumes pushed bytes until an event is ready; `None` when more bytes are needed.
    ///
    /// # Errors
    ///
    /// A violation of the frame rules, or a `HEADERS` frame over `max_headers_frame`.
    pub fn step(&mut self, max_headers_frame: u64) -> Result<Option<Event>, Fault> {
        loop {
            match self.current {
                Current::Between => {
                    let Some((kind, len)) = self.header.read(&mut self.carry) else {
                        return Ok(None);
                    };
                    self.current = self.begin(kind, len, max_headers_frame)?;
                }
                Current::Data(remaining) => {
                    if self.carry.is_empty() {
                        return Ok(None);
                    }
                    let take = take_len(remaining, self.carry.len());
                    self.current = left(Current::Data, remaining, take);
                    return Ok(Some(Event::Data(self.carry.split_to(take))));
                }
                Current::Skip(remaining) => {
                    let take = take_len(remaining, self.carry.len());
                    self.carry.advance(take);
                    self.current = left(Current::Skip, remaining, take);
                    if self.carry.is_empty() && matches!(self.current, Current::Skip(_)) {
                        return Ok(None);
                    }
                }
                Current::Headers(remaining) => {
                    let take = take_len(remaining, self.carry.len());
                    let last = take as u64 == remaining;
                    if !last && take == 0 {
                        return Ok(None);
                    }
                    let block = self.headers_piece(take, last);
                    self.current = left(Current::Headers, remaining, take);
                    let Some(block) = block else { return Ok(None) };
                    self.phase = if self.phase == Phase::Head {
                        Phase::Body
                    } else {
                        Phase::Done
                    };
                    return Ok(Some(Event::Headers(block)));
                }
            }
        }
    }

    /// The next `take` bytes of a `HEADERS` payload; the whole payload once `last` (zero-copy when it arrived in
    /// one piece).
    fn headers_piece(&mut self, take: usize, last: bool) -> Option<Bytes> {
        if last && self.block.is_empty() {
            return Some(self.carry.split_to(take));
        }
        self.block.extend_from_slice(&self.carry.split_to(take));
        last.then(|| self.block.split().freeze())
    }

    fn begin(&self, kind: u64, len: u64, max_headers_frame: u64) -> Result<Current, Fault> {
        let unexpected = |reason| Fault::Connection(Code::H3_FRAME_UNEXPECTED, reason);
        Ok(match kind {
            frame::DATA if self.phase == Phase::Body && len > 0 => Current::Data(len),
            frame::DATA if self.phase == Phase::Body => Current::Skip(0),
            frame::DATA => return Err(unexpected("DATA frame outside the message body")),
            frame::HEADERS if self.phase == Phase::Done => {
                return Err(unexpected("frame after the trailers"));
            }
            frame::HEADERS if len > max_headers_frame => {
                return Err(Fault::Stream(
                    Code::H3_EXCESSIVE_LOAD,
                    "HEADERS frame over the size limit",
                ));
            }
            frame::HEADERS => Current::Headers(len),
            frame::PUSH_PROMISE
            | frame::CANCEL_PUSH
            | frame::SETTINGS
            | frame::GOAWAY
            | frame::MAX_PUSH_ID => {
                return Err(unexpected("control frame on a request stream"));
            }
            kind if frame::is_reserved_h2(kind) => return Err(unexpected("reserved frame type")),
            _ => Current::Skip(len),
        })
    }
}

/// `min(remaining, available)` as a buffer length.
fn take_len(remaining: u64, available: usize) -> usize {
    usize::try_from(remaining).map_or(available, |remaining| remaining.min(available))
}

/// The state after `take` of `remaining` bytes were consumed.
fn left(state: fn(u64) -> Current, remaining: u64, take: usize) -> Current {
    match remaining - take as u64 {
        0 => Current::Between,
        rest => state(rest),
    }
}

#[cfg(test)]
mod tests;
