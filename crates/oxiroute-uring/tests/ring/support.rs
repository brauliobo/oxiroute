#![allow(dead_code)]

use std::{
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use oxiroute_uring::{Config, Event, Handle, Ring};

pub const LISTENER: u64 = 1;

pub fn ring(config: Config) -> Ring {
    Ring::new(&config).expect("ring")
}

pub fn small() -> Config {
    Config {
        entries: 256,
        flush: Duration::from_secs(2),
        ..Config::default()
    }
}

pub struct Harness {
    pub ring: Ring,
    pub events: Vec<Event>,
    pub baseline: usize,
}

impl Harness {
    pub fn new(config: Config) -> Self {
        Self {
            ring: ring(config),
            events: Vec::new(),
            baseline: 0,
        }
    }

    pub fn pump(&mut self, millis: u64) {
        self.ring
            .wait(Some(Duration::from_millis(millis)), &mut self.events)
            .expect("wait");
    }

    pub fn until(&mut self, what: &str, mut found: impl FnMut(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(at) = self.events.iter().position(&mut found) {
                return self.events.remove(at);
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.events
            );
            self.pump(50);
        }
    }

    pub fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.ring.in_flight() > self.baseline {
            assert!(
                Instant::now() < deadline,
                "operations stayed in flight: {}",
                self.ring.in_flight()
            );
            self.pump(20);
        }
    }

    pub fn listener(&mut self) -> (Handle, SocketAddr) {
        let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = socket.local_addr().expect("address");
        let handle = self.ring.listen(OwnedFd::from(socket), LISTENER);
        self.ring.set_accepting(handle, true);
        self.baseline = self.ring.in_flight();
        (handle, address)
    }

    pub fn accept(&mut self, tag: u64) -> (Handle, SocketAddr) {
        self.accept_with(tag, None)
    }

    pub fn accept_with(&mut self, tag: u64, send_buffer: Option<usize>) -> (Handle, SocketAddr) {
        let Event::Accepted { fd, peer, .. } = self.until("an accepted connection", |event| {
            matches!(event, Event::Accepted { tag: LISTENER, .. })
        }) else {
            unreachable!()
        };
        if let Some(size) = send_buffer {
            rustix::net::sockopt::set_socket_send_buffer_size(&fd, size).expect("send buffer");
        }
        let handle = self.ring.adopt(fd, tag);
        self.ring.start(handle);
        (handle, peer.expect("peer address"))
    }

    pub fn readable(&mut self, tag: u64) {
        self.until(
            "readable",
            |event| matches!(event, Event::Readable { tag: t } if *t == tag),
        );
    }

    pub fn read_exact(&mut self, handle: Handle, tag: u64, count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut chunk = vec![0_u8; 3000];
        while bytes.len() < count {
            match self.ring.read(handle, &mut chunk) {
                oxiroute_uring::Read::Data(n) => bytes.extend_from_slice(&chunk[..n]),
                oxiroute_uring::Read::Block => {
                    self.until(
                        "more data",
                        |event| matches!(event, Event::Readable { tag: t } if *t == tag),
                    );
                }
                other => panic!("read ended early: {other:?}"),
            }
        }
        bytes
    }
}

pub fn shrink_send_buffer(stream: &TcpStream) {
    rustix::net::sockopt::set_socket_recv_buffer_size(stream, 65536).expect("receive buffer");
}

pub fn client(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(address).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream
}

pub fn read_all(mut stream: &TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read to end");
    bytes
}

pub fn write_all(mut stream: &TcpStream, bytes: &[u8]) {
    stream.write_all(bytes).expect("write");
}
