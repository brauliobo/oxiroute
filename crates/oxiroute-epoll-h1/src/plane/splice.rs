use super::{
    client::{Client, Down, Mode, Relay, Wait},
    link::Link,
    pipes::{Carry, Piped, unread},
    reactor::Reactor,
    service::{Failure, Service},
    slab::Token,
    sys::{Read, Wrote},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Spliced {
    Buffered,
    Progress,
    Blocked,
}

enum Move {
    Drained,
    Filled,
    Done,
    ClientBlocked,
    OriginBlocked,
    ClientGone,
    OriginGone,
}

impl<S: Service> Reactor<S> {
    pub(crate) fn splice_step(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Result<Spliced, Failure> {
        if !self.piped(client) {
            return Ok(Spliced::Buffered);
        }
        let Some(origin) = self.slab.upstream_mut(upstream) else {
            return Err(Failure::UpstreamClosed);
        };
        let Client { link, flow, .. } = &mut *client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let Down::Relaying(Relay {
            carry: Carry::Piped(piped),
            ..
        }) = &mut flow.down
        else {
            return Ok(Spliced::Buffered);
        };
        match advance(piped, &mut flow.sent, link, &mut origin.link) {
            Move::Drained => Ok(Spliced::Progress),
            Move::Filled => {
                self.await_body(token, client);
                Ok(Spliced::Progress)
            }
            Move::Done => {
                self.finish_piped(client, upstream);
                Ok(Spliced::Progress)
            }
            Move::ClientBlocked => {
                self.relay_blocked(token, client);
                Ok(Spliced::Blocked)
            }
            Move::OriginBlocked => {
                self.await_origin(token, client);
                Ok(Spliced::Blocked)
            }
            Move::ClientGone => Err(Failure::ClientGone),
            Move::OriginGone => Err(Failure::UpstreamClosed),
        }
    }

    fn piped(&mut self, client: &mut Client<S>) -> bool {
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        let Down::Relaying(relay) = &mut flow.down else {
            return false;
        };
        if !relay.flushed(&client.buffers.down_head) {
            return false;
        }
        if matches!(relay.carry, Carry::Undecided) {
            relay.carry = self.carry_of(relay.mode);
        }
        matches!(relay.carry, Carry::Piped(_))
    }

    fn carry_of(&mut self, mode: Mode) -> Carry {
        match (self.pipes.as_mut(), mode) {
            (Some(pipes), Mode::Length(left)) if pipes.wants(left) => pipes
                .lease()
                .map_or(Carry::Buffered, |pipe| Carry::Piped(Piped { pipe, left })),
            _ => Carry::Buffered,
        }
    }

    fn await_body(&mut self, token: Token, client: &mut Client<S>) {
        let limit = client.flow().timeouts.map(|timeouts| timeouts.body);
        self.wait_for(token, client, Wait::Body, limit);
    }

    fn await_origin(&mut self, token: Token, client: &mut Client<S>) {
        if client.flow().wait != Wait::Body {
            self.await_body(token, client);
        }
    }

    fn finish_piped(&mut self, client: &mut Client<S>, upstream: Token) {
        let Some(origin) = self.slab.upstream_mut(upstream) else {
            return;
        };
        let drained = origin.link.ready.read && matches!(unread(&origin.link.fd), Read::Block);
        let Down::Relaying(relay) = &mut client.flow().down else {
            return;
        };
        origin.link.ready.read &= !drained;
        relay.reusable &= !origin.link.ready.read;
        relay.complete = true;
        if let (Carry::Piped(piped), Some(pipes)) = (
            std::mem::replace(&mut relay.carry, Carry::Buffered),
            self.pipes.as_mut(),
        ) {
            pipes.give_back(piped.pipe);
        }
    }
}

impl Relay {
    fn flushed(&self, down_head: &[u8]) -> bool {
        self.head_pos >= down_head.len() && self.window.is_empty() && !self.complete
    }
}

fn advance(piped: &mut Piped, sent: &mut u64, client: &mut Link, origin: &mut Link) -> Move {
    if piped.pipe.pending() > 0 {
        drain(piped, sent, client)
    } else if piped.left > 0 {
        fill(piped, origin)
    } else {
        Move::Done
    }
}

fn drain(piped: &mut Piped, sent: &mut u64, client: &mut Link) -> Move {
    if !client.ready.write {
        return Move::ClientBlocked;
    }
    match piped.pipe.drain(&client.fd) {
        Wrote::Bytes(count) => {
            *sent += count as u64;
            client.ready.write = piped.pipe.pending() == 0;
            Move::Drained
        }
        Wrote::Block => {
            client.ready.write = false;
            Move::ClientBlocked
        }
        Wrote::Failed => Move::ClientGone,
    }
}

fn fill(piped: &mut Piped, origin: &mut Link) -> Move {
    if !origin.ready.read {
        return Move::OriginBlocked;
    }
    match piped.pipe.fill(&origin.fd, piped.left) {
        Read::Data(count) => {
            piped.left -= count as u64;
            Move::Filled
        }
        Read::Block => {
            origin.ready.read = false;
            Move::OriginBlocked
        }
        Read::Eof | Read::Failed => Move::OriginGone,
    }
}
