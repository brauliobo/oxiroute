use std::rc::Rc;

use oxiroute_h1_head::{Connection, HeadRead, ResponseHead, read_response_head};

use super::{
    chunked::{ChunkScanner, HEADER_ROOM},
    client::{Buffers, Client, Down, Flow, HEAD_INITIAL, Mode, Relay, Wait, Window},
    pipes::Carry,
    reactor::Reactor,
    relay::absorb,
    service::{Answered, Failure, Framing, Recovery, Response, Service},
    slab::Token,
    sys::{Read, Wrote},
};

pub(crate) const HEAD_LIMIT: usize = 1_048_575;
const RETAINED_HEAD: usize = 64 * 1024;
const SWITCHING_PROTOCOLS: u16 = 101;

pub(crate) struct Heads {
    head: Box<ResponseHead>,
    canonical: Vec<u8>,
    is_canonical: bool,
    raw_len: usize,
}

impl Heads {
    pub(crate) fn new() -> Self {
        Self {
            head: Box::new(ResponseHead::new()),
            canonical: Vec::new(),
            is_canonical: false,
            raw_len: 0,
        }
    }

    #[inline]
    fn read(&mut self, raw: &[u8]) -> HeadRead {
        let read = read_response_head(raw, &mut self.head, &mut self.canonical);
        if let HeadRead::Head { canonical, raw_len } = read {
            self.is_canonical = canonical;
            self.raw_len = raw_len;
        }
        read
    }

    #[inline]
    fn bytes<'a>(&'a self, raw: &'a [u8]) -> &'a [u8] {
        if self.is_canonical {
            &self.canonical
        } else {
            &raw[..self.raw_len]
        }
    }
}

#[derive(Default)]
struct Held {
    spill: Vec<u8>,
    interim: Vec<u8>,
    sent: usize,
}

#[derive(Default)]
pub(crate) struct OriginHead {
    held: Option<Box<Held>>,
    forwarded: bool,
}

impl OriginHead {
    pub(crate) fn restart(&mut self) {
        self.held = None;
        self.forwarded = false;
    }

    pub(crate) const fn forwarded(&self) -> bool {
        self.forwarded
    }

    pub(crate) fn torn(&self) -> bool {
        self.held
            .as_ref()
            .is_some_and(|held| held.sent > 0 && held.sent < held.interim.len())
    }

    fn spill_len(&self) -> usize {
        self.held.as_ref().map_or(0, |held| held.spill.len())
    }

    #[inline]
    fn pending<'a>(&'a self, resp: &'a [u8], len: usize) -> &'a [u8] {
        match &self.held {
            Some(held) if !held.spill.is_empty() => &held.spill,
            _ => &resp[..len],
        }
    }

    #[inline]
    fn leftover(&mut self, resp: &mut [u8], resp_len: usize, raw_len: usize) -> Window {
        if self.held.is_none() {
            return Window::new(raw_len, resp_len);
        }
        self.leftover_of_held(resp, resp_len, raw_len)
    }

    #[inline(never)]
    fn leftover_of_held(&mut self, resp: &mut [u8], resp_len: usize, raw_len: usize) -> Window {
        let Some(held) = self.held.take().filter(|held| !held.spill.is_empty()) else {
            return Window::new(raw_len, resp_len);
        };
        let tail = &held.spill[raw_len..];
        resp[HEADER_ROOM..HEADER_ROOM + tail.len()].copy_from_slice(tail);
        Window::new(HEADER_ROOM, HEADER_ROOM + tail.len())
    }

    fn held(&mut self) -> &mut Held {
        self.held.get_or_insert_default()
    }

    fn consume(&mut self, resp: &mut [u8], resp_len: &mut usize, count: usize) {
        if let Some(held) = self.held.as_mut().filter(|held| !held.spill.is_empty()) {
            held.spill.drain(..count);
        } else {
            resp.copy_within(count..*resp_len, 0);
            *resp_len -= count;
        }
    }
}

