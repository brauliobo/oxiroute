#![allow(clippy::pedantic)]

use std::{
    fs,
    io::Read as _,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::OwnedFd,
    os::unix::fs::FileExt as _,
    time::{Duration, Instant},
};

use oxiroute_uring::{Config, Event, Ring};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

const LISTENER: u64 = 1;
const SLACK: u64 = 6;

fn open_descriptors() -> u64 {
    fs::read_dir("/proc/self/fd").expect("descriptors").count() as u64
}

fn cpu_ticks(stat: &fs::File) -> u64 {
    let mut text = [0_u8; 512];
    let read = stat.read_at(&mut text, 0).expect("statistics");
    let text = String::from_utf8_lossy(&text[..read]).into_owned();
    let fields: Vec<&str> = text
        .rsplit_once(')')
        .expect("comm")
        .1
        .split_whitespace()
        .collect();
    fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime")
}

struct Listening {
    ring: Ring,
    events: Vec<Event>,
    address: SocketAddr,
}

impl Listening {
    fn new() -> Self {
        let mut ring = Ring::new(&Config {
            entries: 256,
            ..Config::default()
        })
        .expect("ring");
        let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = socket.local_addr().expect("address");
        let handle = ring
            .listen(OwnedFd::from(socket), LISTENER)
            .expect("listen");
        ring.set_accepting(handle, true);
        Self {
            ring,
            events: Vec::new(),
            address,
        }
    }

    fn pump(&mut self, limit: Duration) {
        self.ring.wait(Some(limit), &mut self.events).expect("wait");
    }

    fn accepted(&mut self) -> Vec<OwnedFd> {
        let mut taken = Vec::new();
        for event in std::mem::take(&mut self.events) {
            match event {
                Event::Accepted { fd, .. } => taken.push(fd),
                other => self.events.push(other),
            }
        }
        taken
    }
}

fn limit_descriptors(limit: u64) {
    let maximum = getrlimit(Resource::Nofile).maximum;
    let new = Rlimit {
        current: Some(limit),
        maximum,
    };
    setrlimit(Resource::Nofile, new).expect("limit the descriptors");
}

fn ended(stream: &mut TcpStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .expect("timeout");
    let mut byte = [0_u8; 1];
    !matches!(stream.read(&mut byte), Ok(1..))
}

#[test]
fn descriptors_exhausted_at_accept_neither_spin_the_ring_nor_stop_the_listener() {
    let mut listening = Listening::new();
    let stat = fs::File::open("/proc/self/stat").expect("statistics");
    let mut waiting: Vec<TcpStream> = (0..24)
        .map(|_| TcpStream::connect(listening.address).expect("connect"))
        .collect();
    limit_descriptors(open_descriptors() + SLACK);
    let mut held = Vec::new();
    for _ in 0..20 {
        listening.pump(Duration::from_millis(50));
        held.extend(listening.accepted());
    }
    assert!(!held.is_empty(), "nothing was accepted before the limit");
    listening.pump(Duration::from_millis(500));
    held.extend(listening.accepted());
    let shed = waiting
        .iter_mut()
        .filter_map(|stream| ended(stream).then_some(()))
        .count();
    assert!(
        shed >= waiting.len() - held.len() - 1,
        "{shed} of {} waiting connections were shed while {} are held",
        waiting.len(),
        held.len()
    );
    let before = cpu_ticks(&stat);
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        listening.pump(Duration::from_millis(100));
    }
    let spent = cpu_ticks(&stat) - before;
    assert!(
        spent < 30,
        "the ring spent {spent} ticks of 150 while it could not accept"
    );
    drop(waiting);
    drop(held);
    let mut next = TcpStream::connect(listening.address).expect("connect");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut served = Vec::new();
    while served.is_empty() && Instant::now() < deadline {
        listening.pump(Duration::from_millis(50));
        served.extend(listening.accepted());
    }
    assert!(!served.is_empty(), "the listener stayed stopped");
    assert!(!ended_quickly(&mut next), "the connection was shed");
}

fn ended_quickly(stream: &mut TcpStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("timeout");
    let mut byte = [0_u8; 1];
    matches!(stream.read(&mut byte), Ok(0))
}
