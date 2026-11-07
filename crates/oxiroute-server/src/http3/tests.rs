use oxiroute_config::{
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
};

use super::*;
use crate::{AdministrativeState, ListenerSnapshot};

#[test]
fn reports_reverse_capability_only_for_listening_ready_listeners() {
    let listeners = vec![ListenerSnapshot {
        administrative_state: AdministrativeState::Ready,
        name: "reverse".into(),
        protocol: "http3".into(),
        bind: "udp://127.0.0.1:9443".into(),
        max_connections: Some(8),
        state: ListenerRuntimeState::Listening,
        accepted_connections: 0,
        rejected_connections: 0,
        active_connections: 0,
        bytes_received: 0,
        bytes_sent: 0,
        http_operations: None,
        tcp_relays: None,
        proxy_protocol: None,
        cache: None,
    }];
    let value = capability_snapshot(&listeners, RuntimeMode::Direct);

    assert_eq!(value["http3"]["reverse"]["status"], "active");
    assert_eq!(value["http3"]["forward"]["status"], "unconfigured");
    assert_eq!(value["udp"]["status"], "unconfigured");
    assert_eq!(value["supervision"]["mode"], "direct");
    assert_eq!(value["supervision"]["descriptorAdoption"]["datagram"], true);
    assert_eq!(value["supervision"]["descriptorAdoption"]["quic"], true);
    assert_eq!(value["http3"]["reverse"]["supported"], true);
    assert_eq!(value["http3"]["reverse"]["goAway"], "graceful");
    assert_eq!(value["http3"]["reverse"]["fallback"], "none");
    assert_eq!(
        value["http3"]["reverse"]["unsupported"],
        serde_json::json!(["compression", "upgrades"])
    );
    assert_eq!(
        value["http3"]["reverse"]["limits"]["maxRequestBodyBytes"],
        H3_MAX_REQUEST_BODY_BYTES
    );
}

#[test]
fn exposes_the_bounded_h3_quic_and_message_contract() {
    let value = capability_snapshot(&[], RuntimeMode::Direct);
    let limits = &value["http3"]["reverse"]["limits"];

    assert_eq!(limits["maxHandshakesAndConnections"], H3_HANDSHAKE_LIMIT);
    assert_eq!(limits["maxBidirectionalStreams"], H3_BIDI_STREAM_LIMIT);
    assert_eq!(limits["maxUnidirectionalStreams"], H3_UNI_STREAM_LIMIT);
    assert_eq!(limits["maxFieldSectionBytes"], H3_MAX_FIELD_SECTION_BYTES);
    assert_eq!(limits["maxRequestBodyBytes"], H3_MAX_REQUEST_BODY_BYTES);
    assert_eq!(limits["maxResponseBodyBytes"], H3_MAX_RESPONSE_BODY_BYTES);
}

#[test]
fn reports_forward_h3_cache_as_supported() {
    let value = capability_snapshot(&[], RuntimeMode::Direct);

    assert_eq!(
        value["http3"]["forward"]["unsupported"],
        serde_json::json!(["compression", "upgrades"])
    );
}

#[tokio::test]
async fn h3_drain_waits_for_admitted_requests_to_finish() {
    let (request_cancel, cancelled) = watch::channel(false);
    let mut requests = tokio::task::JoinSet::new();
    requests.spawn(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
    });

    join_h3_requests(
        "test HTTP/3",
        &mut requests,
        &request_cancel,
        Instant::now() + Duration::from_secs(1),
    )
    .await;

    assert!(!*cancelled.borrow(), "completed request was cancelled");
}

#[tokio::test]
async fn h3_drain_cancels_admitted_requests_at_the_deadline() {
    let (request_cancel, cancelled) = watch::channel(false);
    let mut requests = tokio::task::JoinSet::new();
    requests.spawn(std::future::pending::<()>());

    join_h3_requests(
        "test HTTP/3",
        &mut requests,
        &request_cancel,
        Instant::now() + Duration::from_millis(10),
    )
    .await;

    assert!(*cancelled.borrow(), "timed-out request was not cancelled");
}

#[tokio::test]
async fn h3_accept_gate_close_is_acknowledged_for_drain() {
    let gate = pingora::apps::AcceptGate::closed();
    gate.enable();
    let mut participant = gate.register();
    let close = gate.close();
    let state = participant.changed().await.expect("accept gate change");

    assert!(!state.accepting);
    participant.acknowledge(state.epoch);
    assert!(close.wait(Duration::from_secs(1)));
}

