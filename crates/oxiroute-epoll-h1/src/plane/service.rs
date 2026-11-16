use std::{
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

use oxiroute_h1_head::{Connection, RequestHead, ResponseHead};

use super::wake::Waker;

pub struct Request<'a> {
    bytes: &'a [u8],
    head: &'a RequestHead,
    peer: SocketAddr,
    waker: &'a Waker,
    replayable: bool,
}

impl<'a> Request<'a> {
    pub(crate) const fn new(
        bytes: &'a [u8],
        head: &'a RequestHead,
        peer: SocketAddr,
        (waker, replayable): (&'a Waker, bool),
    ) -> Self {
        Self {
            bytes,
            head,
            peer,
            waker,
            replayable,
        }
    }

    #[must_use]
    pub const fn waker(&self) -> &'a Waker {
        self.waker
    }

    #[must_use]
    pub const fn replayable(&self) -> bool {
        self.replayable
    }

    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    #[must_use]
    pub const fn head(&self) -> &'a RequestHead {
        self.head
    }

    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }

    #[must_use]
    pub fn method(&self) -> &'a [u8] {
        self.head.method(self.bytes)
    }

    #[must_use]
    pub fn target(&self) -> &'a [u8] {
        self.head.target(self.bytes)
    }

    #[must_use]
    pub fn host(&self) -> &'a [u8] {
        self.head.host(self.bytes)
    }

    #[must_use]
    pub fn body_len(&self) -> u64 {
        self.head.facts.content_length.unwrap_or(0)
    }

    #[must_use]
    pub fn is_head(&self) -> bool {
        self.method() == b"HEAD"
    }

    #[must_use]
    pub fn close(&self) -> bool {
        self.head.facts.connection == Connection::Close
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Framing {
    Empty,
    Length,
    Chunked,
}

pub struct Response<'a> {
    pub bytes: &'a [u8],
    pub head: &'a ResponseHead,
    pub framing: Framing,
    pub keepalive: bool,
    pub request_is_head: bool,
}

pub struct Answered<'a> {
    pub bytes: &'a [u8],
    pub head: &'a ResponseHead,
    pub connected: Option<Instant>,
    pub now: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    NoServer,
    Connect,
    ConnectTimeout,
    UpstreamWrite,
    UpstreamClosed,
    UnexpectedStatus,
    HeaderTimeout,
    BodyTimeout,
    TotalTimeout,
    BadResponse,
    ResponseHeadTooLarge,
    RequestHeadTooLarge,
    ClientGone,
    ClientTimeout,
    Shutdown,
    NoCapacity,
    Cancelled,
}

impl Failure {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoServer => "no_server",
            Self::Connect => "connect",
            Self::ConnectTimeout => "connect_timeout",
            Self::UpstreamWrite => "upstream_write",
            Self::UpstreamClosed => "upstream_closed",
            Self::UnexpectedStatus => "unexpected_status",
            Self::HeaderTimeout => "header_timeout",
            Self::BodyTimeout => "body_timeout",
            Self::TotalTimeout => "total_timeout",
            Self::BadResponse => "bad_response",
            Self::ResponseHeadTooLarge => "response_head_too_large",
            Self::RequestHeadTooLarge => "request_head_too_large",
            Self::ClientGone => "client_gone",
            Self::ClientTimeout => "client_timeout",
            Self::Shutdown => "shutdown",
            Self::NoCapacity => "no_capacity",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reuse {
    Never,
    Pooled { idle: Duration },
}

#[derive(Clone, Copy, Debug)]
pub struct AttemptTimeouts {
    pub connect: Duration,
    pub header: Duration,
    pub body: Duration,
    pub write: Duration,
    pub deadline: Option<Instant>,
}

pub trait Lease {
    fn connecting(&self) -> bool;

    fn idle(&self, idle: bool);
}

pub struct Target<L> {
    pub address: SocketAddr,
    pub key: u64,
    pub reuse: Reuse,
    pub lease: L,
    pub timeouts: AttemptTimeouts,
}

pub enum Selected<L> {
    Server(Target<L>),
    Refused(Failure),
    Handoff,
}

pub enum Routed<E> {
    Serve(E),
    Handoff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recovery {
    Retry,
    Answer,
}

#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    pub status: Option<u16>,
    pub failure: Option<Failure>,
    pub attempts: u8,
    pub received: u64,
    pub sent: u64,
    pub started: Instant,
    pub ended: Instant,
    pub connect: Option<Duration>,
    pub header: Option<Duration>,
    pub keepalive: bool,
}

pub struct Handoff<A> {
    pub stream: TcpStream,
    pub prefix: Vec<u8>,
    pub peer: SocketAddr,
    pub listener: usize,
    pub admission: A,
    pub requests_served: u32,
}

pub trait Service: Send + Sync + 'static {
    type Admission: Send + 'static;
    type Exchange;
    type Lease: Lease;

    fn admit(&self, peer: SocketAddr) -> Option<Self::Admission>;

    fn route(&self, request: &Request<'_>, now: Instant) -> Routed<Self::Exchange>;

    fn select(&self, exchange: &mut Self::Exchange, now: Instant) -> Selected<Self::Lease>;

    fn render_request(&self, exchange: &Self::Exchange, request: &Request<'_>, out: &mut Vec<u8>);

    fn render_response(
        &self,
        exchange: &Self::Exchange,
        response: &Response<'_>,
        now: Instant,
        out: &mut Vec<u8>,
    );

    fn judge(&self, exchange: &mut Self::Exchange, answered: &Answered<'_>) -> Recovery;

    fn recover(&self, exchange: &mut Self::Exchange, failure: Failure, now: Instant) -> Recovery;

    fn answer(
        &self,
        exchange: &mut Self::Exchange,
        failure: Failure,
        request_is_head: bool,
        out: &mut Vec<u8>,
    ) -> Option<u16>;

    fn cancelled(&self, exchange: &Self::Exchange) -> bool;

    fn finish(&self, exchange: Self::Exchange, outcome: &Outcome);

    fn handoff(&self, handoff: Handoff<Self::Admission>);
}
