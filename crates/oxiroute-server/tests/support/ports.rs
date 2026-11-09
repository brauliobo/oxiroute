#![allow(dead_code, clippy::missing_panics_doc, clippy::must_use_candidate)]
//! Loopback addresses for tests that must name a port before the code under test binds it.
//!
//! A port given back after `bind("127.0.0.1:0")` returns to the kernel's ephemeral pool, where a
//! parallel test (another `bind(0)` or an outgoing connection) can take it before the real bind. The
//! kernel never hands out ports below the ephemeral range, so only this allocator uses them. Test
//! binaries run as separate processes, so each port is also claimed with an exclusive lock on a file
//! named after it that stays held until the process exits.

use std::{
    fs::{self, File},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

const BAND_PORTS: u32 = 8_000;
const DEFAULT_EPHEMERAL_START: u32 = 32_768;

static NEXT: AtomicU32 = AtomicU32::new(0);
static CLAIMED: Mutex<Vec<File>> = Mutex::new(Vec::new());

fn ephemeral_start() -> u32 {
    fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse().ok())
        .unwrap_or(DEFAULT_EPHEMERAL_START)
}

fn claim(port: u16) -> bool {
    let directory: PathBuf = std::env::temp_dir().join("oxiroute-test-ports");
    fs::create_dir_all(&directory).expect("test port claim directory");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(port.to_string()))
        .expect("test port claim file");
    let claimed = file.try_lock().is_ok();
    if claimed {
        CLAIMED.lock().unwrap().push(file);
    }
    claimed
}

/// A loopback address whose TCP and UDP ports nothing uses and the kernel will not assign to anyone
/// else, never returned twice to any test process running at the same time.
pub fn unused_address() -> SocketAddr {
    let start = ephemeral_start() - BAND_PORTS;
    loop {
        // Test processes started together begin at different offsets of the band.
        let offset = (NEXT.fetch_add(1, Ordering::Relaxed) + std::process::id() * 97) % BAND_PORTS;
        let port = u16::try_from(start + offset).expect("port");
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        if claim(port) && TcpListener::bind(address).is_ok() && UdpSocket::bind(address).is_ok() {
            return address;
        }
    }
}

pub fn unused_addresses(count: usize) -> Vec<SocketAddr> {
    (0..count).map(|_| unused_address()).collect()
}
