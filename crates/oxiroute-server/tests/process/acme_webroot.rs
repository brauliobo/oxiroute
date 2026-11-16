#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! An operator route under `/.well-known/acme-challenge/`, such as the webroot an imported nginx `location ^~` serves
//! for another ACME client, is reached for every token the built-in HTTP-01 store does not hold. The proxy is the real
//! binary, whose store is empty here, on the HTTP/1.1 lane and on the classic path.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
#[path = "../support/process.rs"]
mod process_support;

use std::{fs, os::unix::fs::PermissionsExt as _};

use oxiroute_config::{ConfigDraft, HttpPathSelector};
use serde_json::json;

use h1_wire::{Origin, Wire};

const TOKEN: &str = "/.well-known/acme-challenge/webroot-token_1";

/// The route `oxiroute import nginx` lowers `location ^~ /.well-known/acme-challenge/ { root ...; try_files $uri =404; }`
/// to, beside a raw-prefix catch-all proxy route, which the longer raw prefix outranks.
fn webroot(root: &std::path::Path, fast_lane: bool) -> impl FnOnce(&mut ConfigDraft) {
    let route = json!({
        "path": {"kind": "raw_prefix", "value": "/.well-known/acme-challenge/"},
        "action": {
            "type": "static_files",
            "root_directory": root,
            "try_files": [{"type": "request_path"}, {"type": "status", "status": 404}]
        }
    });
    move |config| {
        let route = serde_json::from_value(route).expect("webroot route");
        config.http_services[0].routes[0].path = HttpPathSelector::RawPrefix { value: "/".into() };
        config.http_services[0].routes.insert(0, route);
        config.http_fast_lane = (!fast_lane).then_some(false);
    }
}

fn webroot_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir_in("/dev/shm").expect("webroot in memory");
    let challenges = directory.path().join("public/.well-known/acme-challenge");
    fs::create_dir_all(&challenges).expect("challenge directory");
    for path in [
        directory.path().join("public"),
        directory.path().join("public/.well-known"),
        challenges.clone(),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("secure directory");
    }
    fs::write(
        challenges.join("webroot-token_1"),
        "webroot-token_1.thumbprint\n",
    )
    .expect("token file");
    directory
}

async fn request(wire: &Wire, method: &str, path: &str) -> String {
    wire.exchange(
        format!("{method} {path} HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
}

fn status(response: &str) -> &str {
    response.lines().next().expect("status line")
}

async fn serving(fast_lane: bool) -> (Wire, tempfile::TempDir) {
    let directory = webroot_directory();
    let origin = Origin::start(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\norigin").await;
    let wire = Wire::over(origin, webroot(&directory.path().join("public"), fast_lane)).await;
    (wire, directory)
}

async fn webroot_files_are_served(fast_lane: bool) {
    let (wire, _directory) = serving(fast_lane).await;
    let get = request(&wire, "GET", TOKEN).await;
    assert!(status(&get).starts_with("HTTP/1.1 200"), "{get}");
    assert!(get.ends_with("webroot-token_1.thumbprint\n"), "{get}");
    let head = request(&wire, "HEAD", TOKEN).await;
    assert!(status(&head).starts_with("HTTP/1.1 200"), "{head}");
    let missing = request(&wire, "GET", "/.well-known/acme-challenge/absent").await;
    assert!(status(&missing).starts_with("HTTP/1.1 404"), "{missing}");
    assert!(
        !missing.contains("origin"),
        "the webroot route answers, not the catch-all proxy: {missing}"
    );
    let post = request(&wire, "POST", TOKEN).await;
    assert!(
        !post.ends_with("origin"),
        "a method the store does not answer reaches the route: {post}"
    );
    assert!(
        wire.origin.requests().is_empty(),
        "no request reached the origin"
    );
    wire.finish();
}

#[tokio::test]
async fn webroot_files_under_the_acme_prefix_are_served_on_the_lane() {
    webroot_files_are_served(true).await;
}

#[tokio::test]
async fn webroot_files_under_the_acme_prefix_are_served_on_the_classic_path() {
    webroot_files_are_served(false).await;
}

async fn unrouted_tokens_are_the_default_not_found(fast_lane: bool) {
    let origin = Origin::start(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\norigin").await;
    let wire = Wire::over(origin, move |config| {
        let route = &mut config.http_services[0].routes[0];
        route.path = oxiroute_config::HttpPathSelector::Exact {
            value: "/only".into(),
        };
        config.http_fast_lane = (!fast_lane).then_some(false);
    })
    .await;
    for method in ["GET", "HEAD", "POST"] {
        let response = request(&wire, method, TOKEN).await;
        assert!(
            status(&response).starts_with("HTTP/1.1 404"),
            "{method}: {response}"
        );
    }
    assert!(
        wire.origin.requests().is_empty(),
        "no request reached the origin"
    );
    wire.finish();
}

#[tokio::test]
async fn an_unrouted_token_is_the_default_not_found_on_the_lane() {
    unrouted_tokens_are_the_default_not_found(true).await;
}

#[tokio::test]
async fn an_unrouted_token_is_the_default_not_found_on_the_classic_path() {
    unrouted_tokens_are_the_default_not_found(false).await;
}
