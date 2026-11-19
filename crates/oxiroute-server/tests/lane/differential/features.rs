//! What routes of a production shape add to the differential tests: header rules, forwarded headers, a service access
//! log, a TLS origin. Each is a configuration of both proxies; the corpus and the comparison are the ones of the
//! plain route.

use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, HttpProxyPolicy, HttpRequestHeaderMutation,
    HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRouteAction, HttpUpstreamHost,
    UpstreamTls,
};

use futures_util::future::join_all;

use super::{Fleet, OK, Pair, SHARDS, compare, corpus, play};
use crate::{
    fixture_support::fixture,
    h1_wire::{Origin, Wire},
};

fn request(name: &str, value: HttpRequestHeaderValue) -> HttpRequestHeaderMutation {
    HttpRequestHeaderMutation::Set {
        name: name.into(),
        value,
    }
}

fn response(name: &str, value: &str, always: bool, add: bool) -> HttpResponseHeaderMutation {
    let (name, value) = (name.into(), value.into());
    if add {
        HttpResponseHeaderMutation::Add {
            name,
            value,
            always,
        }
    } else {
        HttpResponseHeaderMutation::Set {
            name,
            value,
            always,
        }
    }
}

/// The proxy route of the service with `policy`.
fn proxy_policy(config: &mut ConfigDraft, policy: HttpProxyPolicy) {
    let route = config.http_services[0]
        .routes
        .last_mut()
        .expect("the proxy route");
    let HttpRouteAction::Proxy { policy: slot, .. } = &mut route.action else {
        panic!("the last route proxies");
    };
    **slot = policy;
}

/// What an nginx server block with `proxy_set_header` lowers to, and more: every kind of value the lane evaluates.
fn forwarding(upstream_host: HttpUpstreamHost) -> impl Fn(&mut ConfigDraft) {
    move |config| {
        proxy_policy(
            config,
            HttpProxyPolicy {
                upstream_host: upstream_host.clone(),
                request_headers: vec![
                    request(
                        "x-forwarded-for",
                        HttpRequestHeaderValue::AppendedXForwardedFor {
                            max_bytes: 256,
                            except_source_cidrs: Vec::new(),
                        },
                    ),
                    request(
                        "x-forwarded-proto",
                        HttpRequestHeaderValue::DownstreamScheme,
                    ),
                    request(
                        "x-forwarded-host",
                        HttpRequestHeaderValue::IncomingAuthority,
                    ),
                    request("x-real-ip", HttpRequestHeaderValue::ClientIp),
                    request("x-normalized", HttpRequestHeaderValue::NormalizedHost),
                    request(
                        "x-nginx-host",
                        HttpRequestHeaderValue::NginxHost {
                            fallback: "fallback.test".into(),
                        },
                    ),
                    request(
                        "x-incoming",
                        HttpRequestHeaderValue::IncomingHeader {
                            name: "x-a".into(),
                            max_bytes: 64,
                        },
                    ),
                    request(
                        "x-literal",
                        HttpRequestHeaderValue::Literal {
                            value: "fixed value".into(),
                        },
                    ),
                    request(
                        "x-selected-host",
                        HttpRequestHeaderValue::SelectedUpstreamHost,
                    ),
                    HttpRequestHeaderMutation::Remove {
                        name: "x-secret".into(),
                    },
                ],
                response_headers: vec![
                    response("x-served-by", "lane-test", true, false),
                    response("x-extra", "one", true, true),
                    response("x-extra", "two", true, true),
                    response("x-not-always", "n", false, true),
                    HttpResponseHeaderMutation::Remove {
                        name: "x-powered-by".into(),
                    },
                    HttpResponseHeaderMutation::Remove {
                        name: "x-origin".into(),
                    },
                    response("server", "lane", true, false),
                    response("cache-control", "no-store", true, false),
                ],
                ..HttpProxyPolicy::default()
            },
        );
    }
}

fn literal_host(config: &mut ConfigDraft) {
    forwarding(HttpUpstreamHost::Literal {
        value: "origin.internal".into(),
    })(config);
}

