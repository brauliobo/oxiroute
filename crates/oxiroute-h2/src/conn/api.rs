//! The poll-style operations behind the application's stream handles and the connection-level controls
//! (accept queue, graceful shutdown). Every method is called with the connection mutex held and never blocks.
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{self, HeaderName};
use http::request::Parts;
use http::{HeaderMap, StatusCode};

use super::streams::{RecvState, SendState, Slot};
use super::{Connection, SHUTDOWN_PING, Shutdown};
use crate::conn::output::COPY_LIMIT;
use crate::error::{Error, Initiator, Reason};
use crate::frame;
use crate::hpack::Indexing;

fn indexing(name: &HeaderName) -> Indexing {
    if name == header::SET_COOKIE
        || name == header::AUTHORIZATION
        || name == header::PROXY_AUTHORIZATION
        || name.as_str() == "x-request-id"
    {
        Indexing::Never
    } else {
        Indexing::Incremental
    }
}

fn reset_error(s: &Slot) -> Option<Error> {
    s.reset.map(|(reason, by)| Error::reset(reason, by))
}

fn park(slot: &mut Option<std::task::Waker>, cx: &Context<'_>) {
    if !slot.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
        *slot = Some(cx.waker().clone());
    }
}

impl Connection {
    /// `GOAWAY(2^31-1)` and a PING now, the final `GOAWAY` when the PING comes back: streams the client opened
    /// before it saw the first `GOAWAY` are still accepted.
    pub fn graceful_shutdown(&mut self) {
        if self.shutdown == Shutdown::None && self.closed.is_none() {
            self.kick(|c| {
                frame::go_away(&mut c.out.tail, 0x7fff_ffff, 0);
                frame::ping(&mut c.out.tail, SHUTDOWN_PING, false);
            });
            self.shutdown = Shutdown::Draining;
        }
    }

    /// Next request the application has not seen: slot index, stream id and request head.
    pub fn pop_accept(&mut self) -> Option<(u32, u32, Box<Parts>)> {
        while let Some(p) = self.accept.pop_front() {
            if p.counted {
                self.pending_resets -= 1;
            }
            if let Some(parts) = p.parts {
                self.live_pending -= 1;
                self.streams.slot(p.idx).accepted = true;
                return Some((p.idx, p.id, parts));
            }
        }
        None
    }

    /// A stream that exists and was not reset; otherwise the error to report.
    fn live(&mut self, idx: u32, id: u32) -> Result<&mut Slot, Error> {
        if let Some(e) = &self.closed {
            return Err(e.clone());
        }
        let s = self.streams.get(idx, id).ok_or_else(Error::closed)?;
        match reset_error(s) {
            Some(e) => Err(e),
            None => Ok(s),
        }
    }

    // ---- request body ----

    pub fn poll_data(
        &mut self,
        idx: u32,
        id: u32,
        cx: &Context<'_>,
    ) -> Poll<Option<Result<Bytes, Error>>> {
        let Some(s) = self.streams.get(idx, id) else {
            return Poll::Ready(None);
        };
        if let Some(c) = s.body.as_mut().and_then(|b| b.chunks.pop_front()) {
            self.replenish(&c.data, c.budgeted);
            return Poll::Ready(Some(Ok(c.data)));
        }
        if s.recv == RecvState::Ended {
            return Poll::Ready(None);
        }
        if let Some(e) = reset_error(s).or_else(|| self.closed.clone()) {
            return Poll::Ready(Some(Err(e)));
        }
        park(&mut s.wakers.recv, cx);
        Poll::Pending
    }

    pub fn poll_trailers(
        &mut self,
        idx: u32,
        id: u32,
        cx: &Context<'_>,
    ) -> Poll<Option<Result<HeaderMap, Error>>> {
        let Some(s) = self.streams.get(idx, id) else {
            return Poll::Ready(None);
        };
        if let Some(t) = s.body.as_mut().and_then(|b| b.trailers.take()) {
            return Poll::Ready(Some(Ok(t)));
        }
        if s.recv == RecvState::Ended {
            return Poll::Ready(None);
        }
        if let Some(e) = reset_error(s).or_else(|| self.closed.clone()) {
            return Poll::Ready(Some(Err(e)));
        }
        park(&mut s.wakers.recv, cx);
        Poll::Pending
    }

