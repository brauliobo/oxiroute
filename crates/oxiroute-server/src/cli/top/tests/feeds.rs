use serde_json::json;

use super::{
    fixture::{
        clients_document, conns_document, load, requests_document, rtmp_document, snapshot_document,
    },
    harness::{PLAIN, app, screen},
    server::{Fixture, Route},
};
use crate::cli::top::{
    model::{Filter, Notice},
    prefs::Prefs,
    source::{self, ClientsDoc, Feed, Wants},
    views::View,
};

fn stats() -> Route {
    Route::ok(
        "/api/v1/stats",
        &snapshot_document(12_000, 102_000, &load(3_000, 40, 60)),
    )
}

fn everything() -> Vec<Route> {
    vec![
        stats(),
        Route::ok("/api/v1/clients", &clients_document(12_000, 3)),
        Route::ok("/api/v1/connections", &conns_document(12_000, 3)),
        Route::ok("/api/v1/rtmp/stats", &rtmp_document(12_000, 3)),
        Route::ok(
            "/api/v1/requests",
            &requests_document(&[("req-old", 42_000)]),
        ),
    ]
}

fn wants(view: View, sort: &str, filter: &str) -> Wants {
    Wants::for_view(view, sort, &Filter::parse(filter), 200)
}

#[test]
fn a_view_without_extra_feeds_reads_only_the_stats() {
    let fixture = Fixture::serve(everything());
    let sample =
        source::sample(&fixture.client(), &wants(View::Listeners, "name", "")).expect("sample");
    assert_eq!(fixture.requested(), ["/api/v1/stats"]);
    assert!(matches!(sample.clients, Feed::Off) && matches!(sample.rtmp, Feed::Off));
    assert_eq!(sample.snapshot.sampled_at_unix_ms, 12_000);
}

#[test]
fn the_clients_query_carries_the_limit_the_sort_and_the_ip_prefix() {
    let fixture = Fixture::serve(everything());
    let wanted = wants(View::Clients, "in", "ip:192.0.2 edge");
    let sample = source::sample(&fixture.client(), &wanted).expect("sample");
    assert_eq!(
        fixture.requested(),
        [
            "/api/v1/stats",
            "/api/v1/clients?limit=200&sort=bytes&ip=192.0.2"
        ]
    );
    let Feed::Ready(clients) = sample.clients else {
        panic!("clients are ready: {:?}", sample.clients);
    };
    assert_eq!(clients.clients.len(), 2);
    assert_eq!(clients.totals.requests.0, 925 * 3);
    assert_eq!(clients.other.clients.0, 1);
    assert_eq!(clients.ipv6_prefix.map(|prefix| prefix.0), Some(64));
}

#[test]
fn the_sort_column_chooses_the_server_side_sort() {
    let sorted = |key| {
        let fixture = Fixture::serve(everything());
        source::sample(&fixture.client(), &wants(View::Clients, key, "")).expect("sample");
        fixture.requested()[1].clone()
    };
    assert_eq!(sorted("conn"), "/api/v1/clients?limit=200&sort=connections");
    assert_eq!(sorted("req"), "/api/v1/clients?limit=200&sort=requests");
    assert_eq!(sorted("age"), "/api/v1/clients?limit=200&sort=recent");
    assert_eq!(sorted("ip"), "/api/v1/clients?limit=200&sort=bytes");
}

#[test]
fn the_connections_query_carries_the_ip_and_the_listener() {
    let fixture = Fixture::serve(everything());
    let sample = source::sample(
        &fixture.client(),
        &wants(View::Conns, "age", "ip:192.0.2.10 listener:edge"),
    )
    .expect("sample");
    assert_eq!(
        fixture.requested()[1],
        "/api/v1/connections?limit=1000&ip=192.0.2.10&listener=edge"
    );
    let Feed::Ready(conns) = sample.conns else {
        panic!("connections are ready");
    };
    assert_eq!(conns.connections.len(), 2);
    let first = &conns.connections[0];
    assert_eq!(
        first
            .tls
            .as_ref()
            .and_then(|tls| tls.sni.as_ref())
            .map(|sni| sni.0.as_str()),
        Some("app.example.com")
    );
    assert!(conns.connections[1].tls.is_none() && conns.connections[1].current_request.is_none());
}

#[test]
fn requests_and_rtmp_are_read_when_their_view_is_open() {
    let fixture = Fixture::serve(everything());
    let sample =
        source::sample(&fixture.client(), &wants(View::Requests, "age", "")).expect("sample");
    assert!(matches!(sample.requests, Feed::Ready(_)));
    let sample = source::sample(&fixture.client(), &wants(View::Rtmp, "name", "")).expect("sample");
    let Feed::Ready(rtmp) = sample.rtmp else {
        panic!("rtmp is ready");
    };
    assert_eq!(rtmp.live[0].name, "show");
    assert_eq!(rtmp.clients.len(), 2);
}

