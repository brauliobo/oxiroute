//! Response `HEADERS`, alone or together with the whole body when it fits the send windows. Every path encodes
//! through [`Connection::write_response`], so the bytes for one logical response never depend on the entry point.
use bytes::Bytes;
use http::{HeaderMap, StatusCode};

use super::Connection;
use super::streams::{SendState, Slot};
use super::validate;
use crate::error::Error;
use crate::frame;
use crate::hpack::Indexing;

/// Fields that must never enter the peer's dynamic table.
pub(super) fn indexing(name: &[u8]) -> Indexing {
    if matches!(
        name,
        b"set-cookie" | b"authorization" | b"proxy-authorization" | b"x-request-id"
    ) {
        Indexing::Never
    } else {
        Indexing::Incremental
    }
}

type Field<'a> = (&'a [u8], &'a [u8]);

impl Connection {
    /// # Errors
    ///
    /// The stream was reset, the connection closed or a response was already sent.
    pub fn send_response(
        &mut self,
        idx: u32,
        id: u32,
        status: StatusCode,
        headers: &HeaderMap,
        end: bool,
    ) -> Result<(), Error> {
        let fields = headers
            .iter()
            .map(|(name, value)| (name.as_str().as_bytes(), value.as_bytes()));
        self.send_response_fields(idx, id, status.as_u16(), fields, end)
    }

    /// Queues the response `HEADERS` for `fields` (lowercase names, no pseudo-headers, valid values: the caller's
    /// guarantee, checked in debug builds). `fields` is iterated with the connection locked and must not call back
    /// into it.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection closed or a response was already sent.
    pub fn send_response_fields<'a>(
        &mut self,
        idx: u32,
        id: u32,
        status: u16,
        fields: impl IntoIterator<Item = Field<'a>>,
        end: bool,
    ) -> Result<(), Error> {
        self.begin_response(idx, id, end)?;
        self.kick(|c| c.write_response(id, status, fields, end));
        if end {
            self.update_open(idx);
        }
        Ok(())
    }

    /// The response `HEADERS` and as much of `body` as fits the stream and connection send windows now, in one
    /// call. Returns how many trailing octets of `body` did not fit: with zero the body and `END_STREAM` are
    /// queued; otherwise the response stays open and the caller sends `body[body.len() - n..]` through
    /// `poll_capacity` and `send_data`. An empty `body` ends the response with the `HEADERS`.
    ///
    /// # Errors
    ///
    /// The stream was reset, the connection closed or a response was already sent.
    pub fn send_response_with_data<'a>(
        &mut self,
        idx: u32,
        id: u32,
        status: u16,
        fields: impl IntoIterator<Item = Field<'a>>,
        mut body: Bytes,
    ) -> Result<usize, Error> {
        let connection_free = (self.send_window - self.send_reserved).max(0);
        let s = self.idle(idx, id)?;
        let total = body.len();
        let fit = (total as i64)
            .min(i64::from(s.send_window))
            .min(connection_free)
            .max(0) as usize;
        let end = fit == total;
        s.send = if end {
            SendState::Ended
        } else {
            SendState::Open
        };
        s.send_window -= fit as i32;
        self.send_window -= fit as i64;
        let mut sent = body.split_to(fit);
        self.kick(|c| {
            c.write_response(id, status, fields, total == 0);
            if total > 0 {
                c.write_data(id, &mut sent, end);
            }
        });
        if end {
            self.update_open(idx);
        }
        Ok(total - fit)
    }

    /// A stream that may still start its response.
    fn idle(&mut self, idx: u32, id: u32) -> Result<&mut Slot, Error> {
        let s = self.live(idx, id)?;
        if s.send != SendState::Idle {
            return Err(Error::user("response already sent"));
        }
        Ok(s)
    }

    fn begin_response(&mut self, idx: u32, id: u32, end: bool) -> Result<(), Error> {
        self.idle(idx, id)?.send = if end {
            SendState::Ended
        } else {
            SendState::Open
        };
        Ok(())
    }

    /// Encodes and queues a response `HEADERS` frame.
    pub(super) fn write_response<'a>(
        &mut self,
        id: u32,
        status: u16,
        fields: impl IntoIterator<Item = Field<'a>>,
        end: bool,
    ) {
        debug_assert!((100..1000).contains(&status), "status {status}");
        let digits = [status / 100, status / 10 % 10, status % 10].map(|d| b'0' + d as u8);
        self.scratch.clear();
        let enc = &mut self.enc;
        enc.encode_field(
            &mut self.scratch,
            b":status",
            &digits,
            Indexing::Incremental,
        );
        for (name, value) in fields {
            debug_assert!(validate::name(name) && validate::value(value));
            enc.encode_field(&mut self.scratch, name, value, indexing(name));
        }
        frame::headers_block(
            &mut self.out.tail,
            id,
            &self.scratch,
            end,
            self.peer_max_frame,
        );
    }
}
