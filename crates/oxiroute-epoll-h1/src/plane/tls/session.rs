use std::{
    io::{self, IoSlice, Read as _, Write as _},
    sync::Arc,
};

use rustls::{
    ProtocolVersion, ServerConnection,
    server::{Accepted, AcceptedAlert, Acceptor},
};

use super::{TlsRouter, transport::Transport};
use crate::plane::{
    service::Secured,
    sys::{Read, Wrote},
};

const NO_APPLICATION_PROTOCOL_ALERT: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x78];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Progress {
    Blocked,
    Established { h1: bool },
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Finish {
    Done,
    Pending,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Drain {
    Done,
    Blocked,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pulled {
    Data,
    Blocked,
    Closed,
    Failed,
}

enum Greeted {
    Connection(Box<ServerConnection>),
    Blocked,
    Failed,
}

struct Hello {
    acceptor: Box<Acceptor>,
    router: Arc<dyn TlsRouter>,
}

struct Live {
    conn: Box<ServerConnection>,
    unread: usize,
    closing: bool,
}

enum Phase {
    Hello(Hello),
    Live(Live),
}

pub(crate) struct Session {
    phase: Phase,
}

impl Session {
    pub(crate) fn new(router: Arc<dyn TlsRouter>) -> Self {
        Self {
            phase: Phase::Hello(Hello {
                acceptor: Box::default(),
                router,
            }),
        }
    }

    pub(crate) fn facts(&self) -> Secured {
        let Phase::Live(live) = &self.phase else {
            unreachable!("a served connection finished its handshake");
        };
        Secured {
            version: match live.conn.protocol_version() {
                Some(ProtocolVersion::TLSv1_3) => "TLS1.3",
                Some(ProtocolVersion::TLSv1_2) => "TLS1.2",
                _ => "TLS",
            },
            sni: live.conn.server_name().map(str::to_owned),
            alpn: live.conn.alpn_protocol().map(<[u8]>::to_vec),
        }
    }

    pub(crate) const fn has_plaintext(&self) -> bool {
        matches!(&self.phase, Phase::Live(live) if live.unread > 0)
    }

    fn live(&mut self) -> &mut Live {
        match &mut self.phase {
            Phase::Live(live) => live,
            Phase::Hello(_) => unreachable!("a served connection finished its handshake"),
        }
    }

    pub(crate) fn advance(&mut self, io: &mut Transport<'_>) -> Progress {
        if let Phase::Hello(hello) = &mut self.phase {
            match hello.greet(io) {
                Greeted::Connection(conn) => self.phase = Phase::Live(Live::new(conn)),
                Greeted::Blocked => return Progress::Blocked,
                Greeted::Failed => return Progress::Failed,
            }
        }
        self.live().handshake(io)
    }

    pub(crate) fn recv(&mut self, io: &mut Transport<'_>, buf: &mut [u8]) -> Read {
        self.live().recv(io, buf)
    }

    pub(crate) fn send(&mut self, io: &mut Transport<'_>, head: &[u8], body: &[u8]) -> Wrote {
        self.live().send(io, head, body)
    }

    pub(crate) fn flush(&mut self, io: &mut Transport<'_>) -> bool {
        match &mut self.phase {
            Phase::Live(live) => live.drain(io) != Drain::Failed,
            Phase::Hello(_) => true,
        }
    }

    pub(crate) fn finish(&mut self, io: &mut Transport<'_>) -> Finish {
        match &mut self.phase {
            Phase::Live(live) => live.finish(io),
            Phase::Hello(_) => Finish::Done,
        }
    }

    pub(crate) fn into_connection(self) -> Option<Box<ServerConnection>> {
        match self.phase {
            Phase::Live(live) => Some(live.conn),
            Phase::Hello(_) => None,
        }
    }
}

impl Hello {
    fn greet(&mut self, io: &mut Transport<'_>) -> Greeted {
        loop {
            match self.acceptor.read_tls(io) {
                Ok(0) => return Greeted::Failed,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Greeted::Blocked;
                }
                Err(_) => return Greeted::Failed,
            }
            match self.acceptor.accept() {
                Ok(None) => {}
                Ok(Some(accepted)) => return self.admit(accepted, io),
                Err((_, alert)) => return refuse(alert, io),
            }
        }
    }

    fn admit(&self, accepted: Accepted, io: &mut Transport<'_>) -> Greeted {
        let Some(config) = self.router.config(&accepted.client_hello()) else {
            let _ = io.write(&NO_APPLICATION_PROTOCOL_ALERT);
            return Greeted::Failed;
        };
        match accepted.into_connection(config) {
            Ok(conn) => Greeted::Connection(Box::new(conn)),
            Err((_, alert)) => refuse(alert, io),
        }
    }
}

fn refuse(mut alert: AcceptedAlert, io: &mut Transport<'_>) -> Greeted {
    while matches!(alert.write(io), Ok(written) if written > 0) {}
    Greeted::Failed
}

impl Live {
    const fn new(conn: Box<ServerConnection>) -> Self {
        Self {
            conn,
            unread: 0,
            closing: false,
        }
    }

    fn handshake(&mut self, io: &mut Transport<'_>) -> Progress {
        loop {
            match self.drain(io) {
                Drain::Failed => return Progress::Failed,
                Drain::Blocked => return Progress::Blocked,
                Drain::Done => {}
            }
            if !self.conn.is_handshaking() {
                return Progress::Established {
                    h1: self.speaks_h1(),
                };
            }
            match self.pull(io) {
                Pulled::Data => {}
                Pulled::Blocked => return Progress::Blocked,
                Pulled::Closed | Pulled::Failed => return Progress::Failed,
            }
        }
    }

    fn speaks_h1(&self) -> bool {
        matches!(self.conn.alpn_protocol(), None | Some(b"http/1.1"))
    }

    fn drain(&mut self, io: &mut Transport<'_>) -> Drain {
        while self.conn.wants_write() {
            match self.conn.write_tls(io) {
                Ok(0) => return Drain::Failed,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Drain::Blocked,
                Err(_) => return Drain::Failed,
            }
        }
        Drain::Done
    }

    fn pull(&mut self, io: &mut Transport<'_>) -> Pulled {
        match self.conn.read_tls(io) {
            Ok(0) => Pulled::Closed,
            Ok(_) => self.process(io),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Pulled::Blocked,
            Err(_) => Pulled::Failed,
        }
    }

    fn process(&mut self, io: &mut Transport<'_>) -> Pulled {
        if let Ok(state) = self.conn.process_new_packets() {
            self.unread = state.plaintext_bytes_to_read();
            return Pulled::Data;
        }
        self.drain(io);
        Pulled::Failed
    }

    fn recv(&mut self, io: &mut Transport<'_>, buf: &mut [u8]) -> Read {
        loop {
            match self.conn.reader().read(buf) {
                Ok(0) => return Read::Eof,
                Ok(count) => {
                    self.unread = self.unread.saturating_sub(count);
                    return Read::Data(count);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Read::Eof,
                Err(_) => return Read::Failed,
            }
            match self.pull(io) {
                Pulled::Data if self.drain(io) == Drain::Failed => return Read::Failed,
                Pulled::Data | Pulled::Closed => {}
                Pulled::Blocked => return Read::Block,
                Pulled::Failed => return Read::Failed,
            }
        }
    }

    fn send(&mut self, io: &mut Transport<'_>, head: &[u8], body: &[u8]) -> Wrote {
        match self.drain(io) {
            Drain::Done => {}
            Drain::Blocked => return Wrote::Block,
            Drain::Failed => return Wrote::Failed,
        }
        let total = head.len() + body.len();
        let mut taken = 0;
        while taken < total {
            let (head, body) = remaining(head, body, taken);
            match self
                .conn
                .writer()
                .write_vectored(&[IoSlice::new(head), IoSlice::new(body)])
            {
                Ok(0) | Err(_) => return Wrote::Failed,
                Ok(count) => taken += count,
            }
            match self.drain(io) {
                Drain::Done => {}
                Drain::Blocked => break,
                Drain::Failed => return Wrote::Failed,
            }
        }
        Wrote::Bytes(taken)
    }

    fn finish(&mut self, io: &mut Transport<'_>) -> Finish {
        if !self.closing {
            self.conn.send_close_notify();
            self.closing = true;
        }
        match self.drain(io) {
            Drain::Done => Finish::Done,
            Drain::Blocked => Finish::Pending,
            Drain::Failed => Finish::Failed,
        }
    }
}

fn remaining<'a>(head: &'a [u8], body: &'a [u8], taken: usize) -> (&'a [u8], &'a [u8]) {
    let from_head = taken.min(head.len());
    (&head[from_head..], &body[taken - from_head..])
}
