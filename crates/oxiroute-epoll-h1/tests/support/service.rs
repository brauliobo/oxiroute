use std::{
    io::Write as _,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use oxiroute_epoll_h1::{
    Answered, AttemptTimeouts, Failure, Framing, Handoff, Lease, Outcome, Recovery, Request,
    Response, Reuse, Routed, Selected, Service, Target, Waker,
};
use oxiroute_h1_head::{
    Automatic, Class, Edits, Name, Value, names, write_request, write_response,
};

use super::date::cached_http_date;

const HOP_BY_HOP: [Class; 9] = [
    Class::Connection,
    Class::KeepAlive,
    Class::ProxyAuthenticate,
    Class::ProxyAuthorization,
    Class::ProxyConnection,
    Class::Te,
    Class::Trailer,
    Class::TransferEncoding,
    Class::Upgrade,
];

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Route {
    pub host: &'static str,
    pub prefix: &'static str,
    pub origin: SocketAddr,
}

#[derive(Clone, Copy)]
pub struct Knobs {
    pub connect: Duration,
    pub header: Duration,
    pub body: Duration,
    pub write: Duration,
    pub total: Option<Duration>,
    pub retries: u8,
    pub retry_status: Option<u16>,
    pub record: bool,
    pub idle_upstream: Duration,
    pub pause_in_route: Option<Duration>,
    pub admit_limit: Option<usize>,
    pub panic_in_route: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            header: Duration::from_secs(5),
            body: Duration::from_secs(5),
            write: Duration::from_secs(5),
            total: None,
            retries: 0,
            retry_status: None,
            record: false,
            idle_upstream: Duration::from_secs(30),
            pause_in_route: None,
            admit_limit: None,
            panic_in_route: false,
        }
    }
}

#[derive(Default)]
pub struct Counters {
    pub completed: AtomicUsize,
    pub failed: AtomicUsize,
    pub admitted: AtomicUsize,
    pub live_admissions: Arc<AtomicUsize>,
    pub live_leases: Arc<AtomicUsize>,
    pub spent_retries: AtomicUsize,
}

pub struct Guard(Arc<AtomicUsize>);

impl Guard {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(counter))
    }
}

impl Lease for Guard {
    fn connecting(&self) -> bool {
        true
    }

    fn idle(&self, _idle: bool) {}
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Exchange {
    id: [u8; 16],
    origin: SocketAddr,
    tries: u8,
}

pub const MOVE: u8 = 1;
pub const KILL: u8 = 2;

pub struct Reference {
    routes: Vec<Route>,
    knobs: Knobs,
    cancel: AtomicU8,
    waker: Mutex<Option<Waker>>,
    pub counters: Counters,
    pub outcomes: Mutex<Vec<Outcome>>,
    pub routed: Mutex<Vec<String>>,
    handoffs: Mutex<Sender<Handoff<Guard>>>,
    paused: Mutex<Sender<()>>,
    clock: (Instant, SystemTime),
}

impl Reference {
    pub fn new(
        routes: Vec<Route>,
        knobs: Knobs,
        (handoffs, paused): (Sender<Handoff<Guard>>, Sender<()>),
    ) -> Self {
        Self {
            routes,
            knobs,
            cancel: AtomicU8::new(0),
            waker: Mutex::new(None),
            counters: Counters::default(),
            outcomes: Mutex::new(Vec::new()),
            routed: Mutex::new(Vec::new()),
            handoffs: Mutex::new(handoffs),
            paused: Mutex::new(paused),
            clock: (Instant::now(), SystemTime::now()),
        }
    }

    pub fn cancel(&self, mode: u8) {
        self.cancel.store(mode, Ordering::SeqCst);
        let waker = self.waker.lock().expect("waker").clone();
        waker.expect("a request was routed").wake();
    }

    fn date(&self, now: Instant) -> [u8; 29] {
        let wall = self.clock.1.duration_since(UNIX_EPOCH).unwrap_or_default();
        cached_http_date(wall.as_secs() + now.saturating_duration_since(self.clock.0).as_secs())
    }

    fn route_of(&self, request: &Request<'_>) -> Option<&Route> {
        let host = request.host();
        let host = host.split(|byte| *byte == b':').next().unwrap_or(host);
        let target = request.target();
        let path = target.split(|byte| *byte == b'?').next().unwrap_or(target);
        self.routes
            .iter()
            .filter(|route| route.host.as_bytes().eq_ignore_ascii_case(host))
            .filter(|route| path.starts_with(route.prefix.as_bytes()))
            .max_by_key(|route| route.prefix.len())
    }
}

fn hex_id() -> [u8; 16] {
    let mut id = [0_u8; 16];
    let mut value = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    for slot in id.iter_mut().rev() {
        *slot = b"0123456789abcdef"[(value & 15) as usize];
        value >>= 4;
    }
    id
}

fn edit(result: Result<(), oxiroute_h1_head::EditsFull>) {
    result.expect("a head needs few edits");
}

impl Service for Reference {
    type Admission = Guard;
    type Exchange = Exchange;
    type Lease = Guard;

    fn admit(&self, _peer: SocketAddr) -> Option<Guard> {
        let live = self.counters.live_admissions.load(Ordering::SeqCst);
        if self.knobs.admit_limit.is_some_and(|limit| live >= limit) {
            return None;
        }
        self.counters.admitted.fetch_add(1, Ordering::Relaxed);
        Some(Guard::new(&self.counters.live_admissions))
    }

