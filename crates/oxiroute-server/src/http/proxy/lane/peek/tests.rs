use std::{collections::HashMap, time::Duration};

use oxiroute_config::{HttpHostSelector, HttpPathSelector, HttpRoutePolicy};
use oxiroute_h1_head::RequestHead;

use super::*;
use crate::http::proxy::lane::LaneRoute;
use crate::{
    RouteTable,
    http::action::{FixedResponsePlan, HttpActionPlan, RoutePolicyPlan},
    routing::Route,
};

struct Spec {
    host: Option<&'static str>,
    path: &'static str,
    method: Option<Method>,
    lane: LaneRoute,
}

const fn spec(path: &'static str, lane: LaneRoute) -> Spec {
    Spec {
        host: None,
        path,
        method: None,
        lane,
    }
}

fn service(specs: &[Spec]) -> HttpServicePlan {
    let mut routes = Vec::new();
    let mut plans = HashMap::new();
    for (index, spec) in specs.iter().enumerate() {
        let id = index.to_string();
        let host = spec.host.map(|value| HttpHostSelector::ExactAuthority {
            value: value.into(),
        });
        let path = HttpPathSelector::SegmentPrefix {
            value: spec.path.into(),
        };
        let methods = spec.method.clone().map(|method| vec![method]);
        routes.push(Route::new(host, path, methods, id.clone()).expect("route"));
        plans.insert(
            id.clone(),
            Arc::new(HttpRoutePlan {
                access: None,
                lane: spec.lane,
                edits: crate::http::proxy::lane::RouteEdits::default(),
                action: HttpActionPlan::Fixed(FixedResponsePlan::compile(200, "ok", &[])),
                policy: RoutePolicyPlan::compile(HttpRoutePolicy::default()),
                route_id: id.into(),
            }),
        );
    }
    HttpServicePlan::new(None, plans, Duration::from_secs(1), RouteTable::new(routes))
}

/// A static route in front of a proxied catch-all and a direct response.
fn mixed() -> HttpServicePlan {
    service(&[
        spec("/", LaneRoute::Proxy),
        spec("/static", LaneRoute::Classic),
        spec("/fixed", LaneRoute::Direct),
    ])
}

fn request(target: &str) -> String {
    format!("GET {target} HTTP/1.1\r\nHost: example.test\r\n\r\n")
}

/// The route the first lines selected: `None` when the classic path serves the request or the lines did not select.
fn lane_route(service: &HttpServicePlan, bytes: &[u8]) -> Option<LaneRoute> {
    decide(service, bytes)?.route.map(|(route, _)| route.lane)
}

#[test]
fn a_request_for_a_classic_route_is_decided_before_any_scan() {
    let service = mixed();
    for target in ["/static", "/static/a.bin", "/static/a.bin?x=1", "/static?"] {
        assert!(
            decide(&service, request(target).as_bytes()).is_none(),
            "{target}"
        );
    }
}

#[test]
fn a_request_for_a_lane_route_keeps_the_route_it_selected() {
    let service = mixed();
    let proxied = request("/api/items?id=7");
    let direct = request("/fixed");
    assert_eq!(
        lane_route(&service, proxied.as_bytes()),
        Some(LaneRoute::Proxy)
    );
    assert_eq!(
        lane_route(&service, direct.as_bytes()),
        Some(LaneRoute::Direct)
    );
}

#[test]
fn a_request_no_route_serves_is_the_classic_paths() {
    let service = service(&[spec("/static", LaneRoute::Classic)]);
    assert!(decide(&service, request("/other").as_bytes()).is_none());
}

#[test]
fn a_head_with_a_partial_or_refused_request_line_is_the_classic_paths() {
    let service = mixed();
    for bytes in [
        &b"GET /api"[..],
        b"GET /api HTTP/1.1",
        b"GET /api HTTP/1.1\n\n",
        b"GET /api\r\n\r\n",
        b"GET /api HTTP/1.0\r\n\r\n",
        b"GET http://example.test/api HTTP/1.1\r\n\r\n",
        b"OPTIONS * HTTP/1.1\r\n\r\n",
        b"GET /ap\xffi HTTP/1.1\r\n\r\n",
        b"G@T /api HTTP/1.1\r\n\r\n",
        b"",
    ] {
        assert!(decide(&service, bytes).is_none(), "{bytes:?}");
    }
}

#[test]
fn the_method_takes_part_in_the_decision() {
    let service = service(&[
        spec("/", LaneRoute::Proxy),
        Spec {
            method: Some(Method::POST),
            ..spec("/api", LaneRoute::Classic)
        },
    ]);
    assert_eq!(
        lane_route(&service, request("/api").as_bytes()),
        Some(LaneRoute::Proxy)
    );
    let post = b"POST /api HTTP/1.1\r\nHost: example.test\r\n\r\n";
    assert!(decide(&service, post).is_none());
}

fn by_host() -> HttpServicePlan {
    service(&[
        spec("/", LaneRoute::Proxy),
        Spec {
            host: Some("static.example.test"),
            ..spec("/", LaneRoute::Classic)
        },
    ])
}