#[test]
fn counters_may_arrive_as_numbers_or_as_decimal_strings() {
    let mut document = clients_document(12_000, 1);
    document["clients"][0]["bytesIn"] = json!("18446744073709551615");
    document["trackedClients"] = json!("3");
    let clients: ClientsDoc = serde_json::from_value(document).expect("clients");
    assert_eq!(clients.clients[0].bytes_in.0, u64::MAX);
    assert_eq!(clients.tracked_clients.0, 3);
}

fn sampled_with(route: Option<Route>) -> source::Sample {
    let mut routes = vec![stats()];
    routes.extend(route);
    let fixture = Fixture::serve(routes);
    source::sample(&fixture.client(), &wants(View::Clients, "in", "")).expect("sample")
}

fn notice(sample: &source::Sample) -> Notice {
    match &sample.clients {
        Feed::Unavailable(notice) => notice.clone(),
        other => panic!("the feed is unavailable: {other:?}"),
    }
}

#[test]
fn a_server_without_the_endpoint_is_reported_as_lacking_it() {
    let sample = sampled_with(None);
    assert_eq!(notice(&sample), Notice::Lacks("per-client tracking"));
    assert_eq!(
        notice(&sample).text(),
        "the server lacks per-client tracking: its management API answered 404"
    );
    assert!(!View::Clients.usable(&sample));
    assert!(View::Clients.badge(&sample).is_none());
}

#[test]
fn a_disabled_feature_is_reported_as_disabled_whatever_the_status() {
    let refused = Route::answering(
        "/api/v1/clients",
        503,
        &json!({ "error": { "code": "clients_disabled", "message": "per-client tracking is off" } }),
    );
    assert_eq!(
        notice(&sampled_with(Some(refused))),
        Notice::Disabled("per-client tracking")
    );
    let switched_off = Route::ok("/api/v1/clients", &json!({ "enabled": false }));
    assert_eq!(
        notice(&sampled_with(Some(switched_off))),
        Notice::Disabled("per-client tracking")
    );
    let flagged = Route::ok("/api/v1/clients", &json!({ "disabled": true }));
    assert_eq!(
        notice(&sampled_with(Some(flagged))),
        Notice::Disabled("per-client tracking")
    );
}

#[test]
fn any_other_failure_is_shown_with_its_reason() {
    let broken = Route::answering(
        "/api/v1/clients",
        500,
        &json!({ "error": { "code": "tracker_crashed" } }),
    );
    assert!(
        matches!(notice(&sampled_with(Some(broken))), Notice::Failed(reason) if reason.contains("tracker_crashed"))
    );
    let garbled = Route::ok("/api/v1/clients", &json!({ "clients": "many" }));
    assert!(matches!(
        notice(&sampled_with(Some(garbled))),
        Notice::Failed(_)
    ));
}

#[test]
fn a_missing_stats_endpoint_is_an_error_not_a_notice() {
    let fixture = Fixture::serve(Vec::new());
    assert!(source::sample(&fixture.client(), &Wants::default()).is_err());
}

#[test]
fn the_clients_view_says_why_it_is_empty_in_one_line() {
    let mut app = app(View::Clients, Prefs::default(), PLAIN);
    app.sample(Ok(sampled_with(None)));
    let screen = screen(&mut app, 120, 30);
    let notices = screen
        .lines()
        .filter(|line| line.contains("the server lacks per-client tracking"))
        .count();
    assert_eq!(notices, 1, "{screen}");
    assert!(screen.contains("Clients"));
}

#[test]
fn a_disabled_feed_degrades_only_its_own_view() {
    let refused = Route::answering(
        "/api/v1/connections",
        503,
        &json!({ "error": { "code": "connections_disabled" } }),
    );
    let fixture = Fixture::serve(vec![stats(), refused]);
    let sample = source::sample(&fixture.client(), &wants(View::Conns, "age", "")).expect("sample");
    let mut app = app(View::Conns, Prefs::default(), PLAIN);
    app.sample(Ok(sample));
    let screen = screen(&mut app, 100, 24);
    assert!(
        screen.contains("per-connection tracking is disabled on the server"),
        "{screen}"
    );
    app.act(crate::cli::top::app::Action::GoTo(View::Listeners));
    let listeners = super::harness::screen(&mut app, 100, 24);
    assert!(listeners.contains("edge"));
}

#[test]
fn the_dashboard_asks_the_server_only_for_the_open_view() {
    let mut app = app(View::Clients, Prefs::default(), PLAIN);
    let control = app.control();
    assert_eq!(control.wants.clients.expect("clients wanted").limit, 200);
    assert!(control.wants.conns.is_none() && !control.wants.requests && !control.wants.rtmp);
    app.act(crate::cli::top::app::Action::GoTo(View::Requests));
    assert!(app.control().wants.requests && app.control().wants.clients.is_none());
}
