#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The plane a suite is told to run on is the one the real binary serves plain HTTP on: the binary announces the plane
//! of every listener at plan time, and refuses a plane this machine cannot run (a ring the kernel does not give, a
//! sandbox that blocks `io_uring`) instead of serving on another one.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use h1_wire::{Origin, Wire, status};
use oxiroute_config::HttpDataPlane;
use process_support::suite_data_plane;

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn plane_name(plane: HttpDataPlane) -> &'static str {
    match plane {
        HttpDataPlane::Epoll => "epoll",
        HttpDataPlane::IoUring => "io_uring",
        HttpDataPlane::Tokio => "tokio",
    }
}

#[tokio::test]
async fn the_binary_serves_plain_http_on_the_plane_the_suite_selects() {
    let wire = Wire::over(Origin::start(OK).await, |config| {
        config.log_filter = Some("oxiroute_server::service_plan=info".into());
    })
    .await;
    let response = wire
        .exchange(b"GET /r HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    let output = wire.server.shutdown_output();
    let announced = output
        .lines()
        .find_map(|line| {
            line.split_once("`wire` data plane: ")
                .map(|(_, plane)| plane)
        })
        .unwrap_or_else(|| panic!("the binary announced no data plane:\n{output}"));
    if let Some(plane) = suite_data_plane() {
        assert_eq!(
            announced.split(' ').next(),
            Some(plane_name(plane)),
            "the suite selects {plane:?}: {announced}"
        );
        assert!(!announced.contains("skipped"), "{announced}");
    }
}
