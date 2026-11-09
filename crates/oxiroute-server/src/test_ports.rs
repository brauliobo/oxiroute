//! Addresses for tests that must name a port before the code under test binds it.
//!
//! A port given back after `bind("127.0.0.1:0")` returns to the kernel's ephemeral pool, where a parallel
//! test (another `bind(0)` or an outgoing connection) can take it before the real bind. The kernel never
//! hands out ports below the ephemeral range, so only this allocator uses them.

use std::{
    net::{SocketAddr, TcpListener},
    sync::atomic::{AtomicU32, Ordering},
};

const BAND_PORTS: u32 = 8_000;
const DEFAULT_EPHEMERAL_START: u32 = 32_768;

static NEXT: AtomicU32 = AtomicU32::new(0);

fn ephemeral_start() -> u32 {
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse().ok())
        .unwrap_or(DEFAULT_EPHEMERAL_START)
}

/// A loopback TCP address that nothing listens on and the kernel will not assign to anyone else.
pub(crate) fn unused_tcp_address() -> SocketAddr {
    let start = ephemeral_start() - BAND_PORTS;
    loop {
        // Test processes started together begin at different offsets of the band.
        let offset = (NEXT.fetch_add(1, Ordering::Relaxed) + std::process::id() * 97) % BAND_PORTS;
        let address =
            SocketAddr::from(([127, 0, 0, 1], u16::try_from(start + offset).expect("port")));
        if TcpListener::bind(address).is_ok() {
            return address;
        }
    }
}
