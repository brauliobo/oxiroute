use std::{collections::HashMap, time::Duration};

use oxiroute_config::{HttpHostSelector, HttpPathSelector, HttpRoutePolicy};
use oxiroute_h1_head::RequestHead;

use super::*;
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

fn is_classic(early: &Early<'_>) -> bool {
    matches!(early, Early::Classic)
}

fn lane_route(early: &Early<'_>) -> Option<LaneRoute> {
    match early {
        Early::Lane(route, _) => Some(route.lane),
        Early::Unknown | Early::Classic => None,
    }
}

#[test]
fn a_request_for_a_classic_route_is_decided_before_any_scan() {
    let service = mixed();
    for target in ["/static", "/static/a.bin", "/static/a.bin?x=1", "/static?"] {
        let request = request(target);
        assert!(
            is_classic(&decide(&service, request.as_bytes())),
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
        lane_route(&decide(&service, proxied.as_bytes())),
        Some(LaneRoute::Proxy)
    );
    assert_eq!(
        lane_route(&decide(&service, direct.as_bytes())),
        Some(LaneRoute::Direct)
    );
}

#[test]
fn a_request_no_route_serves_is_the_classic_paths() {
    let service = service(&[spec("/static", LaneRoute::Classic)]);
    assert!(is_classic(&decide(&service, request("/other").as_bytes())));
}

#[test]
fn a_head_that_does_not_show_its_first_line_is_left_to_the_scan() {
    let service = mixed();
    for bytes in [
        &b"GET /static"[..],
        b"GET /static HTTP/1.1",
        b"GET /static HTTP/1.1\n\n",
        b"GET /static\r\n\r\n",
        b"GET http://example.test/static HTTP/1.1\r\n\r\n",
        b"OPTIONS * HTTP/1.1\r\n\r\n",
        b"GET /st\xffatic HTTP/1.1\r\n\r\n",
        b"G@T /static HTTP/1.1\r\n\r\n",
        b"",
    ] {
        assert!(
            matches!(decide(&service, bytes), Early::Unknown),
            "{bytes:?}"
        );
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
    let get_request = request("/api");
    let get = decide(&service, get_request.as_bytes());
    assert_eq!(lane_route(&get), Some(LaneRoute::Proxy));
    let post = b"POST /api HTTP/1.1\r\nHost: example.test\r\n\r\n";
    assert!(is_classic(&decide(&service, post)));
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
fn a_service_with_host_routes_decides_by_the_host_line() {
    let service = by_host();
    let on =
        |host: &str| format!("GET /a HTTP/1.1\r\nAccept: */*\r\nhOsT: {host}\r\nX-A: b\r\n\r\n");
    assert!(is_classic(&decide(
        &service,
        on("static.example.test").as_bytes()
    )));
    assert_eq!(
        lane_route(&decide(&service, on("www.example.test").as_bytes())),
        Some(LaneRoute::Proxy)
    );
}

#[test]
fn a_service_with_host_routes_waits_for_a_usable_host() {
    let service = by_host();
    for bytes in [
        &b"GET /a HTTP/1.1\r\n\r\n"[..],
        b"GET /a HTTP/1.1\r\nAccept: */*\r\n",
        b"GET /a HTTP/1.1\r\nHost: not a host\r\n\r\n",
        b"GET /a HTTP/1.1\r\nHost: \xff\r\n\r\n",
        b"GET /a HTTP/1.1\r\nHost\r\n\r\n",
    ] {
        assert!(
            matches!(decide(&service, bytes), Early::Unknown),
            "{bytes:?}"
        );
    }
}

#[test]
fn the_selected_route_is_reused_only_for_the_head_it_was_selected_for() {
    let service = by_host();
    let text = "GET /a?q=1 HTTP/1.1\r\nHost: www.example.test\r\n\r\n";
    let other = "GET /b?q=1 HTTP/1.1\r\nHost: www.example.test\r\n\r\n";
    let early = decide(&service, text.as_bytes());
    let Early::Lane(_, peek) = early else {
        panic!("a lane route");
    };
    let mut head = RequestHead::new();
    let own = head::scan(text.as_bytes(), &mut head).expect("a clean head");
    assert!(peek.is_of(text.as_bytes(), &own, true));
    let mut head = RequestHead::new();
    let foreign = head::scan(other.as_bytes(), &mut head).expect("a clean head");
    assert!(!peek.is_of(other.as_bytes(), &foreign, true));
}