    pub fn is_end_stream(&mut self, idx: u32, id: u32) -> bool {
        self.streams.get(idx, id).is_none_or(|s| {
            s.recv == RecvState::Ended && s.body.as_ref().is_none_or(|b| b.chunks.is_empty())
        })
    }

    /// The application consumed `n` octets of request body: connection and stream credit, sent in batches.
    pub fn release_capacity(&mut self, idx: u32, id: u32, n: usize) {
        let n = n.min(u32::MAX as usize) as u32;
        self.kick(|c| {
            c.release_conn(n);
            if c.streams.get(idx, id).is_some() {
                c.release_stream(idx, n);
            }
        });
    }

    /// Stream-level receive credit; nothing to send once the peer is done with the stream.
    pub(super) fn release_stream(&mut self, idx: u32, n: u32) {
        let half = (self.cfg.initial_stream_window / 2).max(1);
        let s = self.streams.slot(idx);
        if s.recv == RecvState::Ended || s.reset.is_some() {
            return;
        }
        s.unreleased += n;
        if s.unreleased >= half {
            let inc = std::mem::take(&mut s.unreleased);
            s.recv_window += inc as i32;
            frame::window_update(&mut self.out.tail, s.id, inc);
        }
    }

    // ---- response ----

    pub fn send_response(
        &mut self,
        idx: u32,
        id: u32,
        status: StatusCode,
        headers: &HeaderMap,
        end: bool,
    ) -> Result<(), Error> {
        let s = self.live(idx, id)?;
        if s.send != SendState::Idle {
            return Err(Error::user("response already sent"));
        }
        s.send = if end {
            SendState::Ended
        } else {
            SendState::Open
        };
        self.kick(|c| c.write_response(id, status, headers, end));
        if end {
            self.update_open(idx);
        }
        Ok(())
    }

    /// Encodes and queues a response HEADERS frame.
    pub(super) fn write_response(
        &mut self,
        id: u32,
        status: StatusCode,
        headers: &HeaderMap,
        end: bool,
    ) {
        self.scratch.clear();
        let enc = &mut self.enc;
        enc.encode_field(
            &mut self.scratch,
            b":status",
            status.as_str().as_bytes(),
            Indexing::Incremental,
        );
        for (name, value) in headers {
            enc.encode_field(
                &mut self.scratch,
                name.as_str().as_bytes(),
                value.as_bytes(),
                indexing(name),
            );
        }
        frame::headers_block(
            &mut self.out.tail,
            id,
            &self.scratch,
            end,
            self.peer_max_frame,
        );
    }

    /// How many octets (at least one, at most `want`) fit in both windows now; the grant is reserved until
    /// `send_data` or the next call.
    pub fn poll_capacity(
        &mut self,
        idx: u32,
        id: u32,
        cx: &Context<'_>,
        want: usize,
    ) -> Poll<Result<usize, Error>> {
        let (mine, others) = match self.live(idx, id) {
            Ok(s) if s.send == SendState::Open => (i64::from(s.reserved), i64::from(s.send_window)),
            Ok(s) if s.send == SendState::Idle => {
                return Poll::Ready(Err(Error::user("send the response first")));
            }
            Ok(_) => return Poll::Ready(Err(Error::user("response already finished"))),
            Err(e) => return Poll::Ready(Err(e)),
        };
        let conn_free = (self.send_window - (self.send_reserved - mine)).max(0);
        let grant = (want as i64).min(others).min(conn_free);
        let s = self.streams.slot(idx);
        if want == 0 || grant > 0 {
            s.reserved = grant as u32;
            self.send_reserved += grant - mine;
            return Poll::Ready(Ok(grant as usize));
        }
        park(&mut s.wakers.send, cx);
        if conn_free == 0 && !s.waiting_conn {
            s.waiting_conn = true;
            self.cap_waiters.push_back(id);
        }
        Poll::Pending
    }

