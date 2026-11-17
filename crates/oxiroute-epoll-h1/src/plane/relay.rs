use oxiroute_uring::Peek;

use super::{
    chunked::{HEADER_ROOM, LAST_CHUNK, Step as ChunkStep, write_chunk_header},
    client::{Buffers, Cargo, Client, Down, Flow, Mode, RESPONSE_TAIL, Relay, Wait, Window},
    link::{Delivery, Link},
    reactor::Reactor,
    service::{Failure, Service},
    slab::Token,
    splice::Spliced,
    sys::{Read, Wrote},
};

const TAIL_ROOM: usize = RESPONSE_TAIL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Moved {
    Progress,
    Blocked,
    Complete,
}

impl<S: Service> Reactor<S> {
    pub(crate) fn move_data(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<Moved, Failure> {
        let uploaded = self.upload(token, client, upstream)?;
        let downloaded = self.download(token, client, upstream)?;
        if matches!(client.flow().down, Down::Finished { .. }) {
            return Ok(Moved::Complete);
        }
        Ok(if uploaded || downloaded {
            Moved::Progress
        } else {
            Moved::Blocked
        })
    }

    fn upload(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<bool, Failure> {
        let mut progressed = false;
        loop {
            let flow = client.flow.as_mut().expect("a serving client has a flow");
            if flow.upload_done || flow.upload_failed || !matches!(flow.down, Down::Awaiting) {
                return Ok(progressed);
            }
            let head = &client.buffers.up_head[flow.head_pos..];
            let body = &client.buffers.req[flow.window.start..flow.window.end];
            if head.is_empty() && body.is_empty() {
                if flow.body_unread == 0 {
                    return Ok(self.uploaded(token, client));
                }
                match read_body(
                    &mut client.link,
                    &mut client.buffers,
                    flow,
                    &mut client.req_len,
                ) {
                    Read::Data(_) => progressed = true,
                    Read::Block => {
                        let limit = client.timeouts.request;
                        self.wait_for(token, client, Wait::ClientRead, limit);
                        return Ok(progressed);
                    }
                    Read::Eof | Read::Failed => return Err(Failure::ClientGone),
                }
                continue;
            }
            let Some(target) = self.slab.upstream_mut(upstream) else {
                return Err(Failure::UpstreamClosed);
            };
            match target.link.send((head, body), Delivery::Batched) {
                Wrote::Bytes(sent) => {
                    let from_head = sent.min(head.len());
                    flow.head_pos += from_head;
                    flow.window.consume(sent - from_head);
                    flow.received += (sent - from_head) as u64;
                    progressed = true;
                    if sent < head.len() + body.len() {
                        {
                            self.upload_blocked(token, client);
                            return Ok(true);
                        }
                    }
                }
                Wrote::Block => {
                    self.upload_blocked(token, client);
                    return Ok(progressed);
                }
                Wrote::Failed => {
                    flow.upload_failed = true;
                    let limit = flow.timeouts.map(|timeouts| timeouts.header);
                    self.wait_for(token, client, Wait::Header, limit);
                    return Ok(true);
                }
            }
        }
    }

    fn upload_blocked(&mut self, token: Token, client: &mut Client<S>) {
        let limit = client.flow().timeouts.map(|timeouts| timeouts.write);
        self.wait_for(token, client, Wait::UpstreamWrite, limit);
    }

    fn uploaded(&mut self, token: Token, client: &mut Client<S>) -> bool {
        let flow = client.flow();
        flow.upload_done = true;
        let limit = flow.timeouts.map(|timeouts| timeouts.header);
        self.wait_for(token, client, Wait::Header, limit);
        true
    }

    fn download(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<bool, Failure> {
        match client.flow().down {
            Down::Awaiting => self.await_head(token, client, upstream),
            Down::Relaying(_) => self.relay(token, client, upstream),
            Down::Finished { .. } => Ok(false),
        }
    }

    fn relay(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<bool, Failure> {
        let mut progressed = false;
        loop {
            match self.splice_step(token, client, upstream)? {
                Spliced::Progress => {
                    progressed = true;
                    continue;
                }
                Spliced::Blocked => return Ok(progressed),
                Spliced::Buffered => {}
            }
            let Down::Relaying(relay) = &client.flow().down else {
                return Ok(progressed);
            };
            let fetching = relay.cargo.is_empty() && !relay.complete;
            if fetching && self.pull(token, client, upstream)? == Pulled::Data {
                progressed = true;
            }
            let Client { buffers, flow, .. } = &mut *client;
            let flow = flow.as_mut().expect("a serving client has a flow");
            let Down::Relaying(relay) = &mut flow.down else {
                return Ok(progressed);
            };
            if relay.head_pos < buffers.down_head.len() || !relay.cargo.is_empty() {
                match self.push(token, client, upstream)? {
                    Pushed::All => progressed = true,
                    Pushed::Part => return Ok(true),
                    Pushed::Nothing => return Ok(progressed),
                }
                continue;
            }
            if !relay.complete {
                return Ok(progressed);
            }
            flow.down = Down::Finished {
                reusable: relay.reusable,
            };
            return Ok(true);
        }
    }

    fn push(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<Pushed, Failure> {
        let Client {
            link,
            buffers,
            flow,
            ..
        } = &mut *client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let delivery = Delivery::of(flow.keepalive);
        let Down::Relaying(relay) = &mut flow.down else {
            return Ok(Pushed::Nothing);
        };
        let head = &buffers.down_head[relay.head_pos..];
        let wrote = match relay.cargo {
            Cargo::Held(window) => {
                link.send((head, &buffers.resp[window.start..window.end]), delivery)
            }
            Cargo::Lent(count) => {
                let Some(origin) = self.slab.upstream_mut(upstream) else {
                    return Err(Failure::UpstreamClosed);
                };
                link.forward(&mut origin.link, (head, count), delivery)
            }
        };
        let total = head.len() + relay.cargo.len();
        match wrote {
            Wrote::Bytes(sent) => {
                let from_head = sent.min(head.len());
                relay.head_pos += from_head;
                relay.cargo.consume(sent - from_head);
                if sent == total {
                    return Ok(Pushed::All);
                }
                self.relay_blocked(token, client);
                Ok(Pushed::Part)
            }
            Wrote::Block => {
                self.relay_blocked(token, client);
                Ok(Pushed::Nothing)
            }
            Wrote::Failed => Err(Failure::ClientGone),
        }
    }

    fn pull(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<Pulled, Failure> {
        let Some(origin) = self.slab.upstream_mut(upstream) else {
            return Err(Failure::UpstreamClosed);
        };
        let Client { buffers, flow, .. } = &mut *client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let Down::Relaying(relay) = &mut flow.down else {
            return Ok(Pulled::Blocked);
        };
        let pulled = if origin.link.lends() && !matches!(relay.mode, Mode::UntilClose) {
            pull_lent(&mut origin.link, relay, &mut flow.sent)?
        } else {
            pull_copied(&mut origin.link, relay, buffers, &mut flow.sent)?
        };
        let awaiting_body = flow.wait == Wait::Body;
        if pulled == Pulled::Data || (pulled == Pulled::Blocked && !awaiting_body) {
            let limit = flow.timeouts.map(|timeouts| timeouts.body);
            self.wait_for(token, client, Wait::Body, limit);
        }
        Ok(pulled)
    }

    pub(super) fn relay_blocked(&mut self, token: Token, client: &mut Client<S>) {
        let limit = client.timeouts.write;
        self.wait_for(token, client, Wait::ClientWrite, limit);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pushed {
    All,
    Part,
    Nothing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Pulled {
    Data,
    Blocked,
    Ended,
}

pub(crate) fn pull_lent(
    link: &mut Link,
    relay: &mut Relay,
    sent: &mut u64,
) -> Result<Pulled, Failure> {
    match link.peek(|bytes| relay.frame(bytes)) {
        Peek::Held(framed) => {
            let take = framed?;
            relay.cargo = Cargo::Lent(take);
            *sent += take as u64;
            Ok(Pulled::Data)
        }
        Peek::Block => Ok(Pulled::Blocked),
        Peek::Eof | Peek::Failed => Err(Failure::UpstreamClosed),
    }
}

fn pull_copied(
    link: &mut Link,
    relay: &mut Relay,
    buffers: &mut Buffers,
    sent: &mut u64,
) -> Result<Pulled, Failure> {
    let start = if matches!(relay.mode, Mode::UntilClose) {
        HEADER_ROOM
    } else {
        0
    };
    match buffers.resp.recv(link, start, usize::MAX) {
        Read::Data(count) => {
            absorb(
                relay,
                &mut buffers.resp,
                Window::new(start, start + count),
                sent,
            )?;
            Ok(Pulled::Data)
        }
        Read::Block => Ok(Pulled::Blocked),
        Read::Eof if matches!(relay.mode, Mode::UntilClose) => {
            buffers.resp[..LAST_CHUNK.len()].copy_from_slice(LAST_CHUNK);
            relay.cargo = Cargo::Held(Window::new(0, LAST_CHUNK.len()));
            relay.complete = true;
            Ok(Pulled::Ended)
        }
        Read::Eof | Read::Failed => Err(Failure::UpstreamClosed),
    }
}

fn read_body<S: Service>(
    link: &mut Link,
    buffers: &mut Buffers,
    flow: &mut Flow<S>,
    req_len: &mut usize,
) -> Read {
    let capacity = buffers.req.limit();
    let (start, room) = if flow.replayable {
        (*req_len, capacity - *req_len)
    } else {
        (0, capacity)
    };
    let room = usize::try_from(flow.body_unread).map_or(room, |unread| room.min(unread));
    let read = buffers.req.recv(link, start, room);
    if let Read::Data(count) = read {
        flow.body_unread -= count as u64;
        flow.window = Window::new(start, start + count);
        if flow.replayable {
            *req_len += count;
            flow.request_end = *req_len;
        }
    }
    read
}

impl Relay {
    pub(crate) fn frame(&mut self, data: &[u8]) -> Result<usize, Failure> {
        let count = data.len();
        let take = match &mut self.mode {
            Mode::Empty => {
                self.complete = true;
                0
            }
            Mode::Length(remaining) => {
                let take = usize::try_from(*remaining).map_or(count, |left| left.min(count));
                *remaining -= take as u64;
                self.complete = *remaining == 0;
                take
            }
            Mode::Chunked(scanner) => {
                let progress = scanner.feed(data).map_err(|_| Failure::BadResponse)?;
                self.complete = progress != ChunkStep::Pending;
                match progress {
                    ChunkStep::Done(end) => end,
                    ChunkStep::Pending => count,
                }
            }
            Mode::UntilClose => count,
        };
        self.reusable &= take == count;
        Ok(take)
    }
}

pub(crate) fn absorb(
    relay: &mut Relay,
    resp: &mut [u8],
    data: Window,
    sent: &mut u64,
) -> Result<(), Failure> {
    let count = data.end - data.start;
    if matches!(relay.mode, Mode::UntilClose) {
        relay.cargo = Cargo::Held(wrap_until_close(resp, data));
        *sent += count as u64;
        return Ok(());
    }
    let take = relay.frame(&resp[data.start..data.end])?;
    relay.cargo = Cargo::Held(Window::new(data.start, data.start + take));
    *sent += take as u64;
    Ok(())
}

fn wrap_until_close(resp: &mut [u8], data: Window) -> Window {
    let count = data.end - data.start;
    if count == 0 {
        return Window::new(data.start, data.start);
    }
    let start = write_chunk_header(resp, data.start, count);
    resp[data.end..data.end + TAIL_ROOM].copy_from_slice(b"\r\n");
    Window::new(start, data.end + TAIL_ROOM)
}
