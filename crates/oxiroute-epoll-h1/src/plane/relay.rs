use super::{
    chunked::{HEADER_ROOM, LAST_CHUNK, Step as ChunkStep, write_chunk_header},
    client::{Buffers, Client, Down, Flow, Mode, RESPONSE_TAIL, Relay, Wait, Window},
    link::Link,
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
            match target.link.send(head, body) {
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
            let Client {
                link,
                buffers,
                flow,
                ..
            } = &mut *client;
            let flow = flow.as_mut().expect("a serving client has a flow");
            let Down::Relaying(relay) = &mut flow.down else {
                return Ok(progressed);
            };
            let pending = relay.head_pos < buffers.down_head.len() || !relay.window.is_empty();
            if pending {
                let head = &buffers.down_head[relay.head_pos..];
                let body = &buffers.resp[relay.window.start..relay.window.end];
                match link.send(head, body) {
                    Wrote::Bytes(sent) => {
                        let from_head = sent.min(head.len());
                        relay.head_pos += from_head;
                        relay.window.consume(sent - from_head);
                        progressed = true;
                        if sent < head.len() + body.len() {
                            {
                                self.relay_blocked(token, client);
                                return Ok(true);
                            }
                        }
                    }
                    Wrote::Block => {
                        self.relay_blocked(token, client);
                        return Ok(progressed);
                    }
                    Wrote::Failed => return Err(Failure::ClientGone),
                }
                continue;
            }
            if relay.complete {
                flow.down = Down::Finished {
                    reusable: relay.reusable,
                };
                return Ok(true);
            }
            let Some(origin) = self.slab.upstream_mut(upstream) else {
                return Err(Failure::UpstreamClosed);
            };
            let first = if matches!(relay.mode, Mode::UntilClose) {
                HEADER_ROOM
            } else {
                0
            };
            let read = buffers.resp.recv(&mut origin.link, first, usize::MAX);
            match read {
                Read::Data(count) => {
                    absorb(
                        relay,
                        &mut buffers.resp,
                        Window::new(first, first + count),
                        &mut flow.sent,
                    )?;
                    progressed = true;
                    let limit = flow.timeouts.map(|timeouts| timeouts.body);
                    self.wait_for(token, client, Wait::Body, limit);
                }
                Read::Block => {
                    let limit = flow.timeouts.map(|timeouts| timeouts.body);
                    self.wait_for(token, client, Wait::Body, limit);
                    return Ok(progressed);
                }
                Read::Eof if matches!(relay.mode, Mode::UntilClose) => {
                    buffers.resp[..LAST_CHUNK.len()].copy_from_slice(LAST_CHUNK);
                    relay.window = Window::new(0, LAST_CHUNK.len());
                    relay.complete = true;
                }
                Read::Eof | Read::Failed => return Err(Failure::UpstreamClosed),
            }
        }
    }

    pub(super) fn relay_blocked(&mut self, token: Token, client: &mut Client<S>) {
        let limit = client.timeouts.write;
        self.wait_for(token, client, Wait::ClientWrite, limit);
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

pub(crate) fn absorb(
    relay: &mut Relay,
    resp: &mut [u8],
    data: Window,
    sent: &mut u64,
) -> Result<(), Failure> {
    let count = data.end - data.start;
    match &mut relay.mode {
        Mode::Empty => {
            relay.window = Window::new(data.start, data.start);
            relay.complete = true;
            relay.reusable &= count == 0;
        }
        Mode::Length(remaining) => {
            let take = usize::try_from(*remaining).map_or(count, |remaining| remaining.min(count));
            *remaining -= take as u64;
            relay.window = Window::new(data.start, data.start + take);
            relay.complete = *remaining == 0;
            relay.reusable &= take == count;
            *sent += take as u64;
        }
        Mode::Chunked(scanner) => {
            let progress = scanner
                .feed(&resp[data.start..data.end])
                .map_err(|_| Failure::BadResponse)?;
            let take = match progress {
                ChunkStep::Done(end) => end,
                ChunkStep::Pending => count,
            };
            relay.window = Window::new(data.start, data.start + take);
            relay.complete = progress != ChunkStep::Pending;
            relay.reusable &= take == count;
            *sent += take as u64;
        }
        Mode::UntilClose => {
            relay.window = if count == 0 {
                Window::new(data.start, data.start)
            } else {
                let start = write_chunk_header(resp, data.start, count);
                resp[data.end..data.end + TAIL_ROOM].copy_from_slice(b"\r\n");
                Window::new(start, data.end + TAIL_ROOM)
            };
            *sent += count as u64;
        }
    }
    Ok(())
}