fn pending<'a>(spill: &'a [u8], resp: &'a [u8], len: usize) -> &'a [u8] {
    if spill.is_empty() {
        &resp[..len]
    } else {
        spill
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Next {
    Partial,
    Interim,
    Final,
    Refused,
    TooLarge,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Filled {
    Data,
    Blocked,
}

impl<S: Service> Reactor<S> {
    pub(crate) fn await_head(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<bool, Failure> {
        let mut progressed = false;
        loop {
            if !self.flush_interim(token, client)? {
                return Ok(progressed);
            }
            match self.next_head(client) {
                Next::Refused => return Err(Failure::BadResponse),
                Next::TooLarge => return Err(Failure::ResponseHeadTooLarge),
                Next::Final => return self.open_response(token, client).map(|()| true),
                Next::Interim => {
                    self.queue_interim(client);
                    progressed = true;
                    continue;
                }
                Next::Partial => {}
            }
            match self.fill_head(client, upstream)? {
                Filled::Data => progressed = true,
                Filled::Blocked => return Ok(progressed),
            }
        }
    }

    fn next_head(&mut self, client: &mut Client<S>) -> Next {
        let Client {
            buffers,
            flow,
            resp_len,
            ..
        } = client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let raw = flow.origin.pending(&buffers.resp, *resp_len);
        if raw.is_empty() {
            return Next::Partial;
        }
        match self.response.read(raw) {
            HeadRead::Partial => Next::Partial,
            HeadRead::Refused => Next::Refused,
            HeadRead::Head { raw_len, .. } if raw_len > HEAD_LIMIT => Next::TooLarge,
            HeadRead::Head { .. } => match self.response.head.status {
                SWITCHING_PROTOCOLS => Next::Refused,
                100..=199 => Next::Interim,
                _ => Next::Final,
            },
        }
    }

    fn queue_interim(&mut self, client: &mut Client<S>) {
        let service = Rc::clone(&client.service);
        let Client {
            buffers,
            flow,
            resp_len,
            ..
        } = client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let exchange = flow.exchange.as_ref().expect("an exchange is served once");
        let origin = &mut flow.origin;
        origin.forwarded = true;
        let Held {
            spill,
            interim,
            sent,
        } = &mut **origin.held.get_or_insert_default();
        let raw = pending(spill, &buffers.resp, *resp_len);
        let response = Response {
            bytes: self.response.bytes(raw),
            head: &self.response.head,
            framing: Framing::Empty,
            keepalive: flow.client_keepalive,
            request_is_head: flow.is_head,
        };
        interim.clear();
        service.render_response(exchange, &response, self.now, interim);
        *sent = 0;
        origin.consume(&mut buffers.resp, resp_len, self.response.raw_len);
    }

    #[inline]
    fn flush_interim(&mut self, token: Token, client: &mut Client<S>) -> Result<bool, Failure> {
        if client.flow().origin.held.is_none() {
            return Ok(true);
        }
        self.send_interim(token, client)
    }

    #[inline(never)]
    fn send_interim(&mut self, token: Token, client: &mut Client<S>) -> Result<bool, Failure> {
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        let Some(held) = flow
            .origin
            .held
            .as_mut()
            .filter(|held| !held.interim.is_empty())
        else {
            return Ok(true);
        };
        match client.link.send(&held.interim[held.sent..], &[]) {
            Wrote::Bytes(count) => held.sent += count,
            Wrote::Block => {}
            Wrote::Failed => return Err(Failure::ClientGone),
        }
        if held.sent < held.interim.len() {
            let limit = client.timeouts.write;
            self.wait_for(token, client, Wait::ClientWrite, limit);
            return Ok(false);
        }
        held.interim.clear();
        held.sent = 0;
        self.rearm_header_wait(token, client);
        Ok(true)
    }

    fn rearm_header_wait(&mut self, token: Token, client: &mut Client<S>) {
        let flow = client.flow();
        if flow.upload_done || flow.upload_failed {
            let limit = flow.timeouts.map(|timeouts| timeouts.header);
            self.wait_for(token, client, Wait::Header, limit);
        }
    }

    fn fill_head(&mut self, client: &mut Client<S>, upstream: Token) -> Result<Filled, Failure> {
        let Client {
            buffers,
            flow,
            resp_len,
            ..
        } = client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        if buffers.resp.exhausted(*resp_len) && flow.origin.spill_len() == 0 {
            flow.origin
                .held()
                .spill
                .extend_from_slice(&buffers.resp[..*resp_len]);
            *resp_len = 0;
        }
        let spill_len = flow.origin.spill_len();
        if spill_len > HEAD_LIMIT {
            return Err(Failure::ResponseHeadTooLarge);
        }
        let spilled = spill_len > 0;
        let Some(origin) = self.slab.upstream_mut(upstream) else {
            return Err(Failure::UpstreamClosed);
        };
        let start = if spilled { HEADER_ROOM } else { *resp_len };
        match buffers.resp.recv(&mut origin.link, start, usize::MAX) {
            Read::Data(count) => {
                if spilled {
                    flow.origin
                        .held()
                        .spill
                        .extend_from_slice(&buffers.resp[start..start + count]);
                } else {
                    *resp_len += count;
                }
                flow.first_byte_at.get_or_insert(self.moment);
                Ok(Filled::Data)
            }
            Read::Block => Ok(Filled::Blocked),
            Read::Eof | Read::Failed => Err(flow.closed_before_head()),
        }
    }

    fn open_response(&mut self, token: Token, client: &mut Client<S>) -> Result<(), Failure> {
        let service = Rc::clone(&client.service);
        let Client {
            buffers,
            flow,
            resp_len,
            ..
        } = &mut *client;
        let buffers: &mut Buffers = buffers;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let head = &*self.response.head;
        let facts = head.facts;
        let exchange = flow.exchange.as_mut().expect("an exchange is served once");
        let retryable =
            flow.replayable && flow.attempts < self.config.max_attempts && !flow.origin.forwarded();
        let raw = flow.origin.pending(&buffers.resp, *resp_len);
        let bytes = self.response.bytes(raw);
        let answered = Answered {
            bytes,
            head,
            connected: flow.connected_at.map(|at| self.clock.instant(at)),
            now: self.now,
            retryable,
        };
        if service.judge(exchange, &answered) == Recovery::Retry && retryable {
            return Err(Failure::UnexpectedStatus);
        }
        let no_body = flow.is_head || matches!(head.status, 204 | 304);
        let (framing, mode) = match facts.content_length {
            _ if no_body => (Framing::Empty, Mode::Empty),
            Some(length) => (Framing::Length, Mode::Length(length)),
            None if facts.transfer_encoding && facts.chunked_final => {
                (Framing::Chunked, Mode::Chunked(ChunkScanner::new()))
            }
            None => (Framing::Chunked, Mode::UntilClose),
        };
        flow.keepalive = flow.client_keepalive && flow.upload_done && self.drain_deadline.is_none();
        let origin_keepalive = match facts.connection {
            Connection::Close => false,
            Connection::KeepAlive => true,
            _ => head.minor == 1,
        } && !(head.minor == 0 && facts.transfer_encoding);
        let reusable = origin_keepalive && !matches!(mode, Mode::UntilClose);
        let down_head = &mut buffers.down_head;
        down_head.clear();
        if down_head.capacity() > RETAINED_HEAD {
            *down_head = Vec::with_capacity(HEAD_INITIAL);
        }
        let response = Response {
            bytes,
            head,
            framing,
            keepalive: flow.keepalive,
            request_is_head: flow.is_head,
        };
        service.render_response(exchange, &response, self.now, down_head);
        let leftover = flow
            .origin
            .leftover(&mut buffers.resp, *resp_len, self.response.raw_len);
        let mut relay = Relay {
            mode,
            window: Window::default(),
            head_pos: 0,
            complete: false,
            reusable,
            carry: Carry::Undecided,
        };
        absorb(&mut relay, &mut buffers.resp, leftover, &mut flow.sent)?;
        flow.status = Some(head.status);
        flow.down = Down::Relaying(relay);
        let limit = flow.timeouts.map(|timeouts| timeouts.body);
        self.wait_for(token, client, Wait::Body, limit);
        Ok(())
    }
}

impl<S: Service> Flow<S> {
    fn closed_before_head(&self) -> Failure {
        if self.upload_failed {
            Failure::UpstreamWrite
        } else {
            Failure::UpstreamClosed
        }
    }
}