    fn route(&self, _admission: &Guard, request: &Request<'_>, _now: Instant) -> Routed<Exchange> {
        assert!(
            !(self.knobs.panic_in_route && request.target() == b"/panic"),
            "injected panic"
        );
        if request.head().facts.hop_by_hop != 0 || request.target().starts_with(b"/handoff") {
            return Routed::Handoff;
        }
        if self.knobs.record {
            let target = String::from_utf8_lossy(request.target()).into_owned();
            self.routed.lock().expect("routed").push(target);
        }
        if let Some(pause) = self
            .knobs
            .pause_in_route
            .filter(|_| request.target().starts_with(b"/pause"))
        {
            let _ = self.paused.lock().expect("paused").send(());
            std::thread::sleep(pause);
        }
        *self.waker.lock().expect("waker") = Some(request.waker().clone());
        match self.route_of(request) {
            Some(route) => Routed::Serve(Exchange {
                id: hex_id(),
                origin: route.origin,
                tries: 0,
            }),
            None => Routed::Handoff,
        }
    }

    fn select(&self, exchange: &mut Exchange, now: Instant) -> Selected<Guard> {
        let knobs = &self.knobs;
        Selected::Server(Target {
            address: exchange.origin,
            key: u64::from(exchange.origin.port()),
            reuse: Reuse::Pooled {
                idle: knobs.idle_upstream,
            },
            lease: Guard::new(&self.counters.live_leases),
            timeouts: AttemptTimeouts {
                connect: knobs.connect,
                header: knobs.header,
                body: knobs.body,
                write: knobs.write,
                deadline: knobs.total.map(|total| now + total),
            },
        })
    }

    fn render_request(&self, exchange: &Exchange, request: &Request<'_>, out: &mut Vec<u8>) {
        let facts = request.head().facts;
        let mut edits = Edits::new();
        if facts.connection != oxiroute_h1_head::Connection::Absent {
            edit(edits.remove(names::CONNECTION));
        }
        if facts.request_id_count != 1 {
            edit(edits.set(names::X_REQUEST_ID, Value::trusted(&exchange.id)));
        }
        write_request(out, request.bytes(), request.head(), &edits);
    }

    fn render_response(
        &self,
        exchange: &Exchange,
        response: &Response<'_>,
        now: Instant,
        out: &mut Vec<u8>,
    ) {
        let (head, facts) = (response.head, response.head.facts);
        let mut edits = Edits::new();
        if facts.transfer_encoding && head.find(Class::ContentLength).is_some() {
            edit(edits.remove(names::CONTENT_LENGTH));
        }
        for class in HOP_BY_HOP {
            if facts.present & class.bit() != 0 {
                edit(edits.remove(Name::known(class)));
            }
        }
        let chunked = response.framing == Framing::Chunked;
        if chunked {
            edit(edits.set(names::TRANSFER_ENCODING, Value::trusted(b"chunked")));
        }
        edit(edits.stamp_request_id(response.bytes, head, Value::trusted(&exchange.id)));
        let date = self.date(now);
        let connection: &[u8] = if response.keepalive {
            b"keep-alive"
        } else {
            b"close"
        };
        let automatic = Automatic {
            date: &date,
            connection,
        };
        let minor = if chunked { 1 } else { head.minor };
        write_response(out, response.bytes, head, &edits, minor, Some(automatic));
    }

    fn judge(&self, exchange: &mut Exchange, answered: &Answered<'_>) -> Recovery {
        let status = answered.head.status;
        let wanted = self.knobs.retry_status == Some(status) && exchange.tries < self.knobs.retries;
        if answered.retryable && wanted {
            exchange.tries += 1;
            self.counters.spent_retries.fetch_add(1, Ordering::SeqCst);
            return Recovery::Retry;
        }
        Recovery::Answer
    }

    fn recover(&self, exchange: &mut Exchange, failure: Failure, _now: Instant) -> Recovery {
        if failure == Failure::Cancelled {
            return if self.cancel.swap(0, Ordering::SeqCst) == MOVE {
                Recovery::Retry
            } else {
                Recovery::Answer
            };
        }
        if exchange.tries < self.knobs.retries {
            exchange.tries += 1;
            return Recovery::Retry;
        }
        Recovery::Answer
    }

    fn answer(
        &self,
        exchange: &mut Exchange,
        failure: Failure,
        _request_is_head: bool,
        out: &mut Vec<u8>,
    ) -> Option<u16> {
        let status = match failure {
            Failure::ConnectTimeout
            | Failure::HeaderTimeout
            | Failure::BodyTimeout
            | Failure::TotalTimeout => 504,
            Failure::NoServer => 503,
            _ => 502,
        };
        let id = std::str::from_utf8(&exchange.id).unwrap_or_default();
        let _ = write!(
            out,
            "HTTP/1.1 {status} Error\r\nContent-Length: 0\r\nConnection: close\r\nX-Request-Id: {id}\r\n\r\n"
        );
        Some(status)
    }

    fn replay_safe(&self, _exchange: &Exchange) -> bool {
        true
    }

    fn cancelled(&self, _exchange: &Exchange) -> bool {
        self.cancel.load(Ordering::SeqCst) != 0
    }

    fn finish(&self, _exchange: Exchange, outcome: &Outcome) {
        let counter = if outcome.failure.is_none() {
            &self.counters.completed
        } else {
            &self.counters.failed
        };
        counter.fetch_add(1, Ordering::Relaxed);
        if self.knobs.record {
            self.outcomes.lock().expect("outcomes").push(*outcome);
        }
    }

    fn handoff(&self, handoff: Handoff<Guard>) {
        let _ = self.handoffs.lock().expect("handoffs").send(handoff);
    }
}