#[test]
fn rejects_duplicate_or_invalid_content_lengths() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("3"));
    assert_eq!(request_content_length(&headers), Ok(Some(3)));

    headers.append(CONTENT_LENGTH, HeaderValue::from_static("4"));
    assert_eq!(request_content_length(&headers), Err(()));

    let mut invalid = HeaderMap::new();
    invalid.insert(CONTENT_LENGTH, HeaderValue::from_static("not-a-number"));
    assert_eq!(request_content_length(&invalid), Err(()));
}

#[test]
fn sanitizes_h3_response_framing_and_hop_by_hop_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, HeaderValue::from_static("x-hop, keep-alive"));
    headers.insert(
        HeaderName::from_static("x-hop"),
        HeaderValue::from_static("removed"),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("999"));
    headers.insert(TE, HeaderValue::from_static("trailers"));

    sanitize_h3_response_headers(&mut headers, StatusCode::OK, 3, false)
        .expect("safe H3 response headers");

    assert_eq!(headers[CONTENT_LENGTH], "3");
    assert!(!headers.contains_key(CONNECTION));
    assert!(!headers.contains_key("x-hop"));
    assert!(!headers.contains_key(TE));

    let mut head_headers = HeaderMap::new();
    head_headers.insert(CONTENT_LENGTH, HeaderValue::from_static("9"));
    sanitize_h3_response_headers(&mut head_headers, StatusCode::OK, 0, true)
        .expect("safe H3 HEAD response headers");
    assert_eq!(head_headers[CONTENT_LENGTH], "9");
}

#[test]
fn rejects_unrepresentable_h3_response_and_trailer_shapes() {
    let mut headers = HeaderMap::new();
    assert!(
        sanitize_h3_response_headers(&mut headers, StatusCode::SWITCHING_PROTOCOLS, 0, false,)
            .is_err()
    );

    let mut headers = HeaderMap::new();
    assert!(sanitize_h3_response_headers(&mut headers, StatusCode::NO_CONTENT, 1, false,).is_err());

    let mut headers = HeaderMap::new();
    assert!(
        sanitize_h3_response_headers(
            &mut headers,
            StatusCode::OK,
            H3_MAX_RESPONSE_BODY_BYTES + 1,
            false,
        )
        .is_err()
    );

    let mut trailers = HeaderMap::new();
    trailers.insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
    sanitize_h3_trailers(&mut trailers).expect("safe H3 trailers");
    assert!(!trailers.contains_key(CONTENT_LENGTH));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one request-value matrix keeps H3 policy behavior comparable"
)]
fn h3_request_values_join_fields_and_honor_source_exceptions() {
    let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "x-literal".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "literal".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-authority".into(),
                value: HttpRequestHeaderValue::IncomingAuthority,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-normalized".into(),
                value: HttpRequestHeaderValue::NormalizedHost,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-nginx".into(),
                value: HttpRequestHeaderValue::NginxHost {
                    fallback: "fallback.test".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-client".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-scheme".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-joined".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-source".into(),
                    max_bytes: 32,
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-selected".into(),
                value: HttpRequestHeaderValue::SelectedUpstreamHost,
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-remove".into(),
            },
        ],
        ..HttpProxyPolicy::default()
    });
    let authority = "Client.Example.:8080"
        .parse()
        .expect("mixed-case authority");
    let mut incoming = Request::builder()
        .uri("https://Client.Example.:8080/path?q=1")
        .header("x-forwarded-for", "trusted")
        .header("x-source", "one")
        .header("x-remove", "stale")
        .body(())
        .unwrap();
    incoming
        .headers_mut()
        .append("x-source", HeaderValue::from_static("two"));
    let mut headers = incoming.headers().clone();
    let decisions = h3_request_header_decisions(
        &incoming,
        &authority,
        &policy,
        Some("127.0.0.1:1234".parse().unwrap()),
    )
    .expect("H3 request decisions");
    apply_h3_header_mutations(
        &mut headers,
        &decisions,
        &HeaderValue::from_static("selected.example:9443"),
    )
    .expect("H3 request values");
    for (name, expected) in [
        ("x-literal", "literal"),
        ("x-authority", "Client.Example.:8080"),
        ("x-normalized", "client.example."),
        ("x-nginx", "client.example"),
        ("x-client", "127.0.0.1"),
        ("x-forwarded-for", "trusted, 127.0.0.1"),
        ("x-scheme", "https"),
        ("x-joined", "one, two"),
        ("x-selected", "selected.example:9443"),
    ] {
        assert_eq!(
            headers.get(name),
            Some(&HeaderValue::from_static(expected)),
            "{name}"
        );
    }
    assert!(!headers.contains_key("x-remove"));

    let excepted = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 128,
                except_source_cidrs: vec!["127.0.0.0/8".into()],
            },
        }],
        ..HttpProxyPolicy::default()
    });
    let mut excepted_headers = incoming.headers().clone();
    let decisions = h3_request_header_decisions(
        &incoming,
        &authority,
        &excepted,
        Some("127.0.0.1:1234".parse().unwrap()),
    )
    .expect("excepted H3 request decisions");
    apply_h3_header_mutations(
        &mut excepted_headers,
        &decisions,
        &HeaderValue::from_static("selected.example:9443"),
    )
    .expect("excepted H3 request values");
    assert_eq!(excepted_headers["x-forwarded-for"], "trusted");
}

