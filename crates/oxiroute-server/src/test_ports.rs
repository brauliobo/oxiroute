//! Addresses for tests that must name a port before the code under test binds it.
//!
//! A port given back after `bind("127.0.0.1:0")` returns to the kernel's ephemeral pool, where a parallel
//! test (another `bind(0)` or an outgoing connection) can take it before the real bind. The kernel never
//! hands out ports below the ephemeral range, so only this allocator uses them. Test processes run
//! side by side, so each port is also claimed with an exclusive lock on a file named after it, shared
//! with the integration test allocator, and the lock is held until the process exits.

use std::{
    fs::{self, File},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
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

fn claim_directory() -> PathBuf {
    std::env::temp_dir().join("oxiroute-test-ports")
}

fn claim(directory: &Path, port: u16) -> Option<File> {
    fs::create_dir_all(directory).expect("test port claim directory");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(port.to_string()))
        .expect("test port claim file");
    file.try_lock().ok().map(|()| file)
}

/// A loopback TCP address that nothing listens on and the kernel will not assign to anyone else,
/// never returned twice to any test process running at the same time.
pub(crate) fn unused_tcp_address() -> SocketAddr {
    let start = ephemeral_start() - BAND_PORTS;
    for _ in 0..BAND_PORTS {
        // Test processes started together begin at different offsets of the band.
        let offset = (NEXT.fetch_add(1, Ordering::Relaxed) + std::process::id() * 97) % BAND_PORTS;
        let port = u16::try_from(start + offset).expect("port");
        let Some(claimed) = claim(&claim_directory(), port) else {
            continue;
        };
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        if TcpListener::bind(address).is_ok() {
            CLAIMED.lock().expect("claimed ports").push(claimed);
            return address;
        }
    }
    panic!("every port of the test band is claimed or in use")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn claims_in_one_process_never_return_the_same_port() {
        let addresses = (0..2_000).map(|_| unused_tcp_address()).collect::<Vec<_>>();
        let distinct = addresses.iter().collect::<HashSet<_>>();
        assert_eq!(distinct.len(), addresses.len());
    }

    #[test]
    fn a_claim_held_by_another_open_file_handle_is_skipped() {
        let directory = TempDir::new().expect("claim directory");
        let held = claim(directory.path(), 1).expect("first claim");
        assert!(claim(directory.path(), 1).is_none());
        assert!(claim(directory.path(), 2).is_some());
        drop(held);
        assert!(claim(directory.path(), 1).is_some());
    }

    #[test]
    fn an_allocated_port_stays_claimed_after_it_is_returned() {
        let port = unused_tcp_address().port();
        assert!(claim(&claim_directory(), port).is_none());
    }
}
