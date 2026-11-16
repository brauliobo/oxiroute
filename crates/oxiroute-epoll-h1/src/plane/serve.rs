use std::{net::TcpStream, rc::Rc};

use oxiroute_h1_head::{Connection, RequestHead, Scan, scan_request};
use oxiroute_uring::Released;

use super::{
    client::{Buffers, Client, Down, Flow, Stage, Window},
    reactor::{Reactor, Step},
    service::{Handoff, Request, Reuse, Routed, Service},
    slab::Token,
    sys::{Read, Wrote},
};

fn servable(head: &RequestHead) -> bool {
    let facts = head.facts;
    !facts.transfer_encoding
        && !facts.upgrade
        && !facts.expect
        && facts.connection != Connection::Other
}

impl<S: Service> Reactor<S> {
    pub(crate) fn drive(&mut self, token: Token, client: &mut Client<S>) -> Step {
        loop {
            let step = match client.stage {
                Stage::Idle | Stage::Head => self.read_request(token, client),
                Stage::Serving => self.pump(token, client),
                Stage::Answering => self.flush_answer(token, client),
                Stage::Lingering => Self::linger(client),
            };
            if step != Step::Again {
                return step;
            }
        }
    }

    fn read_request(&mut self, token: Token, client: &mut Client<S>) -> Step {
        if client.stage == Stage::Idle {
            return self.begin_request(token, client);
        }
        if client.req_len > 0
            && let Some(step) = self.parse(client)
        {
            return step;
        }
        self.fill_request(token, client)
    }

    fn begin_request(&mut self, token: Token, client: &mut Client<S>) -> Step {
        if !client.link.ready.read {
            return Step::Wait;
        }
        self.lease_buffers(client);
        match self.fill_request(token, client) {
            Step::Wait => {
                self.release_buffers(client);
                Step::Wait
            }
            step => step,
        }
    }

    fn parse(&mut self, client: &mut Client<S>) -> Option<Step> {
        let buffers: &mut Buffers = &mut client.buffers;
        match scan_request(&buffers.req[..client.req_len], &mut buffers.head) {
            Scan::Partial if client.req_len >= self.config.head_limit => Some(Step::Handoff),
            Scan::Partial => None,
            Scan::Miss(_) => Some(Step::Handoff),
            Scan::Head => Some(self.begin(client)),
        }
    }

    fn fill_request(&mut self, token: Token, client: &mut Client<S>) -> Step {
        let buffers = &mut client.buffers;
        match client.link.recv(&mut buffers.req[client.req_len..]) {
            Read::Data(count) => {
                client.req_len += count;
                if client.stage == Stage::Idle {
                    client.stage = Stage::Head;
                    let deadline = client
                        .timeouts
                        .request
                        .map(|limit| self.moment.after(limit));
                    self.set_deadline(token, deadline, &mut client.deadline);
                }
                Step::Again
            }
            Read::Block => Step::Wait,
            Read::Eof | Read::Failed => Step::Close,
        }
    }

    fn begin(&mut self, client: &mut Client<S>) -> Step {
        let head = &client.buffers.head;
        if head.head_len as usize > self.config.head_limit || !servable(head) {
            return Step::Handoff;
        }
        let service = Rc::clone(&client.service);
        let request = Request::new(&client.buffers.req[..client.req_len], head, client.peer);
        let Routed::Serve(exchange) = service.route(&request, self.now) else {
            return Step::Handoff;
        };
        let keepalive = !request.close() && self.drain_deadline.is_none();
        let flow = self.flow(&request, exchange, client.buffers.req.len(), keepalive);
        client.flow = Some(flow);
        client.stage = Stage::Serving;
        Step::Again
    }

    fn flow(
        &self,
        request: &Request<'_>,
        exchange: S::Exchange,
        capacity: usize,
        keepalive: bool,
    ) -> Flow<S> {
        let head_len = request.head().head_len as usize;
        let body_len = request.body_len();
        let buffered = request.bytes().len() - head_len;
        let in_buffer = usize::try_from(body_len).map_or(buffered, |len| len.min(buffered));
        Flow {
            exchange: Some(exchange),
            upstream: None,
            head_len,
            body_unread: body_len - in_buffer as u64,
            window: Window::new(head_len, head_len + in_buffer),
            head_pos: 0,
            request_end: head_len + in_buffer,
            replayable: (head_len as u64 + body_len) <= capacity as u64,
            is_head: request.is_head(),
            client_keepalive: keepalive,
            keepalive,
            attempts: 0,
            reused: false,
            key: 0,
            reuse: Reuse::Never,
            timeouts: None,
            wait: super::client::Wait::Connect,
            total: None,
            started: self.now,
            attempt_started: self.moment,
            connected_at: None,
            first_byte_at: None,
            down: Down::Awaiting,
            status: None,
            failure: None,
            received: 0,
            sent: 0,
            upload_done: false,
            upload_failed: false,
            contacted: false,
            fresh: false,
        }
    }

    pub(crate) fn handoff(&mut self, token: Token, mut client: Client<S>) {
        self.wheel.cancel(token.index);
        self.slab.remove(token);
        self.live -= 1;
        let mut prefix = client.buffers.req[..client.req_len].to_vec();
        self.release_buffers(&mut client);
        let Client {
            link,
            service,
            peer,
            admission,
            listener,
            served,
            ..
        } = client;
        let Ok(Released { fd, unread }) = self.driver.release(link) else {
            return;
        };
        prefix.extend(unread);
        service.handoff(Handoff {
            stream: TcpStream::from(fd),
            prefix,
            peer,
            listener,
            admission,
            requests_served: served,
        });
    }

    fn flush_answer(&mut self, token: Token, client: &mut Client<S>) -> Step {
        let flow = client
            .flow
            .as_mut()
            .expect("an answering client has a flow");
        let pending = &client.buffers.down_head[flow.head_pos..];
        match client.link.send(pending, &[]) {
            Wrote::Bytes(count) => {
                flow.head_pos += count;
                if flow.head_pos < client.buffers.down_head.len() {
                    return Step::Wait;
                }
                self.complete(token, client)
            }
            Wrote::Block => Step::Wait,
            Wrote::Failed => Step::Close,
        }
    }

    fn linger(client: &mut Client<S>) -> Step {
        loop {
            match client.link.recv(&mut client.buffers.req) {
                Read::Data(_) => {}
                Read::Block => return Step::Wait,
                Read::Eof | Read::Failed => return Step::Close,
            }
        }
    }
}
