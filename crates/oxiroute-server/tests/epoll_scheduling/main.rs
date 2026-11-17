#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Listeners with `aged_per_worker` scheduling and `dynamic` worker scaling on the epoll data plane: the real binary in
//! front of a raw TCP origin serves keep-alive connections, packs them on the active threads when the load is light and
//! reports through the same metrics as the tokio plane.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, ConnectionScheduling, HttpDataPlane, Stats, WorkerScaling};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use h1_wire::{Origin, Wire};
use http_support::http_request;
use process_support::{reserve_tcp_address, serves_on_epoll, suite_data_plane};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const RUNTIME: &str = "OxiRoute wire";
const WORKERS: u64 = 4;
const CONNECTIONS: usize = 24;

fn scaled(statistics: SocketAddr, plane: Option<HttpDataPlane>) -> impl FnOnce(&mut ConfigDraft) {
    move |config| {
        config.http_data_plane = plane;
        config.listener_workers = Some(WORKERS);
        config.connection_scheduling = Some(ConnectionScheduling::AgedPerWorker);
        config.worker_scaling = Some(WorkerScaling::Dynamic);
        config.scale_down_delay_ms = Some(200);
        config.stats = Some(Stats {
            binds: vec![statistics],
            admin_token_file: None,
            pages: Vec::new(),
        });
    }
}

async fn ask(stream: &mut TcpStream) {
    stream.write_all(GET).await.expect("request");
    let mut response = Vec::new();
    while !response.ends_with(b"ok") {
        let mut chunk = [0_u8; 512];
        let read = stream.read(&mut chunk).await.expect("response");
        assert_ne!(read, 0, "closed early");
        response.extend_from_slice(&chunk[..read]);
    }
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
}

fn engine_threads(pid: u32) -> u64 {
    let prefix = match suite_data_plane() {
        Some(HttpDataPlane::IoUring) => "uring-h1-",
        _ => "epoll-h1-",
    };
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).expect("task list");
    tasks
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
        .filter(|name| name.starts_with(prefix))
        .count() as u64
}

async fn metrics(statistics: SocketAddr) -> String {
    let response = http_request(statistics, "GET", "/metrics", &[], &[]).await;
    String::from_utf8(response.body).expect("metrics UTF-8")
}

fn value(metrics: &str, series: &str) -> Option<u64> {
    metrics
        .lines()
        .find_map(|line| line.strip_prefix(series)?.trim().parse().ok())
}

fn scheduling_series(metrics: &str) -> BTreeSet<String> {
    metrics
        .lines()
        .filter(|line| {
            [
                "oxiroute_worker_scaling",
                "oxiroute_aged_queue",
                "oxiroute_connection_scheduling",
            ]
            .iter()
            .any(|family| line.starts_with(family))
        })
        .map(|line| {
            line.rsplit_once(' ')
                .map_or(line, |(series, _)| series)
                .to_owned()
        })
        .collect()
}

async fn serve_lightly(wire: &Wire, statistics: SocketAddr) -> (usize, String) {
    let mut streams = Vec::new();
    for _ in 0..CONNECTIONS {
        streams.push(TcpStream::connect(wire.proxy).await.expect("connect"));
    }
    let mut requests = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        for stream in &mut streams {
            ask(stream).await;
            requests += 1;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let report = metrics(statistics).await;
        let series = |name: &str| format!("{name}{{runtime=\"{RUNTIME}\"}} ");
        let active = value(&report, &series("oxiroute_worker_scaling_active_workers"));
        let moved = value(
            &report,
            &series("oxiroute_worker_scaling_relocations_total"),
        );
        if active == Some(1) && moved > Some(0) {
            return (requests, report);
        }
        assert!(Instant::now() < deadline, "never packed: {report}");
    }
}

#[tokio::test]
async fn an_aged_listener_with_dynamic_scaling_packs_its_connections_and_reports_the_scaling() {
    let statistics = reserve_tcp_address();
    let wire = Wire::over(Origin::start(OK).await, scaled(statistics, None)).await;

    let (requests, report) = serve_lightly(&wire, statistics).await;

    let series = |name: &str| format!("{name}{{runtime=\"{RUNTIME}\"}} ");
    assert_eq!(
        value(&report, &series("oxiroute_worker_scaling_workers")),
        Some(WORKERS)
    );
    if serves_on_epoll() {
        let moved = value(
            &report,
            &series("oxiroute_worker_scaling_relocations_total"),
        );
        assert!(moved > Some(0), "{report}");
        assert!(
            value(&report, "oxiroute_aged_queue_polls_total ") > Some(0),
            "{report}"
        );
    }
    assert!(report.contains("oxiroute_worker_scaling_info{mode=\"dynamic\"} 1"));
    assert!(report.contains(
        "oxiroute_connection_scheduling_info{mode=\"aged_per_worker\",age_ms=\"adaptive\"} 1"
    ));
    assert_eq!(
        engine_threads(wire.server.pid()),
        if serves_on_epoll() { WORKERS } else { 0 }
    );
    assert!(requests > 0);
    wire.finish();
}

#[tokio::test]
async fn the_epoll_plane_reports_the_series_of_the_tokio_plane() {
    let planes = [None, Some(HttpDataPlane::Tokio)];
    let mut series = Vec::new();
    for plane in planes {
        let statistics = reserve_tcp_address();
        let wire = Wire::over(Origin::start(OK).await, scaled(statistics, plane)).await;
        let (_, report) = serve_lightly(&wire, statistics).await;
        series.push(scheduling_series(&report));
        wire.finish();
    }

    assert_eq!(series[0], series[1]);
}