#[test]
fn h3_join_bounds_include_inserted_separators() {
    let authority = "example.test".parse().unwrap();
    let mut incoming = Request::builder()
        .uri("https://example.test/")
        .header("x-source", "abc")
        .body(())
        .unwrap();
    incoming
        .headers_mut()
        .append("x-source", HeaderValue::from_static("de"));
    let policy = |max_bytes| {
        ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-joined".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-source".into(),
                    max_bytes,
                },
            }],
            ..HttpProxyPolicy::default()
        })
    };

    let exact = policy(7);
    let decisions = h3_request_header_decisions(&incoming, &authority, &exact, None).unwrap();
    let mut headers = incoming.headers().clone();
    apply_h3_header_mutations(
        &mut headers,
        &decisions,
        &HeaderValue::from_static("selected.test"),
    )
    .unwrap();
    assert_eq!(headers["x-joined"], "abc, de");

    assert_eq!(
        h3_request_header_decisions(&incoming, &authority, &policy(6), None),
        Err(RequestPolicyError::SourceTooLarge)
    );
}

#[test]
fn h3_missing_peer_xff_is_noop_without_an_existing_value() {
    let authority = "example.test".parse().unwrap();
    let selected = HeaderValue::from_static("selected.test");
    let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 128,
                except_source_cidrs: Vec::new(),
            },
        }],
        ..HttpProxyPolicy::default()
    });
    for (existing, expected) in [(None, None), (Some("trusted"), Some("trusted"))] {
        let mut incoming = Request::builder()
            .uri("https://example.test/")
            .body(())
            .unwrap();
        if let Some(existing) = existing {
            incoming
                .headers_mut()
                .insert("x-forwarded-for", HeaderValue::from_static(existing));
        }
        let mut headers = incoming.headers().clone();
        let decisions = h3_request_header_decisions(&incoming, &authority, &policy, None).unwrap();
        apply_h3_header_mutations(&mut headers, &decisions, &selected).unwrap();
        assert_eq!(
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok()),
            expected
        );
    }
}

#[test]
fn h3_redirect_templates_characterize_context_and_bounds() {
    let authority = "Client.Example.:8443".parse().unwrap();
    let uri = "/path/to?q=1".parse().unwrap();
    let template = |value: &str| HttpRedirectLocation::RequestTemplate {
        value: value.into(),
        nginx_host_fallback: Some("fallback.test".into()),
    };
    for (location, expected) in [
        (
            template("$scheme://$host$request_uri"),
            Some("https://client.example/path/to?q=1"),
        ),
        (template("$unknown"), None),
    ] {
        let location = RedirectLocationPlan::compile(&location);
        let actual = location
            .as_ref()
            .and_then(|location| h3_redirect_location(location, &authority, &uri));
        assert_eq!(
            actual.as_ref().and_then(|value| value.to_str().ok()),
            expected
        );
    }
    for (length, accepted) in [(8192, true), (8193, false)] {
        let location = HttpRedirectLocation::Literal {
            value: "x".repeat(length),
        };
        let location = RedirectLocationPlan::compile(&location);
        assert_eq!(
            location
                .as_ref()
                .and_then(|location| h3_redirect_location(location, &authority, &uri))
                .is_some(),
            accepted,
            "literal length {length}"
        );
    }
    let boundary_uri = format!("/{}", "x".repeat(8191)).parse().unwrap();
    assert!(
        RedirectLocationPlan::compile(&template("$request_uri"))
            .and_then(|location| h3_redirect_location(&location, &authority, &boundary_uri))
            .is_some()
    );
    let oversized_uri = format!("/{}", "x".repeat(8192)).parse().unwrap();
    assert!(
        RedirectLocationPlan::compile(&template("$request_uri"))
            .and_then(|location| h3_redirect_location(&location, &authority, &oversized_uri))
            .is_none()
    );
}