fn nginx_host(config: &mut ConfigDraft) {
    forwarding(HttpUpstreamHost::NginxHost {
        fallback: "fallback.test".into(),
    })(config);
}

fn preserved_host(config: &mut ConfigDraft) {
    forwarding(HttpUpstreamHost::PreserveIncoming)(config);
}

#[tokio::test]
async fn requests_with_forwarding_rules_are_identical_on_both_paths() {
    for edit in [preserved_host, literal_host, nginx_host] {
        let fleet = Fleet::of(|_| Pair::configured(OK, false, false, edit)).await;
        compare(&fleet, &corpus::forwarded("/r")).await;
        fleet.finish();
    }
}

#[tokio::test]
async fn requests_with_bodies_are_identical_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::start(OK)).await;
    compare(&fleet, &corpus::bodies("/r")).await;
    fleet.finish();
}

/// The origin has seen a part of a body that goes over the route's limit, and how much depends on the reads: the
/// client's answer and the end of the connection are what both paths share.
#[tokio::test]
async fn a_chunked_body_over_the_limit_is_refused_on_both_paths() {
    let pair = Pair::start(OK).await;
    let case = corpus::chunked_over_the_limit("/r");
    let script = case.script();
    let (classic, lane) = pair.play(&script).await;
    pair.finish();
    assert!(classic.response.starts_with("HTTP/1.1 413 "), "{classic:?}");
    assert_eq!(
        (&lane.response, lane.closed),
        (&classic.response, classic.closed)
    );
}

#[tokio::test]
async fn requests_with_bodies_and_rules_are_identical_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::configured(OK, false, false, literal_host)).await;
    compare(&fleet, &corpus::bodies("/r")).await;
    fleet.finish();
}

#[tokio::test]
async fn requests_with_bodies_to_a_tls_origin_are_identical_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::configured(OK, false, true, tls_origin)).await;
    compare(&fleet, &corpus::bodies("/r")).await;
    fleet.finish();
}

#[tokio::test]
async fn requests_with_forwarding_rules_pass_the_plain_corpus_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::configured(OK, false, false, literal_host)).await;
    compare(&fleet, &corpus::requests("/r")).await;
    fleet.finish();
}

#[tokio::test]
async fn responses_with_header_rules_are_identical_on_both_paths() {
    super::compare_replies(|reply, closes| Pair::configured(reply, closes, false, preserved_host))
        .await;
}

/// A pool whose servers speak HTTP/1.1 over TLS, verified against the fixture CA.
fn tls_origin(config: &mut ConfigDraft) {
    config.upstream_pools[0].tls = Some(UpstreamTls {
        server_name: Some("origin.example.test".into()),
        verify: oxiroute_config::UpstreamTlsVerify::Full,
        ca_certificate_path: Some(fixture("ca-a.pem")),
    });
}

#[tokio::test]
async fn requests_to_a_tls_origin_are_identical_on_both_paths() {
    let fleet = Fleet::of(|_| Pair::configured(OK, false, true, tls_origin)).await;
    compare(&fleet, &corpus::forwarded("/r")).await;
    compare(&fleet, &corpus::sequences()).await;
    fleet.finish();
}

#[tokio::test]
async fn replies_from_a_tls_origin_are_identical_on_both_paths() {
    super::compare_replies(|reply, closes| Pair::configured(reply, closes, true, tls_origin)).await;
}

/// An origin certificate the proxy does not trust is refused on both paths, without a request reaching the origin.
#[tokio::test]
async fn a_tls_origin_with_an_untrusted_certificate_is_a_bad_gateway_on_both_paths() {
    let pair = Pair::configured(OK, false, true, |config| {
        config.upstream_pools[0].tls = Some(UpstreamTls {
            server_name: Some("other.example.test".into()),
            verify: oxiroute_config::UpstreamTlsVerify::Full,
            ca_certificate_path: Some(fixture("ca-a.pem")),
        });
    })
    .await;
    let request = corpus::forwarded("/r").remove(0);
    let script = request.script();
    let (classic, lane) = pair.play(&script).await;
    pair.finish();
    assert!(
        !classic.response.starts_with("HTTP/1.1 200 "),
        "{classic:?}"
    );
    assert!(classic.origin.is_empty(), "{classic:?}");
    assert_eq!(classic, lane);
}