#[test]
fn a_service_with_host_routes_decides_by_the_host_field() {
    let service = by_host();
    let on =
        |host: &str| format!("GET /a HTTP/1.1\r\nAccept: */*\r\nhOsT: {host}\r\nX-A: b\r\n\r\n");
    assert!(decide(&service, on("static.example.test").as_bytes()).is_none());
    assert_eq!(
        lane_route(&service, on("www.example.test").as_bytes()),
        Some(LaneRoute::Proxy)
    );
}

#[test]
fn a_service_with_host_routes_leaves_an_unusable_host_to_the_scan() {
    let service = by_host();
    for bytes in [
        &b"GET /a HTTP/1.1\r\n\r\n"[..],
        b"GET /a HTTP/1.1\r\nAccept: */*\r\n",
        b"GET /a HTTP/1.1\r\nHost: not a host\r\n\r\n",
        b"GET /a HTTP/1.1\r\nHost: \xff\r\n\r\n",
        b"GET /a HTTP/1.1\r\nHost\r\n\r\n",
    ] {
        let early = decide(&service, bytes).expect("the scan decides");
        assert!(early.route.is_none(), "{bytes:?}");
    }
}

fn scanned(bytes: &[u8], early: &Early<'_>, head: &mut RequestHead) -> head::Request {
    head::scan(bytes, early.line, head).expect("a clean head")
}

#[test]
fn the_selected_route_stands_for_the_scanned_head_of_the_same_host() {
    let service = by_host();
    let bytes = b"GET /a?q=1 HTTP/1.1\r\nHost: www.example.test\r\n\r\n";
    let early = decide(&service, bytes).expect("a lane request");
    let request = scanned(bytes, &early, &mut RequestHead::new());
    assert_eq!(request.path(bytes), "/a");
    assert!(early.route(bytes, &request).is_some());
}

#[test]
fn the_selected_route_does_not_stand_for_another_host() {
    let service = by_host();
    let bytes = b"GET /a HTTP/1.1\r\nHost: www.example.test\r\n\r\n";
    let early = decide(&service, bytes).expect("a lane request");
    let other = b"GET /a HTTP/1.1\r\nHost: static.example.test\r\n\r\n";
    let request = scanned(other, &early, &mut RequestHead::new());
    assert!(early.route(other, &request).is_none());
}

/// A service whose only route serves the files of `root` under `/static`.
fn static_service(root: &std::path::Path) -> (HttpServicePlan, Arc<HttpRoutePlan>) {
    let blueprint = crate::http::action::StaticFilesBlueprint::compile(
        "/static",
        &serde_json::from_value(
            serde_json::json!({ "type": "static_files", "root_directory": root }),
        )
        .expect("static files action"),
    )
    .expect("blueprint");
    let route = Arc::new(HttpRoutePlan {
        access: None,
        lane: LaneRoute::Static,
        edits: crate::http::proxy::lane::RouteEdits::default(),
        action: HttpActionPlan::Static(
            crate::http::action::StaticFilesPlan::acquire(&blueprint).expect("static files plan"),
        ),
        policy: RoutePolicyPlan::compile(HttpRoutePolicy::default()),
        route_id: "0".into(),
    });
    let path = HttpPathSelector::SegmentPrefix {
        value: "/static".into(),
    };
    let routes = vec![Route::new(None, path, None, "0".to_owned()).expect("route")];
    let plans = HashMap::from([("0".to_owned(), Arc::clone(&route))]);
    let service =
        HttpServicePlan::new(None, plans, Duration::from_secs(1), RouteTable::new(routes));
    (service, route)
}

fn cached(service: &HttpServicePlan, bytes: &[u8]) -> Option<bool> {
    decide(service, bytes).map(|early| early.route.is_some_and(|(_, file)| file.is_some()))
}

#[tokio::test]
async fn a_static_file_is_the_lanes_only_once_the_cache_holds_it() {
    let root = tempfile::tempdir().expect("static root");
    std::fs::create_dir(root.path().join("static")).expect("directory");
    std::fs::write(root.path().join("static/a.txt"), b"hello").expect("file");
    let (service, route) = static_service(root.path());
    let get = request("/static/a.txt");
    assert_eq!(
        cached(&service, get.as_bytes()),
        None,
        "a miss is decided before any scan"
    );

    let files = route.action.static_files().expect("a static route");
    files
        .serve("/static/a.txt")
        .await
        .expect("the classic path fills the cache");
    assert_eq!(cached(&service, get.as_bytes()), Some(true));
    let head = "HEAD /static/a.txt HTTP/1.1\r\nHost: example.test\r\n\r\n";
    assert_eq!(cached(&service, head.as_bytes()), Some(true));
    let query = request("/static/a.txt?v=2");
    assert_eq!(cached(&service, query.as_bytes()), Some(true));
    let other = request("/static/b.txt");
    assert_eq!(cached(&service, other.as_bytes()), None);
}

#[tokio::test]
async fn a_method_other_than_get_or_head_is_the_classic_paths_even_for_a_cached_file() {
    let root = tempfile::tempdir().expect("static root");
    std::fs::create_dir(root.path().join("static")).expect("directory");
    std::fs::write(root.path().join("static/a.txt"), b"hello").expect("file");
    let (service, route) = static_service(root.path());
    let files = route.action.static_files().expect("a static route");
    files.serve("/static/a.txt").await.expect("fill");
    let post = "POST /static/a.txt HTTP/1.1\r\nHost: example.test\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(cached(&service, post.as_bytes()), None);
}
