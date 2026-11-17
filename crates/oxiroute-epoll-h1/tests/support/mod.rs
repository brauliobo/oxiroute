#![allow(dead_code, unused_imports, clippy::pedantic)]

mod date;
mod origin;
mod service;
mod wire;

use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use oxiroute_epoll_h1::{
    Age, Config, Engine, Handoff, Listener, Policy, Scale, Scheduling, Timeouts,
};

pub use origin::{Origin, await_head, reset, sized};
pub use service::{Guard, KILL, Knobs, MOVE, Reference, Route};
pub use wire::Wire;

struct Start {
    accepting: bool,
    shared: bool,
}

pub struct Fixture {
    pub addr: SocketAddr,
    pub origin: Origin,
    pub service: Arc<Reference>,
    pub handoffs: Receiver<Handoff<Guard>>,
    pub paused: Receiver<()>,
    engine: Option<Engine>,
}

pub fn test_config() -> Config {
    Config {
        max_idle_upstreams: 512,
        max_idle_per_target: 256,
        tick: Duration::from_millis(5),
        drain: Duration::from_secs(5),
        linger: Duration::from_millis(500),
        trigger: if std::env::var("EPOLL_H1_TRIGGER").is_ok_and(|value| value == "level") {
            oxiroute_epoll_h1::Trigger::Level
        } else {
            oxiroute_epoll_h1::Trigger::Edge
        },
        scheduling: suite_scheduling(),
        driver: if std::env::var("EPOLL_H1_DRIVER").is_ok_and(|value| value == "io_uring") {
            oxiroute_epoll_h1::Driver::IoUring
        } else {
            oxiroute_epoll_h1::Driver::Epoll
        },
        ..Config::default()
    }
}

fn suite_scheduling() -> Option<Scheduling> {
    let policy = Policy {
        min_active: 1,
        target_busy_permille: 400,
        release_delay: Duration::from_millis(100),
    };
    let scale = Some(Scale {
        name: "engine-suite".into(),
        policy,
    });
    match std::env::var("EPOLL_H1_SCHEDULING").as_deref() {
        Ok("aged") => Some(Scheduling {
            age: Age::adaptive(),
            scale: None,
        }),
        Ok("scaled") => Some(Scheduling {
            age: Age::Fixed(Duration::from_millis(5)),
            scale,
        }),
        _ => None,
    }
}

impl Fixture {
    pub fn start() -> Self {
        Self::with(test_config(), Timeouts::default(), Knobs::default())
    }

    pub fn with(config: Config, timeouts: Timeouts, knobs: Knobs) -> Self {
        let origin = Origin::start();
        let routes = vec![Route {
            host: "test.example",
            prefix: "/",
            origin: origin.addr,
        }];
        Self::with_routes(config, timeouts, knobs, origin, routes)
    }

    pub fn paused() -> Self {
        let origin = Origin::start();
        let routes = vec![Route {
            host: "test.example",
            prefix: "/",
            origin: origin.addr,
        }];
        Self::build(
            (test_config(), Timeouts::default(), Knobs::default()),
            origin,
            routes,
            false,
        )
    }

    pub fn with_routes(
        config: Config,
        timeouts: Timeouts,
        knobs: Knobs,
        origin: Origin,
        routes: Vec<Route>,
    ) -> Self {
        Self::build((config, timeouts, knobs), origin, routes, true)
    }

    pub fn on_shared_listener(config: Config, knobs: Knobs) -> Self {
        let origin = Origin::start();
        let routes = vec![Route {
            host: "test.example",
            prefix: "/",
            origin: origin.addr,
        }];
        Self::build_with(
            (config, Timeouts::default(), knobs),
            origin,
            routes,
            Start {
                accepting: true,
                shared: true,
            },
        )
    }

    fn build(
        parts: (Config, Timeouts, Knobs),
        origin: Origin,
        routes: Vec<Route>,
        accepting: bool,
    ) -> Self {
        let start = Start {
            accepting,
            shared: false,
        };
        Self::build_with(parts, origin, routes, start)
    }

    fn build_with(
        (config, timeouts, knobs): (Config, Timeouts, Knobs),
        origin: Origin,
        routes: Vec<Route>,
        Start { accepting, shared }: Start,
    ) -> Self {
        let (sender, handoffs) = mpsc::channel();
        let (paused_sender, paused) = mpsc::channel();
        let service = Arc::new(Reference::new(routes, knobs, (sender, paused_sender)));
        let listener = if shared {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            socket.set_nonblocking(true).expect("nonblocking");
            Listener::shared(
                &std::os::fd::OwnedFd::from(socket),
                config.threads,
                Arc::clone(&service),
                timeouts,
            )
            .expect("listeners")
        } else {
            Listener::reuseport(
                "127.0.0.1:0".parse().expect("address"),
                config.threads,
                Arc::clone(&service),
                timeouts,
            )
            .expect("listeners")
        };
        let addr = listener.local_addr().expect("listener address");
        let engine = if accepting {
            Engine::start(&config, vec![listener])
        } else {
            Engine::start_paused(&config, vec![listener])
        }
        .expect("engine starts");
        Self {
            addr,
            origin,
            service,
            handoffs,
            paused,
            engine: Some(engine),
        }
    }

    pub fn wire(&self) -> Wire {
        Wire::connect(self.addr)
    }

    pub fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("engine runs")
    }

    pub fn take_engine(&mut self) -> Engine {
        self.engine.take().expect("engine runs")
    }

    pub fn stop_after_failure(&mut self) -> io::Result<()> {
        let engine = self.take_engine();
        engine.stop();
        engine.join()
    }

    pub fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.stop();
            engine.join().expect("engine stops cleanly");
        }
    }

    pub fn drain_and_join(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.drain();
            engine.join().expect("engine drains cleanly");
        }
    }

    pub fn outcomes(&self) -> Vec<oxiroute_epoll_h1::Outcome> {
        self.service.outcomes.lock().expect("outcomes").clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub struct Blackhole {
    pub addr: SocketAddr,
    _listener: std::os::fd::OwnedFd,
    _queued: Vec<std::net::TcpStream>,
}

pub fn blackhole() -> Blackhole {
    use rustix::net::{
        AddressFamily, SocketFlags, SocketType, bind, getsockname, listen, socket_with,
    };
    let listener = socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("socket");
    bind(&listener, &std::net::SocketAddr::from(([127, 0, 0, 1], 0))).expect("bind");
    listen(&listener, 0).expect("listen");
    let addr = SocketAddr::try_from(getsockname(&listener).expect("name")).expect("address");
    let mut queued = Vec::new();
    while let Ok(stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
        queued.push(stream);
    }
    Blackhole {
        addr,
        _listener: listener,
        _queued: queued,
    }
}

pub fn closed_port() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("address")
}
