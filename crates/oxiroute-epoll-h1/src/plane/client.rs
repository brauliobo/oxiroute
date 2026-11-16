use std::{
    net::SocketAddr,
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use oxiroute_h1_head::RequestHead;

use super::{
    chunked::ChunkScanner,
    clock::Moment,
    config::{Config, Timeouts},
    link::Link,
    pipes::{Carry, SpliceUse},
    service::{AttemptTimeouts, Failure, Reuse, Service},
    slab::Token,
};

pub(crate) type Shared<S> = Rc<Arc<S>>;

pub(crate) struct Buffers {
    pub(crate) req: Box<[u8]>,
    pub(crate) resp: Box<[u8]>,
    pub(crate) up_head: Vec<u8>,
    pub(crate) down_head: Vec<u8>,
    pub(crate) head: Box<RequestHead>,
}

impl Buffers {
    pub(crate) fn new(config: &Config) -> Self {
        let head_capacity = config.head_limit + HEAD_GROWTH;
        Self {
            req: vec![0; config.request_buffer].into_boxed_slice(),
            resp: vec![0; config.response_buffer].into_boxed_slice(),
            up_head: Vec::with_capacity(head_capacity),
            down_head: Vec::with_capacity(head_capacity),
            head: Box::new(RequestHead::new()),
        }
    }
}

pub(crate) const HEAD_GROWTH: usize = 4096;

pub(crate) struct Pooled(Box<Buffers>);

impl Pooled {
    pub(crate) fn new(config: &Config) -> Self {
        Self(Box::new(Buffers::new(config)))
    }
}

pub(crate) struct Leased(Option<Pooled>);

impl Leased {
    pub(crate) const fn none() -> Self {
        Self(None)
    }

    pub(crate) fn hold(&mut self, buffers: Pooled) {
        self.0 = Some(buffers);
    }

    pub(crate) fn release(&mut self) -> Option<Pooled> {
        self.0.take()
    }
}

impl Deref for Leased {
    type Target = Buffers;

    fn deref(&self) -> &Buffers {
        &self.0.as_ref().expect("a client past Idle holds buffers").0
    }
}

impl DerefMut for Leased {
    fn deref_mut(&mut self) -> &mut Buffers {
        &mut self.0.as_mut().expect("a client past Idle holds buffers").0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Window {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Window {
    pub(crate) const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub(crate) fn consume(&mut self, count: usize) {
        self.start += count;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    Idle,
    Head,
    Serving,
    Answering,
    Lingering,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Wait {
    Connect,
    UpstreamWrite,
    Header,
    Body,
    ClientRead,
    ClientWrite,
}

impl Wait {
    pub(crate) const fn failure(self) -> Failure {
        match self {
            Self::Connect => Failure::ConnectTimeout,
            Self::UpstreamWrite => Failure::UpstreamWrite,
            Self::Header => Failure::HeaderTimeout,
            Self::Body => Failure::BodyTimeout,
            Self::ClientRead | Self::ClientWrite => Failure::ClientTimeout,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Mode {
    Empty,
    Length(u64),
    Chunked(ChunkScanner),
    UntilClose,
}

pub(crate) struct Relay {
    pub(crate) mode: Mode,
    pub(crate) window: Window,
    pub(crate) head_pos: usize,
    pub(crate) complete: bool,
    pub(crate) reusable: bool,
    pub(crate) carry: Carry,
}

pub(crate) enum Down {
    Awaiting,
    Relaying(Relay),
    Finished { reusable: bool },
}

pub(crate) struct Flow<S: Service> {
    pub(crate) exchange: Option<S::Exchange>,
    pub(crate) upstream: Option<Token>,
    pub(crate) head_len: usize,
    pub(crate) body_unread: u64,
    pub(crate) window: Window,
    pub(crate) head_pos: usize,
    pub(crate) request_end: usize,
    pub(crate) replayable: bool,
    pub(crate) is_head: bool,
    pub(crate) client_keepalive: bool,
    pub(crate) keepalive: bool,
    pub(crate) attempts: u8,
    pub(crate) reused: bool,
    pub(crate) key: u64,
    pub(crate) reuse: Reuse,
    pub(crate) timeouts: Option<AttemptTimeouts>,
    pub(crate) wait: Wait,
    pub(crate) total: Option<Moment>,
    pub(crate) started: Instant,
    pub(crate) attempt_started: Moment,
    pub(crate) connected_at: Option<Moment>,
    pub(crate) first_byte_at: Option<Moment>,
    pub(crate) down: Down,
    pub(crate) status: Option<u16>,
    pub(crate) failure: Option<Failure>,
    pub(crate) received: u64,
    pub(crate) sent: u64,
    pub(crate) upload_done: bool,
    pub(crate) upload_failed: bool,
    pub(crate) contacted: bool,
    pub(crate) fresh: bool,
    pub(crate) splice: SpliceUse,
}

pub(crate) struct Client<S: Service> {
    pub(crate) link: Link,
    pub(crate) service: Shared<S>,
    pub(crate) timeouts: Timeouts,
    pub(crate) listener: usize,
    pub(crate) peer: SocketAddr,
    pub(crate) admission: S::Admission,
    pub(crate) served: u32,
    pub(crate) stage: Stage,
    pub(crate) deadline: Option<Moment>,
    pub(crate) buffers: Leased,
    pub(crate) req_len: usize,
    pub(crate) resp_len: usize,
    pub(crate) flow: Option<Flow<S>>,
}

impl<S: Service> Client<S> {
    pub(crate) fn flow(&mut self) -> &mut Flow<S> {
        self.flow.as_mut().expect("a serving client has a flow")
    }
}
