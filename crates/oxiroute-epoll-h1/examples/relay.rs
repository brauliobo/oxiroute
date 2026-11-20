//! A relay on the engine with the semantics of a real service: host and path routes, header edits, request ids,
//! counters per route and status, an access line per request and the timeouts of an attempt.
//!
//! relay --listen ADDR --origin ADDR [--threads N] [--level] [--requests N]

#[path = "../tests/support/date.rs"]
#[allow(clippy::pedantic)]
mod date;
#[path = "../tests/support/service.rs"]
#[allow(dead_code, clippy::pedantic)]
mod service;

use std::{
    cell::RefCell,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{
    Answered, Config, Engine, Failure, Handoff, Listener, Outcome, Recovery, Request, Response,
    Routed, Selected, Service, Timeouts, Trigger,
};
use service::{Guard, Knobs, Reference, Route};

const ROUTES: [(&str, &str); 8] = [
    ("bench.example.com", "/b"),
    ("bench.example.com", "/x"),
    ("a.example", "/"),
    ("b.example", "/"),
    ("c.example", "/api"),
    ("d.example", "/s"),
    ("e.example", "/"),
    ("bench.example.com", "/"),
];

const LOG_FLUSH: usize = 60_000;

thread_local! {
    static LOG: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(LOG_FLUSH + 512));
    static BY_STATUS: RefCell<[u64; 8]> = const { RefCell::new([0; 8]) };
}

struct Relay {
    inner: Reference,
}

fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0_u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[start..]);
}

fn account(outcome: &Outcome) {
    BY_STATUS.with_borrow_mut(|counts| counts[usize::from(outcome.status.unwrap_or(0) % 8)] += 1);
    LOG.with_borrow_mut(|log| {
        log.extend_from_slice(b"127.0.0.1 GET ");
        push_decimal(log, outcome.sent);
        log.push(b' ');
        push_decimal(
            log,
            outcome
                .ended
                .saturating_duration_since(outcome.started)
                .as_micros() as u64,
        );
        log.push(b'\n');
        if log.len() > LOG_FLUSH {
            log.clear();
        }
    });
}

impl Service for Relay {
    type Admission = <Reference as Service>::Admission;
    type Exchange = <Reference as Service>::Exchange;
    type Lease = <Reference as Service>::Lease;

    fn admit(&self, peer: SocketAddr) -> Option<Self::Admission> {
        self.inner.admit(peer)
    }

    fn route(
        &self,
        admission: &Self::Admission,
        request: &Request<'_>,
        now: Instant,
    ) -> Routed<Self::Exchange> {
        self.inner.route(admission, request, now)
    }

    fn select(&self, exchange: &mut Self::Exchange, now: Instant) -> Selected<Self::Lease> {
        self.inner.select(exchange, now)
    }

    fn render_request(&self, exchange: &Self::Exchange, request: &Request<'_>, out: &mut Vec<u8>) {
        self.inner.render_request(exchange, request, out);
    }

    fn render_response(
        &self,
        exchange: &Self::Exchange,
        response: &Response<'_>,
        now: Instant,
        out: &mut Vec<u8>,
    ) {
        self.inner.render_response(exchange, response, now, out);
    }

    fn judge(&self, exchange: &mut Self::Exchange, answered: &Answered<'_>) -> Recovery {
        self.inner.judge(exchange, answered)
    }

    fn recover(&self, exchange: &mut Self::Exchange, failure: Failure, now: Instant) -> Recovery {
        self.inner.recover(exchange, failure, now)
    }

    fn replay_safe(&self, exchange: &Self::Exchange) -> bool {
        self.inner.replay_safe(exchange)
    }

    fn answer(
        &self,
        exchange: &mut Self::Exchange,
        failure: Failure,
        head: bool,
        out: &mut Vec<u8>,
    ) -> Option<u16> {
        self.inner.answer(exchange, failure, head, out)
    }

    fn cancelled(&self, exchange: &Self::Exchange) -> bool {
        self.inner.cancelled(exchange)
    }

    fn finish(&self, exchange: Self::Exchange, outcome: &Outcome) {
        account(outcome);
        self.inner.finish(exchange, outcome);
    }

    fn handoff(&self, handoff: Handoff<Guard>) {
        self.inner.handoff(handoff);
    }
}

struct Arguments {
    listen: SocketAddr,
    origin: SocketAddr,
    threads: usize,
    trigger: Trigger,
    requests: Option<usize>,
}

fn arguments() -> Arguments {
    let mut args = std::env::args().skip(1);
    let mut parsed = Arguments {
        listen: "127.0.0.1:0".parse().expect("address"),
        origin: "127.0.0.1:0".parse().expect("address"),
        threads: 1,
        trigger: Trigger::Edge,
        requests: None,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().expect("a value");
        match flag.as_str() {
            "--listen" => parsed.listen = value().parse().expect("listen address"),
            "--origin" => parsed.origin = value().parse().expect("origin address"),
            "--threads" => parsed.threads = value().parse().expect("threads"),
            "--edge" => parsed.trigger = Trigger::Edge,
            "--level" => parsed.trigger = Trigger::Level,
            "--requests" => parsed.requests = Some(value().parse().expect("requests")),
            other => panic!("unknown flag {other}"),
        }
    }
    parsed
}

fn main() {
    let args = arguments();
    let routes = ROUTES
        .iter()
        .map(|(host, prefix)| Route {
            host,
            prefix,
            origin: args.origin,
        })
        .collect();
    let (handoffs, _kept) = mpsc::channel();
    let (paused, _paused_kept) = mpsc::channel();
    let knobs = Knobs {
        header: Duration::from_secs(30),
        body: Duration::from_secs(30),
        ..Knobs::default()
    };
    let service = Arc::new(Relay {
        inner: Reference::new(routes, knobs, (handoffs, paused)),
    });
    let listener = Listener::reuseport(
        args.listen,
        args.threads,
        Arc::clone(&service),
        Timeouts::default(),
    )
    .expect("listeners");
    let config = Config {
        threads: args.threads,
        trigger: args.trigger,
        ..Config::default()
    };
    let engine = Engine::start(&config, vec![listener]).expect("engine");
    let Some(limit) = args.requests else {
        std::thread::park();
        return;
    };
    while service.inner.counters.completed.load(Ordering::Relaxed) < limit {
        std::thread::sleep(Duration::from_millis(50));
    }
    engine.drain();
    engine.join().expect("engine stops");
}
