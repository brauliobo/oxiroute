#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../../support/config.rs"]
mod config_support;
#[path = "../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../support/http.rs"]
mod http_support;
#[path = "../../support/process.rs"]
mod process_support;
#[path = "../../support/revision.rs"]
mod revision_support;
#[path = "../../support/mod.rs"]
mod wire_support;

mod health;
mod tickets;

use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, Management, ValidatedConfig};
use serde_json::Value;
use tokio::time::sleep;

use http_support::http_request;
use process_support::{ServerProcess, write_config};
use revision_support::{active_revision, wait_for_new_revision};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const WAIT: Duration = Duration::from_secs(15);

pub(crate) fn authorization() -> String {
    format!("Bearer {TOKEN}")
}

pub(crate) fn management(address: SocketAddr) -> Management {
    Management {
        bind: address,
        ui_dir: None,
        token_file: None,
    }
}

pub(crate) fn start(config: &ValidatedConfig, supervised: bool) -> ServerProcess {
    if supervised {
        ServerProcess::start_supervised(config, TOKEN)
    } else {
        ServerProcess::start(config, Some(TOKEN))
    }
}

pub(crate) async fn reload(
    server: &ServerProcess,
    management: SocketAddr,
    config: &mut ConfigDraft,
    sequence: u64,
) {
    let original = active_revision(management, &authorization()).await;
    config.max_connections = Some(1_000 + sequence);
    write_config(&server.config_path, &config.clone().validate().unwrap());
    wait_for_new_revision(management, &authorization(), &original).await;
}

pub(crate) struct Origin {
    pub(crate) address: SocketAddr,
    pub(crate) requests: Arc<AtomicUsize>,
    pub(crate) probes: Arc<AtomicUsize>,
}

pub(crate) fn spawn_origin() -> Origin {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("origin");
    let address = listener.local_addr().expect("origin address");
    let requests = Arc::new(AtomicUsize::new(0));
    let probes = Arc::new(AtomicUsize::new(0));
    let counters = (Arc::clone(&requests), Arc::clone(&probes));
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let counters = counters.clone();
            thread::spawn(move || serve(stream, &counters.0, &counters.1));
        }
    });
    Origin {
        address,
        requests,
        probes,
    }
}

fn serve(mut stream: TcpStream, requests: &AtomicUsize, probes: &AtomicUsize) {
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) => received.push(byte[0]),
            _ => return,
        }
        if !received.ends_with(b"\r\n\r\n") {
            continue;
        }
        let counter = if received.starts_with(b"GET /health ") {
            probes
        } else {
            requests
        };
        counter.fetch_add(1, Ordering::SeqCst);
        received.clear();
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\n\r\nok";
        if stream.write_all(response).is_err() {
            return;
        }
    }
}

pub(crate) async fn wait_until<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25)).await;
    }
}

pub(crate) async fn server_state(management: SocketAddr, pool: &str, server: &str) -> Value {
    let authorization = authorization();
    let response = http_request(
        management,
        "GET",
        "/api/v1/servers",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.text());
    let listing = response.json();
    let entry = listing["servers"]
        .as_array()
        .expect("servers")
        .iter()
        .find(|entry| entry["pool"] == pool && entry["server"]["name"] == server)
        .unwrap_or_else(|| panic!("no server {server} in pool {pool}"));
    entry["server"].clone()
}
