//! `oxiroute route test` against a real daemon, checked against the answers its listener gives.

use oxiroute_config::{
    ConfigDraft, HttpHostSelector, HttpPathSelector, HttpRedirectLocation, HttpRoute,
    HttpRouteAction,
};
use serde_json::Value;

use super::{Daemon, http_support::raw_http_request, reserve_tcp_address, stderr, stdout};

const API_HOST: &str = "api.example.test";

fn routes(draft: &mut ConfigDraft) {
    let proxy = draft.http_services[0].routes[0].clone();
    let route = |path: HttpPathSelector, action: HttpRouteAction| HttpRoute {
        path,
        action,
        host: None,
        ..proxy.clone()
    };
    draft.http_services[0].routes = vec![
        HttpRoute {
            host: Some(HttpHostSelector::NormalizedHost {
                value: API_HOST.into(),
            }),
            path: HttpPathSelector::SegmentPrefix {
                value: "/v1".into(),
            },
            ..proxy.clone()
        },
        route(
            HttpPathSelector::Exact {
                value: "/health".into(),
            },
            HttpRouteAction::FixedResponse {
                status: 204,
                body: String::new(),
                headers: Vec::new(),
            },
        ),
        route(
            HttpPathSelector::SegmentPrefix {
                value: "/old".into(),
            },
            HttpRouteAction::Redirect {
                status: 301,
                location: HttpRedirectLocation::Literal {
                    value: "https://example.com/new".into(),
                },
                headers: Vec::new(),
            },
        ),
    ];
}

async fn daemon() -> Daemon {
    Daemon::start_configured(reserve_tcp_address(), routes).await
}

fn answer(daemon: &Daemon, words: &[&str]) -> Value {
    let output = daemon.run(&[&["--output", "json", "route", "test"], words].concat());
    assert!(output.status.success(), "{words:?}: {}", stderr(&output));
    serde_json::from_slice(&output.stdout).expect("JSON")
}

#[tokio::test]
async fn a_proxied_request_names_its_route_pool_and_servers() {
    let daemon = daemon().await;

    let json = answer(&daemon, &[&format!("https://{API_HOST}/v1/users?page=2")]);
    let route = &json["matches"][0];
    assert_eq!(route["listener"], "traffic");
    assert_eq!(route["action"], "proxy");
    assert_eq!(route["pool"]["name"], "farm");
    assert_eq!(route["pool"]["totalEndpoints"], 2);
    assert_eq!(json["host"], API_HOST);
    assert_eq!(json["path"], "/v1/users");

    let table = daemon.run(&["route", "test", API_HOST, "/v1/users", "--method", "post"]);
    assert!(table.status.success(), "{}", stderr(&table));
    let text = stdout(&table);
    assert!(
        text.contains("proxy") && text.contains("farm") && text.contains("2/2 available"),
        "{text}"
    );
}

#[tokio::test]
async fn the_answer_is_what_the_listener_serves() {
    let daemon = daemon().await;
    let serve = async |path: &str| {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {API_HOST}\r\nConnection: close\r\n\r\n");
        raw_http_request(daemon.traffic, request.as_bytes()).await
    };

    let health = answer(&daemon, &[API_HOST, "/health"]);
    assert_eq!(health["matches"][0]["action"], "fixed");
    assert_eq!(health["matches"][0]["status"], 204);
    assert_eq!(serve("/health").await.status, 204);

    let old = answer(&daemon, &["/old/page"]);
    assert_eq!(old["matches"][0]["action"], "redirect");
    assert_eq!(old["matches"][0]["status"], 301);
    assert_eq!(serve("/old/page").await.status, 301);

    let other = daemon.run(&["route", "test", "other.example.test", "/v1/users"]);
    assert_eq!(other.status.code(), Some(6), "{}", stderr(&other));
    assert!(
        stderr(&other).contains("route_not_found"),
        "{}",
        stderr(&other)
    );
    assert_eq!(
        raw_http_request(
            daemon.traffic,
            b"GET /v1/users HTTP/1.1\r\nHost: other.example.test\r\nConnection: close\r\n\r\n"
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn a_listener_filter_and_a_bad_question_are_reported() {
    let daemon = daemon().await;

    let named = answer(&daemon, &["/health", "--listener", "traffic"]);
    assert_eq!(named["matches"][0]["listener"], "traffic");
    let unknown = daemon.run(&["route", "test", "/health", "--listener", "nosuch"]);
    assert_eq!(unknown.status.code(), Some(6), "{}", stderr(&unknown));
    assert!(
        stderr(&unknown).contains("listener_not_found"),
        "{}",
        stderr(&unknown)
    );

    let ambiguous = daemon.run(&["route", "test", "example.com", "/a/../b"]);
    assert_eq!(ambiguous.status.code(), Some(8), "{}", stderr(&ambiguous));
    let no_scheme = daemon.run(&["route", "test", "example.com"]);
    assert_eq!(no_scheme.status.code(), Some(3), "{}", stderr(&no_scheme));
    let bad_method = daemon.run(&["route", "test", "/health", "--method", "not a method"]);
    assert_eq!(bad_method.status.code(), Some(2), "{}", stderr(&bad_method));
}