/// The lines of an access log of a proxy that stopped, with what differs by construction left out: times, durations
/// and ids.
pub(super) fn log_lines(path: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("the access log of a stopped proxy");
    text.lines().map(volatile_fields_masked).collect()
}

fn volatile_fields_masked(line: &str) -> String {
    let mut record: serde_json::Value = serde_json::from_str(line).expect("a JSON access line");
    let fields = record.as_object_mut().expect("a JSON object");
    for key in [
        "timestampUnixMs",
        "durationMs",
        "connectMs",
        "headerMs",
        "correlationId",
        "requestId",
    ] {
        if let Some(value) = fields.get_mut(key) {
            *value = serde_json::Value::Null;
        }
    }
    record.to_string()
}

/// Both proxies with an access log of their own in front of an origin that sends `reply`, and what each logged for the
/// `cases`: the responses are compared as everywhere, then the lines.
async fn assert_access_lines_identical(reply: &'static [u8], closes: bool, cases: &[corpus::Case]) {
    let directory = tempfile::tempdir().expect("log directory");
    let logs = |shard: usize| {
        ["classic", "lane"].map(|path| directory.path().join(format!("{path}-{shard}.log")))
    };
    let origin = || async move {
        if closes {
            Origin::start_closing(reply).await
        } else {
            Origin::start(reply).await
        }
    };
    let fleet = Fleet::of(|shard| {
        let [classic_path, lane_path] = logs(shard);
        async move {
            let classic = Wire::over(origin().await, move |config| {
                super::with_fixed_route(config);
                config.http_services[0].access_log =
                    Some(AccessLogPolicy::File { path: classic_path });
                config.http_fast_lane = Some(false);
            })
            .await;
            let lane = Wire::over(origin().await, move |config| {
                super::with_fixed_route(config);
                config.http_services[0].access_log =
                    Some(AccessLogPolicy::File { path: lane_path });
            })
            .await;
            Pair { classic, lane }.warmed().await
        }
    })
    .await;
    compare(&fleet, cases).await;
    fleet.finish_gracefully();
    for shard in 0..SHARDS {
        let [classic_log, lane_log] = logs(shard);
        let (classic_lines, lane_lines) = (log_lines(&classic_log), log_lines(&lane_log));
        assert!(!classic_lines.is_empty());
        assert_eq!(classic_lines, lane_lines, "shard {shard}");
    }
}

#[tokio::test]
async fn access_lines_are_identical_on_both_paths() {
    let cases: Vec<_> = [
        corpus::forwarded("/r"),
        corpus::requests("/fixed"),
        corpus::requests("/moved"),
        corpus::requests("/r").into_iter().take(40).collect(),
        corpus::bodies("/r"),
    ]
    .into_iter()
    .flatten()
    .collect();
    assert_access_lines_identical(OK, false, &cases).await;
}

/// Text a request chooses (the host, the request id, the idempotency key, the method) reaches the line redacted and
/// escaped the same way on both paths.
#[tokio::test]
async fn access_lines_of_requests_with_odd_text_are_identical_on_both_paths() {
    let cases: Vec<_> = ["/r", "/fixed", "/moved"]
        .into_iter()
        .flat_map(corpus::logged)
        .collect();
    assert_access_lines_identical(OK, false, &cases).await;
}

/// An origin that answers with something the proxy refuses, or closes the connection: the line carries the reason.
#[tokio::test]
async fn access_lines_of_failed_requests_are_identical_on_both_paths() {
    let cases = corpus::requests("/r")
        .into_iter()
        .take(12)
        .collect::<Vec<_>>();
    assert_access_lines_identical(b"garbage\r\n\r\n", true, &cases).await;
    assert_access_lines_identical(b"", true, &cases).await;
}