    pub fn send_data(
        &mut self,
        idx: u32,
        id: u32,
        mut data: Bytes,
        end: bool,
    ) -> Result<(), Error> {
        let s = self.live(idx, id)?;
        if s.send != SendState::Open {
            return Err(Error::user("no response in progress"));
        }
        let len = data.len();
        if len as u64 > u64::from(s.reserved) {
            return Err(Error::user("send_data exceeds the granted capacity"));
        }
        s.send_window -= len as i32;
        let reserved = std::mem::take(&mut s.reserved);
        if end {
            s.send = SendState::Ended;
        }
        self.send_window -= len as i64;
        self.send_reserved -= i64::from(reserved);
        if reserved as usize > len {
            self.wake_cap_waiters();
        }
        self.kick(|c| c.write_data(id, &mut data, end));
        if end {
            self.update_open(idx);
        }
        Ok(())
    }

    fn write_data(&mut self, id: u32, data: &mut Bytes, end: bool) {
        if data.is_empty() && !end {
            return;
        }
        loop {
            let n = data.len().min(self.peer_max_frame);
            let last = n == data.len();
            frame::data_head(&mut self.out.tail, n, id, end && last);
            let chunk = data.split_to(n);
            if n <= COPY_LIMIT {
                self.out.tail.extend_from_slice(&chunk);
            } else {
                self.out.push_segment(chunk);
            }
            if last {
                return;
            }
        }
    }

    pub fn send_trailers(&mut self, idx: u32, id: u32, trailers: &HeaderMap) -> Result<(), Error> {
        let s = self.live(idx, id)?;
        if s.send != SendState::Open {
            return Err(Error::user("no response in progress"));
        }
        s.send = SendState::Ended;
        self.kick(|c| {
            c.scratch.clear();
            for (name, value) in trailers {
                c.enc.encode_field(
                    &mut c.scratch,
                    name.as_str().as_bytes(),
                    value.as_bytes(),
                    indexing(name),
                );
            }
            frame::headers_block(&mut c.out.tail, id, &c.scratch, true, c.peer_max_frame);
        });
        self.update_open(idx);
        Ok(())
    }

    pub fn send_reset(&mut self, idx: u32, id: u32, reason: Reason) {
        if self.closed.is_none()
            && self
                .streams
                .get(idx, id)
                .is_some_and(|s| s.reset.is_none() && !s.is_closed())
        {
            self.kick(|c| {
                frame::rst_stream(&mut c.out.tail, id, reason.code());
                c.reset_stream(idx, reason, Initiator::User);
            });
        }
    }

    pub fn poll_reset(
        &mut self,
        idx: u32,
        id: u32,
        cx: &Context<'_>,
    ) -> Poll<Result<Reason, Error>> {
        let Some(s) = self.streams.get(idx, id) else {
            return Poll::Ready(Err(Error::closed()));
        };
        if let Some((reason, _)) = s.reset {
            return Poll::Ready(Ok(reason));
        }
        if let Some(e) = &self.closed {
            return Poll::Ready(Err(e.clone()));
        }
        park(&mut s.wakers.reset, cx);
        Poll::Pending
    }

    /// The application dropped its handle: a response left unfinished is cancelled (`RST_STREAM(CANCEL)`, like
    /// `h2`), a finished response with the request still streaming is cut short with `RST_STREAM(NO_ERROR)`.
    pub fn drop_stream(&mut self, idx: u32, id: u32) {
        let Some(s) = self.streams.get(idx, id) else {
            return;
        };
        let reason = match (s.reset, s.send, s.recv) {
            (Some(_), ..) => None,
            (None, SendState::Ended, RecvState::Ended) => None,
            (None, SendState::Ended, _) => Some(Reason::NO_ERROR),
            (None, ..) => Some(Reason::CANCEL),
        };
        self.kick(|c| {
            if let Some(reason) = reason.filter(|_| c.closed.is_none()) {
                frame::rst_stream(&mut c.out.tail, id, reason.code());
                c.reset_stream(idx, reason, Initiator::User);
            }
            c.release_reservation(idx);
            c.discard_body(idx);
            c.update_open(idx);
            c.streams.remove(id);
        });
    }
}
